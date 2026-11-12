//! R10 CLI 前端验收（TDD-2；keyword-search-design.md §8-2/§2.6/§2.7）：
//! - find 词项文法（决策①）：多词项与、否定词项、空查询/纯否定显式拒收
//!   （非零退出 + JSON 错误体携带 kind，R10.9）；
//! - find/list --filter（决策⑤）：可重复合取、第一个 = 分界、4 枚机判 kind
//!   两端同码；--type 与 --filter 组合（先限定后过滤）；
//! - 统一响应形状（两前端同形）：query 回显、filters_applied、warnings +
//!   warnings_truncated（决策⑦三要素）；
//! - 既有 find/list 行为保持：单串查询、分页续读、read_mode 标注。

use assert_cmd::Command;
use serde_json::{json, Value};

fn mem() -> Command {
    Command::cargo_bin("mem").expect("mem 二进制")
}

fn j(out: &std::process::Output) -> Value {
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("输出不是 JSON：{e}\n{}", String::from_utf8_lossy(&out.stdout)))
}

fn err_json(out: &std::process::Output) -> Value {
    serde_json::from_slice(&out.stderr)
        .unwrap_or_else(|e| panic!("错误输出不是 JSON：{e}\n{}", String::from_utf8_lossy(&out.stderr)))
}

/// x-filter 声明类型（CLI 侧：类型是纯数据——写两件套即得，零代码改动，红线1）。
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
    let out = mem().args(["--store", &s, "add", "doc", "--payload",
        r#"{"title":"Geometry","status":"Geometry","score":1}"#]).output().unwrap();
    assert!(out.status.success(), "add doc1：{}", String::from_utf8_lossy(&out.stderr));
    let out = mem().args(["--store", &s, "add", "doc", "--payload",
        r#"{"title":"plain","status":"Active","score":1.0}"#]).output().unwrap();
    assert!(out.status.success(), "add doc2：{}", String::from_utf8_lossy(&out.stderr));
    let out = mem().args(["--store", &s, "add", "memory", "--payload",
        r#"{"title":"other","status":"Geometry"}"#]).output().unwrap();
    assert!(out.status.success(), "add memory：{}", String::from_utf8_lossy(&out.stderr));
    s
}

// ---------- 决策①：词项文法与显式拒收 ----------

#[test]
fn cli_find_query_grammar_and_rejection() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    let s = setup(&dir);

    // 多词项与：整串单 argv（不依赖 shell 分词——两前端同一解析）。
    // 正文域查询记录：正文 "alpha beta here"（经 --body 文件）。
    let body_file = dir.join("body.txt");
    std::fs::write(&body_file, "alpha beta here").unwrap();
    let out = mem().args(["--store", &s, "add", "doc", "--payload",
        r#"{"title":"pad2","status":"y"}"#, "--body", body_file.to_str().unwrap()])
        .output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    // "alpha beta" = 词项与（正文含两词的 pad2 命中；Geometry 不含 alpha → 不命中）。
    let out = mem().args(["--store", &s, "find", "alpha beta"]).output().unwrap();
    let v = j(&out);
    let hits = v["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1, "多词项与（提案决策⑥，短语→词项与）：{v}");
    assert_eq!(v["query"]["positive"], json!(["alpha", "beta"]), "query 回显：{v}");
    assert_eq!(v["query"]["negative"], json!([]));

    // 否定词项：含 alpha 不含 beta → 零命中；"beta -here" → 零命中（pad2 被排除）。
    let out = mem().args(["--store", &s, "find", "beta -here"]).output().unwrap();
    let v = j(&out);
    assert_eq!(v["hits"].as_array().unwrap().len(), 0, "与位排除：{v}");
    assert_eq!(v["query"]["negative"], json!(["-here"]), "negative 回显保留原始词元");

    // 空查询 → 非零退出 + JSON 错误体携带 kind（R10.9：绝不表现为空命中或全库清单）。
    let out = mem().args(["--store", &s, "find", ""]).output().unwrap();
    assert!(!out.status.success(), "空查询必须非零退出");
    let e = err_json(&out);
    assert_eq!(e["kind"], json!("query_invalid"), "{e}");

    // 纯否定查询 → query_invalid（query 以 - 开头须以 -- 分界传参——CLI 惯例）。
    let out = mem().args(["--store", &s, "find", "--", "-beta"]).output().unwrap();
    assert!(!out.status.success(), "纯否定查询必须非零退出");
    let e = err_json(&out);
    assert_eq!(e["kind"], json!("query_invalid"), "{e}");
}

// ---------- 决策⑤：--filter 形状与 4 枚机判 kind ----------

#[test]
fn cli_find_filter_semantics_and_kinds() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    let s = setup(&dir);

    // 折叠等值：geometry 命中 Geometry（doc1）；memory 同值但类型未声明 → 静默排除。
    let out = mem().args(["--store", &s, "find", "geometry", "--filter", "status=geometry"])
        .output().unwrap();
    let v = j(&out);
    let hits = v["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1, "过滤=声明驱动的等值，未声明类型静默排除：{v}");
    assert_eq!(v["filters_applied"], json!({"status": "geometry"}), "filters_applied 回显：{v}");
    assert!(hits[0]["token"].as_str().unwrap().starts_with('v'), "命中带完整修订令牌");

    // 数值等值："1" 与 "1.0" 同值（决策④）。
    let out = mem().args(["--store", &s, "list", "--type", "doc", "--filter", "score=1"])
        .output().unwrap();
    let v = j(&out);
    assert_eq!(v["items"].as_array().unwrap().len(), 2, "score=1 命中 1 与 1.0：{v}");
    assert_eq!(v["filters_applied"], json!({"score": "1"}));

    // 词项 ∧ 过滤组合。
    let out = mem().args(["--store", &s, "find", "geometry", "--filter", "score=1"])
        .output().unwrap();
    let v = j(&out);
    assert_eq!(v["hits"].as_array().unwrap().len(), 1, "词项与过滤同时生效：{v}");

    // 4 枚机判 kind（两前端同码）：
    let cases: Vec<(Vec<&str>, &str)> = vec![
        (vec!["--filter", "nosuch=1"], "unknown_filter_field"),
        (vec!["--filter", "status=a", "--filter", "status=b"], "duplicate_filter_field"),
        (vec!["--filter", "status="], "invalid_filter_value"),
        (vec!["--filter", "status"], "invalid_filter_value"), // 无 = 分界
    ];
    for (args, kind) in cases {
        let mut full = vec!["--store".to_string(), s.clone(), "find".to_string(), "x".to_string()];
        full.extend(args.iter().map(|a| a.to_string()));
        let out = mem().args(&full).output().unwrap();
        assert!(!out.status.success(), "args={args:?} 必须非零退出");
        let e = err_json(&out);
        assert_eq!(e["kind"], json!(kind), "args={args:?}：{e}");
    }
}

// ---------- 决策⑦：坏记录告警三要素透出 ----------

#[test]
fn cli_find_list_bad_record_warnings() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    let s = setup(&dir);
    std::fs::write(dir.join("records/doc/bad-1.md"), b"corrupt").unwrap();

    // find：坏记录不在命中清单 ∧ 告警三要素（R10.3/验收 10 direct 侧）。
    let out = mem().args(["--store", &s, "find", "geometry"]).output().unwrap();
    let v = j(&out);
    assert!(out.status.success(), "坏记录不得使读命令失败：{}", String::from_utf8_lossy(&out.stderr));
    let hits = v["hits"].as_array().unwrap();
    assert!(hits.iter().all(|h| h["rel"].as_str() != Some("records/doc/bad-1.md")), "坏记录不进命中：{v}");
    let warnings = v["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 1, "{v}");
    let w = warnings[0].as_str().unwrap();
    assert!(w.starts_with("跳过坏条目 records/doc/bad-1.md："), "三要素·位置+原因：{w}");
    assert!(w.contains("修复入口：mem fix records/doc/bad-1.md"), "三要素·修复入口：{w}");
    assert_eq!(v["warnings_truncated"], json!(false));

    // list 同面（决策⑦：find/list 都透出）。
    let out = mem().args(["--store", &s, "list"]).output().unwrap();
    let v = j(&out);
    assert!(v["warnings"].as_array().unwrap().len() == 1, "list 告警透出：{v}");
    assert_eq!(v["warnings_truncated"], json!(false));
    assert!(v["read_mode"].is_object(), "list 响应携带 read_mode 标注：{v}");
}

// ---------- 既有语义保持：分页续读 + --type 限定 ----------

#[test]
fn cli_find_paging_and_type_narrow_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    let s = setup(&dir);
    for i in 0..3 {
        let out = mem().args(["--store", &s, "add", "doc", "--payload",
            &format!(r#"{{"title":"paging{i}","status":"p","score":{i}}}"#)])
            .output().unwrap();
        assert!(out.status.success());
    }

    // 分页续读（红线9）：命中序推进、不重复、穷尽。
    let mut seen = Vec::new();
    let mut off = 0usize;
    for _ in 0..10 {
        let out = mem().args(["--store", &s, "find", "paging", "--limit", "2",
            "--offset", &off.to_string()]).output().unwrap();
        let v = j(&out);
        for h in v["hits"].as_array().unwrap() {
            seen.push(h["id"].as_str().unwrap().to_string());
        }
        match v["resume"]["offset"].as_u64() {
            Some(n) => off = n as usize,
            None => break,
        }
    }
    assert_eq!(seen.len(), 3, "续读穷尽：{seen:?}");
    let unique: std::collections::HashSet<_> = seen.iter().collect();
    assert_eq!(unique.len(), 3, "续读不重复");

    // --type 收窄 = 子集表达（决策④）：score=1 在 doc 全集命中 2 条（1 与 1.0），
    // 收窄后 --filter score=2 只命中 1 条（收窄类型集内声明检查照常）。
    let out = mem().args(["--store", &s, "list", "--type", "doc", "--filter", "score=2"])
        .output().unwrap();
    let v = j(&out);
    assert_eq!(v["items"].as_array().unwrap().len(), 1, "type + filter 组合：{v}");

    // note 的续读指引（红线9）。
    let out = mem().args(["--store", &s, "find", "paging", "--limit", "2"]).output().unwrap();
    let v = j(&out);
    assert_eq!(v["truncated"], json!(true));
    assert!(v["note"].as_str().unwrap_or("").contains("--offset"), "截断必须给续读指引：{v}");
}

/// 数值过滤精确等值（实施评审 B1 回归）：>2^53 的整数×浮点不互认；同一实数的
/// 整数/浮点表示互认；direct 与 indexed 生产路径命中一致。
#[test]
#[cfg(windows)]
fn cli_number_filter_exact_beyond_f64_safe_range() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    let s = setup(&dir);
    let out = mem().args(["--store", &s, "add", "doc", "--payload",
        r#"{"title":"big-int","status":"x","score":9007199254740993}"#]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let out = mem().args(["--store", &s, "add", "doc", "--payload",
        r#"{"title":"big-float","status":"x","score":9007199254740992.0}"#]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let ids = |v: &Value| -> Vec<String> {
        v["items"].as_array().unwrap()
            .iter().map(|i| i["id"].as_str().unwrap().to_string()).collect()
    };
    let list = |s: &str, text: &str| {
        let out = mem().args(["--store", s, "list", "--type", "doc",
            "--filter", &format!("score={text}")]).output().unwrap();
        j(&out)
    };

    // direct：过滤 2^53.0（浮点）只命中浮点记录——不得误命中整数 2^53+1。
    let vf = list(&s, "9007199254740992.0");
    assert_eq!(ids(&vf).len(), 1, "direct 过滤 2^53.0：{vf}");
    let vi = list(&s, "9007199254740993");
    assert_eq!(ids(&vi).len(), 1, "direct 过滤 2^53+1：{vi}");
    assert_ne!(ids(&vf)[0], ids(&vi)[0], "两个不同数值不得互认");
    // 同一实数不同表示：整数文本 9007199254740992 ≍ 浮点 2^53.0 记录。
    let same = list(&s, "9007199254740992");
    assert_eq!(ids(&same), ids(&vf), "同一实数互认（决策④的精确承担）");

    // indexed（sidecar 物化后）：**显式确认 read_mode=indexed**（实施评审非阻断
    // 改进——不确认则 sidecar 证明失败时断言退化为 direct 空转），再断言与 direct
    // 命中一致。
    let mut serve = std::process::Command::new(env!("CARGO_BIN_EXE_mem"))
        .args(["serve", "--store", &s, "--idle-secs", "9"])
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
        .spawn().unwrap();
    assert!(wait_for(&dir), "sidecar 物化");
    assert!(
        wait_until_mode(&s, "indexed", 50, 100),
        "sidecar 收敛后 list 必须走 indexed（否则本测试的 indexed 断言空转）"
    );
    let vi2 = list(&s, "9007199254740993");
    let vf2 = list(&s, "9007199254740992.0");
    for v in [&vi2, &vf2] {
        assert!(
            v["read_mode"].as_object().map(|m| m.contains_key("indexed")).unwrap_or(false),
            "indexed 回归必须真走 indexed（评审非阻断项）：{v}"
        );
    }
    assert_eq!(ids(&vf2), ids(&vf), "indexed 与 direct 一致（B1 修复后）");
    assert_eq!(ids(&vi2), ids(&vi), "indexed 与 direct 一致（B1 修复后）");
    serve.kill().ok();
}

#[cfg(windows)]
fn wait_for(dir: &std::path::Path) -> bool {
    for _ in 0..50 {
        if dir.join("index/index.sqlite").is_file() {
            std::thread::sleep(std::time::Duration::from_millis(300));
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    false
}

/// 轮询直到读命令的 read_mode 达到期望（indexed/direct）。
#[cfg(windows)]
fn wait_until_mode(s: &str, mode: &str, tries: usize, ms: u64) -> bool {
    for _ in 0..tries {
        let v = {
            let out = mem().args(["--store", s, "list", "--type", "doc"]).output().unwrap();
            j(&out)
        };
        let actual = if v["read_mode"].as_object().map(|m| m.contains_key("indexed")).unwrap_or(false) {
            "indexed"
        } else {
            "direct"
        };
        if actual == mode {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
    false
}
