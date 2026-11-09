//! R10 MCP 前端验收（TDD-4；keyword-search-design.md §8-4/§2.6/§2.7）：
//! - 验收 1：MCP 全链路不降级——单 MCP 进程内 add 含词记录 → mem_find →
//!   mem_show 钻取 → 载荷值断言；
//! - mem_find 工具形状（决策⑥）：query 必填且须含肯定词项（纯否定 → JSON-RPC
//!   error，data.kind=query_invalid）；filters 参数（决策⑤：值一律 JSON 字符串，
//!   非字符串标量显式拒收 invalid_filter_value）；
//! - mem_list 增 filters 参数 + read_mode/warnings 统一形状；
//! - 决策④ 四面对账之 MCP 侧：`{"status":"1"}` 与 CLI `status=1` 同命中集。

use assert_cmd::Command;
use serde_json::{json, Value};
use std::io::Write;
use std::process::{Command as PCommand, Stdio};

fn mem() -> Command {
    Command::cargo_bin("mem").expect("mem 二进制")
}

fn j(out: &std::process::Output) -> Value {
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("输出不是 JSON：{e}\n{}", String::from_utf8_lossy(&out.stdout)))
}

/// 真实 stdio MCP 会话（r12 既有模式）：按顺序发请求，返回逐行解析的应答数组。
fn mcp_session(s: &str, requests: &[Value]) -> Vec<Value> {
    let mut child = PCommand::new(env!("CARGO_BIN_EXE_mem"))
        .arg("mcp")
        .env("MEM_STORE", s)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("mem mcp 进程");
    {
        let stdin = child.stdin.as_mut().unwrap();
        for req in requests {
            writeln!(stdin, "{req}").unwrap();
        }
    }
    let out = child.wait_with_output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("应答行非 JSON：{e}\n{l}")))
        .collect()
}

/// 解包 tools/call 应答 content[0].text（工具输出的 pretty JSON）。
fn tool_result(resp: &Value) -> Value {
    let text = resp["result"]["content"][0]["text"].as_str().unwrap_or_default();
    serde_json::from_str(text).unwrap_or_else(|e| panic!("工具输出非 JSON：{e}\n{text}"))
}

fn call(id: i64, name: &str, args: Value) -> Value {
    json!({
        "jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": {"name": name, "arguments": args}
    })
}

const DOC_SCHEMA: &str = r#"{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "title": "doc",
  "type": "object",
  "x-grade": "lenient",
  "properties": {
    "title": {"type": "string"},
    "status": {"type": "string", "x-filter": true},
    "score": {"type": "number", "x-filter": true}
  }
}"#;

fn setup(dir: &std::path::Path) -> String {
    let s = dir.to_str().unwrap().to_string();
    let out = mem().args(["--store", &s, "init", &s, "--principal", "codex_main"]).output().unwrap();
    assert!(out.status.success(), "init：{}", String::from_utf8_lossy(&out.stderr));
    std::fs::create_dir_all(dir.join("types/doc")).unwrap();
    std::fs::write(dir.join("types/doc/schema.json"), DOC_SCHEMA).unwrap();
    s
}

/// 验收 1：MCP 全链路不降级——add（经 CLI 造数据）→ mem_find 命中 → mem_show
/// 钻取载荷值断言；全程单 MCP 进程 + 同店。
#[test]
fn mcp_find_full_chain_no_degradation() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    let s = setup(&dir);
    let out = mem().args(["--store", &s, "add", "doc", "--payload",
        r#"{"title":"Geometry notebook","status":"Geometry","score":1}"#])
        .output().unwrap();
    assert!(out.status.success());
    let title = "Geometry notebook";

    let resps = mcp_session(&s, &[
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        call(2, "mem_find", json!({"query": title})),
        call(3, "mem_find", json!({"query": title, "filters": {"status": "geometry"}})),
    ]);
    // tools/list 动态可见 mem_find（工具 14→15）。
    let resps2 = mcp_session(&s, &[
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    ]);
    let tools = resps2[1]["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert_eq!(names.len(), 15, "工具数 14→15（决策⑥）：{names:?}");
    assert!(names.contains(&"mem_find"));

    // mem_find 命中 + 统一响应形状（两前端同形，§2.6）。
    let found = tool_result(&resps[1]);
    let hits = found["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1, "MCP find 命中：{found}");
    assert_eq!(hits[0]["type"], json!("doc"));
    assert!(hits[0]["token"].as_str().unwrap().starts_with('v'), "命中带完整令牌：{found}");
    assert_eq!(found["query"]["positive"], json!(["Geometry", "notebook"]), "query 回显（原始词项）");
    assert_eq!(found["query"]["negative"], json!([]));
    assert!(found["read_mode"].is_object(), "read_mode 标注：{found}");
    assert_eq!(found["warnings"], json!([]), "warnings 字段在场（决策⑦）");
    assert_eq!(found["warnings_truncated"], json!(false));
    assert_eq!(found["filters_applied"], json!({}));

    // 过滤 + 词项组合（filters 值 = JSON 字符串）。
    let filtered = tool_result(&resps[2]);
    assert_eq!(filtered["hits"].as_array().unwrap().len(), 1, "过滤组合命中：{filtered}");
    assert_eq!(filtered["filters_applied"], json!({"status": "geometry"}));

    // mem_show 钻取：载荷值断言（验收 1 闭环）。
    let hit_id = hits[0]["id"].as_str().unwrap().to_string();
    let resps3 = mcp_session(&s, &[
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        call(2, "mem_show", json!({"id": hit_id})),
    ]);
    let shown = tool_result(&resps3[1]);
    assert_eq!(shown["payload"]["title"], json!(title), "钻取载荷值：{shown}");
    assert_eq!(shown["payload"]["score"], json!(1));
}

/// mem_find 显式拒收（R10.9 + 决策⑤）：空查询/纯否定 → query_invalid；
/// filters 值非 JSON 字符串 → invalid_filter_value；未知字段 → unknown_filter_field；
/// 重复字段天然不可能（JSON 对象），单字段重复语义由 CLI 面承载。
#[test]
fn mcp_find_rejection_kinds_match_cli() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    let s = setup(&dir);
    let out = mem().args(["--store", &s, "add", "doc", "--payload",
        r#"{"title":"x","status":"1"}"#]).output().unwrap();
    assert!(out.status.success());

    let resps = mcp_session(&s, &[
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        call(2, "mem_find", json!({"query": ""})),
        call(3, "mem_find", json!({"query": "-only"})),
        call(4, "mem_find", json!({"query": "x", "filters": {"status": 1}})),
        call(5, "mem_find", json!({"query": "x", "filters": {"status": true}})),
        call(6, "mem_find", json!({"query": "x", "filters": {"nosuch": "1"}})),
        call(7, "mem_find", json!({"query": "x", "filters": {"score": "abc"}})),
        call(8, "mem_find", json!({"query": "x", "filters": {"status": "1"}})),
    ]);
    for (i, kind) in [(1usize, "query_invalid"), (2, "query_invalid"),
                      (3, "invalid_filter_value"), (4, "invalid_filter_value"),
                      (5, "unknown_filter_field"), (6, "invalid_filter_value")] {
        let resp = &resps[i];
        assert!(resp["error"].is_object(), "id={} 必须是 JSON-RPC error：{resp}", i + 1);
        assert_eq!(
            resp["error"]["data"]["kind"], json!(kind),
            "kind 两前端同码（§2.7）：{resp}"
        );
    }
    // 对照：{"status":"1"}（字符串）合法，与 CLI status=1 同一规范条件 → 命中。
    let ok = tool_result(&resps[7]);
    assert_eq!(ok["hits"].as_array().unwrap().len(), 1, "字符串值合法：{ok}");
    assert_eq!(ok["filters_applied"], json!({"status": "1"}));
}

/// mem_list 增 filters + 统一形状；纯过滤（无关键词）由 list 承载（决策⑥）；
/// read_mode 标注（MCP list 下沉后同获快路径标注面）。
#[test]
fn mcp_list_filters_and_unified_shape() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    let s = setup(&dir);
    let out = mem().args(["--store", &s, "add", "doc", "--payload",
        r#"{"title":"a","status":"1","score":1}"#]).output().unwrap();
    assert!(out.status.success());
    let out = mem().args(["--store", &s, "add", "doc", "--payload",
        r#"{"title":"b","status":"2","score":2}"#]).output().unwrap();
    assert!(out.status.success());
    std::fs::write(dir.join("records/doc/bad-1.md"), b"corrupt").unwrap();

    let resps = mcp_session(&s, &[
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        call(2, "mem_list", json!({"filters": {"status": "1"}})),
        call(3, "mem_list", json!({"type": "doc", "filters": {"score": "1"}})),
        call(4, "mem_list", json!({})),
    ]);
    let v2 = tool_result(&resps[1]);
    assert_eq!(v2["items"].as_array().unwrap().len(), 1, "纯过滤清单：{v2}");
    assert_eq!(v2["filters_applied"], json!({"status": "1"}));
    assert!(v2["read_mode"].is_object(), "{v2}");
    let v3 = tool_result(&resps[2]);
    assert_eq!(v3["items"].as_array().unwrap().len(), 1, "score=1 数值等值命中 doc a（score 缺失的记录不命中不计错）：{v3}");
    let v4 = tool_result(&resps[3]);
    assert!(v4["warnings"].as_array().unwrap().len() == 1, "list 告警透出：{v4}");
    assert_eq!(v4["warnings_truncated"], json!(false));
    assert!(v4["warnings"][0].as_str().unwrap().contains("修复入口：mem fix records/doc/bad-1.md"));

    // list filters 拒收同码（非字符串值）。
    let resps2 = mcp_session(&s, &[
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        call(2, "mem_list", json!({"filters": {"status": 1}})),
    ]);
    assert!(resps2[1]["error"].is_object());
    assert_eq!(resps2[1]["error"]["data"]["kind"], json!("invalid_filter_value"));
}

/// 验收 9 的检索切片（MCP 侧对账基准）：同库同查询同过滤，CLI find 与 mem_find
/// 命中集合逐项相等（read_mode 标注可能因路径不同而不同——不作对等断言对象，§2.1）。
#[test]
fn mcp_and_cli_find_hit_sets_equal() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    let s = setup(&dir);
    for payload in [
        r#"{"title":"Geometry","status":"1","score":1}"#,
        r#"{"title":"geometry homework","status":"active","score":1.0}"#,
        r#"{"title":"unrelated","status":"active"}"#,
    ] {
        let out = mem().args(["--store", &s, "add", "doc", "--payload", payload]).output().unwrap();
        assert!(out.status.success());
    }

    for (query, filters) in [
        ("geometry", json!({})),
        ("geometry", json!({"status": "1"})),
        ("geometry", json!({"score": "1"})),
        ("geometry -homework", json!({})),
    ] {
        let mut args = json!({"query": query});
        if !filters.as_object().unwrap().is_empty() {
            args["filters"] = filters.clone();
        }
        let resps = mcp_session(&s, &[
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
            call(2, "mem_find", args),
        ]);
        let mcp_v = tool_result(&resps[1]);
        let mut mcp_ids: Vec<String> = mcp_v["hits"].as_array().unwrap()
            .iter().map(|h| h["id"].as_str().unwrap().to_string()).collect();

        let mut cli_args = vec!["--store".to_string(), s.clone(), "find".to_string(), query.to_string()];
        for (k, v) in filters.as_object().unwrap() {
            let text = v.as_str().expect("filters 测试值均为字符串");
            cli_args.push("--filter".to_string());
            cli_args.push(format!("{k}={text}"));
        }
        let cli_v = j(&mem().args(&cli_args).output().unwrap());
        let mut cli_ids: Vec<String> = cli_v["hits"].as_array().unwrap()
            .iter().map(|h| h["id"].as_str().unwrap().to_string()).collect();

        mcp_ids.sort();
        cli_ids.sort();
        assert_eq!(mcp_ids, cli_ids, "CLI 与 MCP 命中集合逐项相等（验收 9）：query={query:?} filters={filters}");
    }
}

/// 数值过滤精确等值（实施评审 B1 回归，MCP 生产路径）：>2^53 的整数×浮点不互认。
#[test]
fn mcp_number_filter_exact_beyond_f64_safe_range() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    let s = setup(&dir);
    for payload in [
        r#"{"title":"big","status":"x","score":9007199254740993}"#,
        r#"{"title":"big","status":"x","score":9007199254740992.0}"#,
    ] {
        let out = mem().args(["--store", &s, "add", "doc", "--payload", payload]).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    }
    let resps = mcp_session(&s, &[
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        call(2, "mem_list", json!({"filters": {"score": "9007199254740992.0"}})),
        call(3, "mem_list", json!({"filters": {"score": "9007199254740993"}})),
        call(4, "mem_find", json!({"query": "big", "filters": {"score": "9007199254740992.0"}})),
    ]);
    let v_float = tool_result(&resps[1]);
    let v_int = tool_result(&resps[2]);
    let v_find = tool_result(&resps[3]);
    let float_hit = v_float["items"].as_array().unwrap().len();
    let int_hit = v_int["items"].as_array().unwrap().len();
    assert_eq!(float_hit, 1, "MCP list 过滤 2^53.0 恰 1 条（不得误命中整数 2^53+1）：{}", resps[1]);
    assert_eq!(int_hit, 1, "MCP list 过滤 2^53+1 恰 1 条：{}", resps[2]);
    let f1 = v_float["items"][0]["id"].as_str().unwrap().to_string();
    let f2 = v_int["items"][0]["id"].as_str().unwrap().to_string();
    assert_ne!(f1, f2, "两个不同数值不得互认");
    let find_hits = v_find["hits"].as_array().unwrap();
    assert_eq!(find_hits.len(), 1, "mem_find 过滤同规则：{}", resps[3]);
}
