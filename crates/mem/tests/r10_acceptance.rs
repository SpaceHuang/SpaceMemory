#![cfg(windows)]

//! R10 验收面补齐（TDD-5；keyword-search-design.md §4.1/§4.2——既有测试未覆盖的
//! indexed 侧与四面对账）：
//! - 验收 2/5/6/7：indexed 与 direct 逐 ID 等价（多词项与四类记录、与位排除）、
//!   两次检索序列一致、续读穷尽无重复；
//! - 验收 8/9：跨类型异形同名过滤（CLI `status=1` ≡ MCP `{"status":"1"}` 合并
//!   命中集）、`--type` 收窄子集、direct/indexed × CLI/MCP 四面对账；
//! - 验收 10：indexed 告警承接（bad_records 表——坏文件先于物化存在）、坏文件
//!   变化后告警新鲜（stat 失配 → direct）、fix 后恢复参与、201 条告警双路径
//!   同序同子集 + warnings_truncated。

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

fn wait_until(mut cond: impl FnMut() -> bool, tries: usize, ms: u64) -> bool {
    for _ in 0..tries {
        if cond() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
    cond()
}

/// 真实 stdio MCP 会话（r12 既有模式）。
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

fn mode_of(v: &Value) -> &'static str {
    if v["read_mode"].as_object().map(|m| m.contains_key("indexed")).unwrap_or(false) {
        "indexed"
    } else {
        "direct"
    }
}

const DOC_SCHEMA: &str = r#"{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "title": "doc",
  "type": "object",
  "x-grade": "lenient",
  "properties": {
    "title": {"type": "string"},
    "status": {"type": "string", "x-filter": true}
  }
}"#;

const ALT_SCHEMA: &str = r#"{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "title": "alt",
  "type": "object",
  "x-grade": "lenient",
  "properties": {
    "status": {"type": "number", "x-filter": true}
  }
}"#;

fn init(dir: &std::path::Path) -> String {
    let s = dir.to_str().unwrap().to_string();
    let out = mem().args(["--store", &s, "init", &s, "--principal", "codex_main"]).output().unwrap();
    assert!(out.status.success(), "init：{}", String::from_utf8_lossy(&out.stderr));
    s
}

fn add(s: &str, payload: &str) -> String {
    let out = mem().args(["--store", s, "add", "doc", "--payload", payload]).output().unwrap();
    assert!(out.status.success(), "add {payload}：{}", String::from_utf8_lossy(&out.stderr));
    j(&out)["id"].as_str().unwrap().to_string()
}

fn hit_ids(v: &Value, key: &str) -> Vec<String> {
    let mut ids: Vec<String> = v[key].as_array().unwrap()
        .iter().map(|h| h["id"].as_str().unwrap().to_string()).collect();
    ids.sort();
    ids
}

/// CLI find（带可选过滤）。
fn cli_find(s: &str, query: &str, filter: Option<(&str, &str)>) -> Value {
    let mut args = vec!["--store".to_string(), s.to_string(), "find".to_string(), query.to_string()];
    if let Some((k, v)) = filter {
        args.push("--filter".to_string());
        args.push(format!("{k}={v}"));
    }
    let out = mem().args(&args).output().unwrap();
    j(&out)
}

/// MCP mem_find（带可选过滤）。
fn mcp_find(s: &str, query: &str, filter: Option<(&str, &str)>) -> Value {
    let mut args = json!({"query": query});
    if let Some((k, v)) = filter {
        args["filters"] = json!({ k: v });
    }
    let resps = mcp_session(s, &[
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        call(2, "mem_find", args),
    ]);
    tool_result(&resps[1])
}

// ---------- 验收 2/5/6/7/8/9：两执行器 × 两前端对账 ----------

#[test]
#[cfg(windows)]
fn r10_executor_and_frontend_equivalence_matrix() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    let s = init(&dir);
    std::fs::create_dir_all(dir.join("types/doc")).unwrap();
    std::fs::write(dir.join("types/doc/schema.json"), DOC_SCHEMA).unwrap();
    std::fs::create_dir_all(dir.join("types/alt")).unwrap();
    std::fs::write(dir.join("types/alt/schema.json"), ALT_SCHEMA).unwrap();
    // 多词与四类记录（验收 6）：仅 A / 仅 B / AB / 无 + alt（异形同名 status=1）。
    let only_a = add(&s, r#"{"title":"alpha","status":"x"}"#);
    let only_b = add(&s, r#"{"title":"beta","status":"x"}"#);
    let both = add(&s, r#"{"title":"alpha beta","status":"1"}"#);
    let _neither = add(&s, r#"{"title":"gamma","status":"x"}"#);
    let out = mem().args(["--store", &s, "add", "alt", "--payload", r#"{"status":1}"#]).output().unwrap();
    assert!(out.status.success());
    let alt1 = j(&out)["id"].as_str().unwrap().to_string();

    let mut serve = std::process::Command::new(env!("CARGO_BIN_EXE_mem"))
        .args(["serve", "--store", &s, "--idle-secs", "9"])
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
        .spawn().unwrap();
    assert!(wait_until(|| dir.join("index/index.sqlite").is_file(), 50, 100));
    assert!(wait_until(|| mode_of(&cli_find(&s, "alpha", None)) == "indexed", 50, 100),
        "sidecar 收敛后 find 应走 indexed");

    // 查询矩阵：多词与 / 与位排除 / 单词项 / 异形过滤合并。
    // （"x" 词项命中三条 status="x" 的记录，被 status=1 过滤全部排除——词项 ∧
    // 过滤同时生效；alt 记录不含 "x" 词项，天然不在命中。）
    let cases: Vec<(&str, Option<(&str, &str)>, Vec<String>)> = vec![
        ("alpha beta", None, vec![both.clone()]),                       // 验收 6：仅 AB 命中
        ("alpha -beta", None, vec![only_a.clone()]),                    // 验收 7：含 A 不含 B
        ("alpha", None, vec![only_a.clone(), both.clone()]),
        ("beta -alpha", None, vec![only_b.clone()]),
        ("geometry", None, vec![]),
        ("x", Some(("status", "1")), vec![]),                           // 异形同名：全集空（doc "x" 不等值）
    ];

    // 空查询/纯否定双前端拒收（验收 7 后半；indexed 阶段同样拒收）。
    let out_e = mem().args(["--store", &s, "find", ""]).output().unwrap();
    assert!(!out_e.status.success());
    let e: Value = serde_json::from_slice(&out_e.stderr).unwrap();
    assert_eq!(e["kind"], json!("query_invalid"));
    let resps = mcp_session(&s, &[
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        call(2, "mem_find", json!({"query": "-x"})),
    ]);
    assert_eq!(resps[1]["error"]["data"]["kind"], json!("query_invalid"));

    // 四面对账（验收 2/5/8/9）：indexed × {CLI, MCP} 与 direct × {CLI, MCP} 逐 ID 相等。
    for (query, filter, expected) in &cases {
        if query.is_empty() {
            continue;
        }
        let cli_v = cli_find(&s, query, *filter);
        assert_eq!(mode_of(&cli_v), "indexed", "indexed 阶段：{cli_v}");
        let mcp_v = mcp_find(&s, query, *filter);
        assert_eq!(hit_ids(&cli_v, "hits"), hit_ids(&mcp_v, "hits"), "CLI≡MCP（indexed）：{query:?} {filter:?}");
        let mut got = hit_ids(&cli_v, "hits");
        got.sort();
        assert_eq!(got, *expected, "命中集：{query:?} {filter:?}\n{cli_v}");
    }

    // 验收 5：同库同查询连查两次序列相等（indexed；direct 阶段在 sidecar 退出后同型断言）。
    let seq1 = cli_find(&s, "alpha", None);
    let seq2 = cli_find(&s, "alpha", None);
    assert_eq!(seq1["hits"], seq2["hits"], "两次检索序列一致（验收 5）");

    // 验收 2/4：续读穷尽无重复（indexed）。
    let mut seen = Vec::new();
    let mut off = 0usize;
    for _ in 0..10 {
        let args = ["--store", &s, "find", "alpha", "--limit", "1", "--offset", &off.to_string()];
        let v = j(&mem().args(&args).output().unwrap());
        seen.extend(v["hits"].as_array().unwrap().iter()
            .map(|h| h["id"].as_str().unwrap().to_string()));
        match v["resume"]["offset"].as_u64() {
            Some(n) => off = n as usize,
            None => break,
        }
    }
    seen.sort();
    let mut all = vec![only_a.clone(), both.clone()];
    all.sort();
    assert_eq!(seen, all, "续读穷尽无重复（验收 2/4）");

    // direct 阶段：sidecar 退出 → 同一矩阵再对账（四面的另两 face）。
    serve.kill().ok();
    assert!(wait_until(|| mode_of(&cli_find(&s, "alpha", None)) == "direct", 50, 100),
        "sidecar 退出后回落直读");
    for (query, filter, expected) in &cases {
        if query.is_empty() {
            continue;
        }
        let cli_v = cli_find(&s, query, *filter);
        assert_eq!(mode_of(&cli_v), "direct", "direct 阶段：{cli_v}");
        let mcp_v = mcp_find(&s, query, *filter);
        assert_eq!(hit_ids(&cli_v, "hits"), hit_ids(&mcp_v, "hits"), "CLI≡MCP（direct）：{query:?} {filter:?}");
        let mut got = hit_ids(&cli_v, "hits");
        got.sort();
        assert_eq!(got, *expected, "direct 命中集与 indexed 一致：{query:?} {filter:?}");
    }
    let seq1d = cli_find(&s, "alpha", None);
    let seq2d = cli_find(&s, "alpha", None);
    assert_eq!(seq1d["hits"], seq2d["hits"], "两次检索序列一致（direct，验收 5）");

    // 子集表达（验收 8）：--type 收窄得单形态子集——status=1 全集 = {both, alt1}，
    // --type doc 子集 = {both}（alt 不在候选类型集，其记录不命中不报错）。
    let out = mem().args(["--store", &s, "list", "--type", "doc", "--filter", "status=1"]).output().unwrap();
    let v = j(&out);
    assert_eq!(hit_ids(&v, "items"), vec![both.clone()], "--type doc 收窄子集：{v}");
    let out = mem().args(["--store", &s, "list", "--filter", "status=1"]).output().unwrap();
    let v = j(&out);
    let mut got = hit_ids(&v, "items");
    got.sort();
    let mut all = vec![both.clone(), alt1.clone()];
    all.sort();
    assert_eq!(got, all, "合并命中集（A∪B）：{v}");
}

// ---------- 验收 10：indexed 告警承接与新鲜性 ----------

#[test]
#[cfg(windows)]
fn r10_acceptance_10_indexed_bad_record_warnings_fresh_and_fixable() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    let s = init(&dir);
    std::fs::create_dir_all(dir.join("types/doc")).unwrap();
    std::fs::write(dir.join("types/doc/schema.json"), DOC_SCHEMA).unwrap();
    let out = mem().args(["--store", &s, "add", "doc", "--payload",
        r#"{"title":"target"}"#]).output().unwrap();
    assert!(out.status.success());
    let target = j(&out)["id"].as_str().unwrap().to_string();
    // 坏文件**先于**物化存在（文件名以查询词开头构造——验收 10 的查询无关化口径：
    // 不在命中清单 ∧ 告警可见）。
    std::fs::write(dir.join("records/doc/target-broken.md"), b"corrupt").unwrap();

    let mut serve = std::process::Command::new(env!("CARGO_BIN_EXE_mem"))
        .args(["serve", "--store", &s, "--idle-secs", "30"])
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
        .spawn().unwrap();
    assert!(wait_until(|| dir.join("index/index.sqlite").is_file(), 50, 100));
    assert!(wait_until(|| mode_of(&cli_find(&s, "target", None)) == "indexed", 50, 100));

    // indexed 响应携带坏记录告警（来自 bad_records 表——同扫描同事务），
    // 坏记录不在命中清单、不崩。
    let v = cli_find(&s, "target", None);
    assert_eq!(mode_of(&v), "indexed", "{v}");
    let warnings = v["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 1, "indexed 告警承接（决策⑦）：{v}");
    let w = warnings[0].as_str().unwrap();
    assert!(w.starts_with("跳过坏条目 records/doc/target-broken.md："), "三要素·位置+原因：{w}");
    assert!(w.contains("修复入口：mem fix records/doc/target-broken.md"), "三要素·修复入口：{w}");
    let rels: Vec<&str> = v["hits"].as_array().unwrap()
        .iter().filter_map(|h| h["rel"].as_str()).collect();
    assert!(!rels.contains(&"records/doc/target-broken.md"), "坏记录不进命中：{v}");

    // 坏文件外部变化（追加字节）→ stat 失配 → 直读 → 告警如实反映（新鲜，B1）。
    std::fs::write(dir.join("records/doc/target-broken.md"), b"corrupt-more").unwrap();
    let v2 = cli_find(&s, "target", None);
    assert_eq!(v2["warnings"].as_array().unwrap().len(), 1, "坏文件变化后告警仍在（直读如实）：{v2}");

    // fix（修复 = 写入合法记录）→ 恢复参与检索（验收 10 收尾）。
    let fixed = format!(r#"{{"title":"target-fixed"}}"#);
    let out = mem().args(["--store", &s, "add", "doc", "--payload", &fixed]).output().unwrap();
    assert!(out.status.success());
    // 坏文件重写为合法记录（模拟 mem fix 的落盘结果——fix 命令本身已在 R6.3 覆盖）。
    let env_id = "01f9a000-0000-7000-8000-0000000000fx1";
    let env = mem_core::envelope::Envelope {
        id: env_id.into(), r#type: "doc".into(),
        created: "2026-11-10T00:00:00Z".into(), modified: "2026-11-10T00:00:00Z".into(),
        version: 1, principal: None, refs: vec![], aliases: vec![],
        evidence_cutoff_seq: None, commit: None, extra: Default::default(),
    };
    let bytes = mem_core::record::serialize_new(&env, &json!({"title":"target-fixed"}), b"");
    std::fs::write(dir.join("records/doc/target-broken.md"), bytes).unwrap();
    // 变更触发维护收敛 → 回到 indexed → 命中含修复记录、告警清空。
    assert!(wait_until(|| mode_of(&cli_find(&s, "target", None)) == "indexed", 80, 150),
        "fix 后维护收敛回 indexed");
    let v3 = cli_find(&s, "target", None);
    assert_eq!(v3["warnings"].as_array().unwrap().len(), 0, "修复后告警清空：{v3}");
    let ids = hit_ids(&v3, "hits");
    assert!(ids.contains(&target) && ids.contains(&env_id.to_string()), "恢复参与：{v3}");
    serve.kill().ok();
}

/// 201 条坏记录：告警上限 = list_limit_max（200），rel 升序截断，双路径同序同子集。
#[test]
#[cfg(windows)]
fn r10_acceptance_10_warning_cap_dual_path_same_subset() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    let s = init(&dir);
    std::fs::create_dir_all(dir.join("types/doc")).unwrap();
    std::fs::write(dir.join("types/doc/schema.json"), DOC_SCHEMA).unwrap();
    let out = mem().args(["--store", &s, "add", "doc", "--payload",
        r#"{"title":"cap-target"}"#]).output().unwrap();
    assert!(out.status.success());
    for i in 0..201 {
        std::fs::write(dir.join(format!("records/doc/bad-{i:03}.md")), b"corrupt").unwrap();
    }

    // direct（无 sidecar）：200 条 rel 升序 + truncated。
    let v = cli_find(&s, "cap", None);
    let warnings = v["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 200, "上限 200（list_limit_max）：{}", warnings.len());
    assert_eq!(v["warnings_truncated"], json!(true));
    assert!(warnings[0].as_str().unwrap().contains("records/doc/bad-000.md"));
    assert!(warnings[199].as_str().unwrap().contains("records/doc/bad-199.md"));
    assert!(v["note"].as_str().unwrap_or("").contains("mem verify"), "note 注明完整清单经 verify：{v}");

    // indexed（sidecar 物化）：同序同子集（排序先行保证，§2.5）。
    let mut serve = std::process::Command::new(env!("CARGO_BIN_EXE_mem"))
        .args(["serve", "--store", &s, "--idle-secs", "9"])
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
        .spawn().unwrap();
    assert!(wait_until(|| dir.join("index/index.sqlite").is_file(), 50, 100));
    assert!(wait_until(|| mode_of(&cli_find(&s, "cap", None)) == "indexed", 60, 100));
    let v2 = cli_find(&s, "cap", None);
    assert_eq!(mode_of(&v2), "indexed", "{v2}");
    assert_eq!(v2["warnings"], v["warnings"], "双路径同序同子集（201 条截断口径）");
    assert_eq!(v2["warnings_truncated"], json!(true));
    serve.kill().ok();
}

/// B2 生产路径复验（实施评审第三轮修复要求 3）：新文件落盘后的第一次读取不得
/// 因"扫描后新增文件被检查点提前确认"而漏——direct（stat 失配）必命中；收敛回
/// indexed 后（下一次物化将其入扫描）仍命中且 read_mode=indexed。CLI 与 MCP 各验。
#[test]
#[cfg(windows)]
fn r10_b2_new_file_first_read_never_lost_cli_and_mcp() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    let s = init(&dir);
    std::fs::create_dir_all(dir.join("types/doc")).unwrap();
    std::fs::write(dir.join("types/doc/schema.json"), DOC_SCHEMA).unwrap();
    let _a = add(&s, r#"{"title":"anchor"}"#);

    let mut serve = std::process::Command::new(env!("CARGO_BIN_EXE_mem"))
        .args(["serve", "--store", &s, "--idle-secs", "30"])
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
        .spawn().unwrap();
    assert!(wait_until(|| dir.join("index/index.sqlite").is_file(), 50, 100));
    assert!(wait_until(|| mode_of(&cli_find(&s, "anchor", None)) == "indexed", 50, 100));

    // 扫描后新落盘的合法记录 X（外部放置形态）。
    let env = mem_core::envelope::Envelope {
        id: "01f9b200-0000-7000-8000-0000000000b2x".into(),
        r#type: "doc".into(),
        created: "2026-11-10T00:00:00Z".into(),
        modified: "2026-11-10T00:00:00Z".into(),
        version: 1,
        principal: None, refs: vec![], aliases: vec![],
        evidence_cutoff_seq: None, commit: None, extra: Default::default(),
    };
    let bytes = mem_core::record::serialize_new(&env, &json!({"title":"b2newx marker"}), b"");
    std::fs::write(dir.join("records/doc/b2-new.md"), bytes).unwrap();

    // 第一次读取（CLI）：X 必须在命中清单（落盘后 stat 失配 → direct 直读真相层）。
    let v1 = cli_find(&s, "b2newx", None);
    assert!(hit_ids(&v1, "hits").contains(&"01f9b200-0000-7000-8000-0000000000b2x".to_string()),
        "新文件第一次读取不得漏（B2）：{v1}");
    // 第一次读取（MCP）：同面。
    let mcp_v1 = mcp_find(&s, "b2newx", None);
    assert!(hit_ids(&mcp_v1, "hits").contains(&"01f9b200-0000-7000-8000-0000000000b2x".to_string()),
        "MCP 新文件第一次读取不得漏（B2）：{mcp_v1}");

    // 维护收敛回 indexed 后仍命中，且此轮响应真走 indexed（显式模式断言）。
    assert!(wait_until(|| mode_of(&cli_find(&s, "b2newx", None)) == "indexed", 80, 150),
        "维护收敛回 indexed");
    let v2 = cli_find(&s, "b2newx", None);
    assert!(v2["read_mode"].as_object().map(|m| m.contains_key("indexed")).unwrap_or(false),
        "收敛后应真走 indexed：{v2}");
    assert!(hit_ids(&v2, "hits").contains(&"01f9b200-0000-7000-8000-0000000000b2x".to_string()),
        "indexed 命中含新文件：{v2}");
    serve.kill().ok();
}
