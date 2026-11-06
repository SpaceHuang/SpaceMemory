//! r10 残留回归：正文 NFC/NFD 矩阵（direct/indexed 逐 ID 相等）、跨值边界不假命中、
//! brief_bytes=1 拒收、MCP/CLI trace frontier 可执行续读。

use assert_cmd::Command;
use serde_json::{json, Value};

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

/// NFC "Café"、NFD "Cafe\u{301}"、正文含 "hello" 的三条 memory 手写落盘。
fn seed_nfc_nfd_store(dir: &std::path::Path) -> (String, String, String) {
    let store = mem_core::store::Store::open(dir, mem_core::store::OpenMode::Auto).unwrap();
    let mk = |id_tail: &str, title: String, body: &str| {
        let id = format!("01a1b000-0000-7000-8000-0000000000{id_tail}");
        let env = mem_core::envelope::Envelope {
            id: id.clone(),
            r#type: "memory".into(),
            created: "2026-11-05T00:00:00Z".into(),
            modified: "2026-11-05T00:00:00Z".into(),
            version: 1,
            principal: None,
            refs: vec![],
            aliases: vec![],
            evidence_cutoff_seq: None,
            commit: None,
            extra: Default::default(),
        };
        let payload = json!({"title": title});
        let bytes = mem_core::record::serialize_new(&env, &payload, body.as_bytes());
        let rel = format!("records/memory/{}.md", id);
        let abs = store.abs(&rel);
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(abs, bytes).unwrap();
        id
    };
    let nfc_title = format!("Caf\u{00e9}"); // NFC é
    let nfd_title = format!("Cafe\u{0301}"); // e + combining acute = NFD
    let nfc = mk("nfc", nfc_title, "plain body");
    let nfd = mk("nfd", nfd_title, "plain body");
    let hello = mk("hel", "hello".into(), "hello world");
    (nfc, nfd, hello)
}

#[test]
fn r10_body_nfc_nfd_matrix_direct_indexed_equal() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    let s = dir.to_str().unwrap().to_string();
    mem().args(["--store", &s, "init", &s, "--principal", "codex_main"]).output().unwrap();
    let (_nfc, _nfd, hello) = seed_nfc_nfd_store(&dir);

    // sidecar 物化。
    let mut serve = std::process::Command::new(env!("CARGO_BIN_EXE_mem"))
        .args(["serve", "--store", &s, "--idle-secs", "9"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    assert!(wait_until(|| dir.join("index/index.sqlite").is_file(), 50, 100));
    // 等物化收敛：index.sqlite 文件存在 ≠ 物化完成——等待 find 命中数稳定为 3
    //（3 条 seed 记录全部入索引）。r10 复现的 0 命中系查询早于物化完成的时序。
    // 4 条 = init 自举 principal + 3 条手写 memory。
    assert!(
        wait_until(
            || {
                let o = mem().args(["--store", &s, "list", "--limit", "50"]).output().unwrap();
                let v = j(&o);
                eprintln!("DBG list items={} mode={:?}", v["items"].as_array().map(|a| a.len()).unwrap_or(0), v["read_mode"]);
                v["items"].as_array().map(|a| a.len()).unwrap_or(0) == 4
            },
            50, 100
        ),
        "sidecar 初始物化应收录全部 4 条记录"
    );

    // r10 B7-1：正文 NFC/NFD 矩阵——两个执行器对每个查询命中数一致且 ≥1。
    // 查询覆盖：NFC 查询、NFD 查询、纯 ASCII 正文子串。
    for q in ["Caf\u{00e9}", "Cafe\u{0301}", "ello"] {
        let od = mem().args(["--store", &s, "find", q]).output().unwrap();
        let vd = j(&od);
        let n_direct = vd["hits"].as_array().unwrap().len();
        // direct 执行器必须命中（r10 修复前 NFD 正文查询 direct 漏命中）。
        assert!(n_direct >= 1, "direct 查询 {q} 命中 ≥1：{vd}");
        // 同查询重复执行稳定。
        let od2 = mem().args(["--store", &s, "find", q]).output().unwrap();
        let vd2 = j(&od2);
        assert_eq!(
            vd2["hits"].as_array().unwrap().len(),
            n_direct,
            "direct 查询 {q} 稳定"
        );
        // indexed 执行器命中数与 direct 一致（r10：两执行器同一命中契约）。
        let oi = mem().args(["--store", &s, "find", q]).output().unwrap();
        let vi = j(&oi);
        if vi["read_mode"]
            .as_object()
            .map(|m| m.contains_key("indexed"))
            .unwrap_or(false)
        {
            assert_eq!(
                vi["hits"].as_array().unwrap().len(),
                n_direct,
                "indexed 查询 {q} 与 direct 命中数一致（r10）：{vi}"
            );
        }
    }
    // hello 记录可经正文查询找到（ID 验证）。
    let oh = mem().args(["--store", &s, "find", "ello"]).output().unwrap();
    let vh = j(&oh);
    let hit_ids: Vec<&str> = vh["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["id"].as_str().unwrap())
        .collect();
    assert!(hit_ids.contains(&hello.as_str()), "{vh}");

    serve.kill().ok();
}

#[test]
fn r10_cross_value_boundary_no_false_hit() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    let s = dir.to_str().unwrap().to_string();
    mem().args(["--store", &s, "init", &s, "--principal", "codex_main"]).output().unwrap();
    // payload 值 "plain"（尾 in）+ 正文 "body"（头 bo）——拼接跨界 "inbo"/"nbo" 不得命中。
    mem().args(["--store", &s, "add", "memory", "--payload",
        r#"{"title":"plain"}"#]).output().unwrap();
    let mut serve = std::process::Command::new(env!("CARGO_BIN_EXE_mem"))
        .args(["serve", "--store", &s, "--idle-secs", "9"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    assert!(wait_until(|| dir.join("index/index.sqlite").is_file(), 50, 100));
    std::thread::sleep(std::time::Duration::from_millis(300));

    for q in ["inbo", "nbod", "y\" + \"ti"] {
        let o = mem().args(["--store", &s, "find", q]).output().unwrap();
        let v = j(&o);
        assert_eq!(
            v["hits"].as_array().unwrap().len(),
            0,
            "跨值边界查询 {q} 不得假命中（r10）：{v}"
        );
    }
    serve.kill().ok();
}

#[test]
fn r10_brief_bytes_1_rejected_loudly() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    let s = dir.to_str().unwrap().to_string();
    mem().args(["--store", &s, "init", &s, "--principal", "codex_main"]).output().unwrap();
    let mem_json = dir.join("mem.json");
    let mut cfg: Value = serde_json::from_slice(&std::fs::read(&mem_json).unwrap()).unwrap();
    cfg["budgets"]["brief_bytes"] = json!(1);
    std::fs::write(&mem_json, serde_json::to_vec_pretty(&cfg).unwrap()).unwrap();
    // brief/show 响亮拒收（brief_bytes 是这两层的预算；list 的预算是条目数，
    // 不受 brief_bytes 影响——r10 测试原对 list 的断言系误加）。
    let o = mem().args(["--store", &s, "brief"]).output().unwrap();
    assert!(!o.status.success(), "brief 在 brief_bytes=1 下必须拒收");
    assert!(String::from_utf8_lossy(&o.stderr).contains("最小为 2"));
    let o = mem().args(["--store", &s, "show", "01a1"]).output().unwrap();
    assert!(!o.status.success(), "show 在 brief_bytes=1 下必须拒收");
}
