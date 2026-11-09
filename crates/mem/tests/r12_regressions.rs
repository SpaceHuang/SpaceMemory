//! r12 回归：B7-1 字面反斜杠查询 direct vs indexed 逐 ID 相等（折叠列 LF 分隔符
//! 不得成为假命中源）；B7-2 MCP mem_trace 节点 resume 重放可执行（不重复、
//! 到达后续节点）；B7-3 MCP mem_show/mem_brief 字节预算与续读（R4.2/红线9
//! 不因前端豁免）。
//!
//! R10 实施迁移注记（keyword-search-design.md §7）：查询文法已按提案决策⑥ 变更为
//! "空白 = 词项分隔"——LF/U+3000 等空白不再属于词项字符。原 B7-1 的字面 LF 查询
//! 用例（单 LF、"a" 与 "b" 以 LF 连接、"o" 与 "tw" 以 LF 连接）按新文法迁移为
//! 多词项与查询 + 空查询/纯否定拒收断言（R10.9）；"LF 分隔符不假命中"的索引侧
//! 保障由新文法结构性成立（词项永不含空白 → 折叠列 LF 分隔符不可能被词项跨越），
//! 等价断言面保留。

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

/// 手写落盘一条 memory（固定 id，绕过 CLI 以精确控制别名/LF/反斜杠内容）。
fn seed_memory(dir: &std::path::Path, id_tail: &str, aliases: Vec<&str>, title: Value, body: &str) -> String {
    let store = mem_core::store::Store::open(dir, mem_core::store::OpenMode::Auto).unwrap();
    let id = format!("01a2b400-0000-7000-8000-0000000000{id_tail}");
    let env = mem_core::envelope::Envelope {
        id: id.clone(),
        r#type: "memory".into(),
        created: "2026-11-06T00:00:00Z".into(),
        modified: "2026-11-06T00:00:00Z".into(),
        version: 1,
        principal: None,
        refs: vec![],
        aliases: aliases.iter().map(|a| a.to_string()).collect(),
        evidence_cutoff_seq: None,
        commit: None,
        extra: Default::default(),
    };
    let bytes = mem_core::record::serialize_new(&env, &title, body.as_bytes());
    let rel = format!("records/memory/{id}.md");
    let abs = store.abs(&rel);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(abs, bytes).unwrap();
    id
}

#[test]
fn r12_literal_lf_and_backslash_direct_indexed_equal() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    let s = dir.to_str().unwrap().to_string();
    mem().args(["--store", &s, "init", &s, "--principal", "codex_main"]).output().unwrap();

    // 四条内容可控的记录：
    // R1 多值 payload（"alpha"/"one"/"two"）+ 别名 "beta"——拼接列内部有 LF 分隔符；
    // R2 仅正文 "gamma"；
    // R3 payload 值内含**真实换行** "line1␊line2"（LF 字面字节）；
    // R4 payload 值含**字面反斜杠+n**（"a\nb" 的字面形式，非换行）。
    let r1 = seed_memory(&dir, "00r1", vec!["beta"], json!({"title": "alpha", "tags": ["one", "two"]}), "");
    let r2 = seed_memory(&dir, "00r2", vec![], json!({"title": "r-two"}), "gamma");
    let r3 = seed_memory(&dir, "00r3", vec![], json!({"title": "line1\nline2"}), "");
    let r4 = seed_memory(&dir, "00r4", vec![], json!({"title": "a\\nb"}), "");
    // 移除自举 principal（host 取机器名——环境相关命中会污染精确集合断言）。
    for e in std::fs::read_dir(dir.join("records/principal")).unwrap() {
        let e = e.unwrap();
        if e.path().extension().map(|x| x == "md").unwrap_or(false) {
            std::fs::remove_file(e.path()).unwrap();
        }
    }

    // R10 文法（决策①）下的查询→期望集（词项与）：
    // - 字面反斜杠单词项：R4；
    // - 值内真实换行的两个词项（原字面 LF 查询迁移）：R3（同值含 line1 与 line2）；
    // - "o tw"（原跨分隔符两端子串迁移）：R1（"one" 含 o、"two" 含 tw——AND 跨值
    //   成立）与 R2（"r-two" 同值含 o 与 tw）；
    // - "a b"（原字面 "a+LF+b" 查询迁移）：R1 与 R4（各自含 a 与 b）；
    // - 常规守卫：alpha → R1；gamma → R2。
    let queries: Vec<(&str, Vec<String>)> = vec![
        ("\\", vec![r4.clone()]),
        ("line1 line2", vec![r3.clone()]),
        ("o tw", vec![r1.clone(), r2.clone()]),
        ("a b", vec![r1.clone(), r4.clone()]),
        ("alpha", vec![r1.clone()]),
        ("gamma", vec![r2.clone()]),
    ];
    // 空查询 / 纯否定（R10.9）：非零退出 + kind=query_invalid——绝不表现为空命中
    // 清单或全库清单。
    for bad_q in ["", "  \n ", "--x"] {
        let o = mem().args(["--store", &s, "find", "--", bad_q]).output().unwrap();
        assert!(!o.status.success(), "查询 {bad_q:?} 必须非零退出");
        let err: Value = serde_json::from_slice(&o.stderr).unwrap();
        assert_eq!(err["kind"], json!("query_invalid"), "{err}");
    }

    let mut direct_hits: Vec<(String, Vec<String>)> = Vec::new();
    for (q, expected) in &queries {
        let o = mem().args(["--store", &s, "find", q]).output().unwrap();
        let v = j(&o);
        assert!(
            v["read_mode"].as_object().map(|m| m.contains_key("direct")).unwrap_or(false),
            "无索引阶段必须直读：q={q:?} {v}"
        );
        let ids: Vec<String> = v["hits"].as_array().unwrap()
            .iter().map(|h| h["id"].as_str().unwrap().to_string()).collect();
        let mut got = ids.clone();
        got.sort();
        let mut exp = expected.clone();
        exp.sort();
        assert_eq!(got, exp, "direct 查询 {q:?} 命中面不符期望\n{v}");
        direct_hits.push((q.to_string(), ids));
    }

    // sidecar 物化 → indexed 模式。
    let mut serve = std::process::Command::new(env!("CARGO_BIN_EXE_mem"))
        .args(["serve", "--store", &s, "--idle-secs", "9"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    assert!(wait_until(|| dir.join("index/index.sqlite").is_file(), 50, 100));
    assert!(
        wait_until(
            || {
                let o = mem().args(["--store", &s, "find", "alpha"]).output().unwrap();
                let v = j(&o);
                v["read_mode"].as_object().map(|m| m.contains_key("indexed")).unwrap_or(false)
            },
            50, 100
        ),
        "sidecar 收敛后 find 应走 indexed 模式"
    );

    // 逐查询：indexed 命中面 == direct 命中面 == 期望集合（逐 ID 相等，B7-1 关闭条件；
    // 词项永不含空白 → 折叠列 LF 分隔符不可能被词项跨越——假命中结构性消除）。
    for (i, (q, _)) in queries.iter().enumerate() {
        let o = mem().args(["--store", &s, "find", q]).output().unwrap();
        let v = j(&o);
        assert!(
            v["read_mode"].as_object().map(|m| m.contains_key("indexed")).unwrap_or(false),
            "查询 {q:?} 应走 indexed：{v}"
        );
        let mut idx_ids: Vec<String> = v["hits"].as_array().unwrap()
            .iter().map(|h| h["id"].as_str().unwrap().to_string()).collect();
        idx_ids.sort();
        let (dq, dref) = &direct_hits[i];
        assert_eq!(dq, q);
        let mut dir_ids = dref.clone();
        dir_ids.sort();
        assert_eq!(idx_ids, dir_ids, "查询 {q:?} indexed 与 direct 逐 ID 不等（B7-1）\n{v}");
    }
    serve.kill().ok();
}

// ---------- B7-2：MCP mem_trace 节点 resume 重放 ----------

/// 真实 stdio MCP 会话：按顺序发请求，返回逐行解析的应答数组。
fn mcp_session(s: &str, requests: &[Value]) -> Vec<Value> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = Command::new(env!("CARGO_BIN_EXE_mem"))
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

/// 手写落盘带 refs 的 memory（A→B→C 链，trace_nodes=1 演示节点截断续读）。
fn seed_ref_record(dir: &std::path::Path, id_tail: &str, title: &str, refs: Vec<(&str, &str)>) -> String {
    let store = mem_core::store::Store::open(dir, mem_core::store::OpenMode::Auto).unwrap();
    let id = format!("01a2c500-0000-7000-8000-0000000000{id_tail}");
    let env = mem_core::envelope::Envelope {
        id: id.clone(),
        r#type: "memory".into(),
        created: "2026-11-06T00:00:00Z".into(),
        modified: "2026-11-06T00:00:00Z".into(),
        version: 1,
        principal: None,
        refs: refs.iter().map(|(rel, target)| mem_core::envelope::Ref {
            rel: rel.to_string(),
            target: target.to_string(),
        }).collect(),
        aliases: vec![],
        evidence_cutoff_seq: None,
        commit: None,
        extra: Default::default(),
    };
    let bytes = mem_core::record::serialize_new(&env, &json!({"title": title}), b"");
    let rel = format!("records/memory/{id}.md");
    let abs = store.abs(&rel);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(abs, bytes).unwrap();
    id
}

#[test]
fn r12_mcp_trace_resume_replay_no_repeat() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    let s = dir.to_str().unwrap().to_string();
    mem().args(["--store", &s, "init", &s, "--principal", "codex_main"]).output().unwrap();
    // 节点预算 1：A→B→C 链上每次 trace 只吐 1 个节点。
    let mem_json = dir.join("mem.json");
    let mut cfg: Value = serde_json::from_slice(&std::fs::read(&mem_json).unwrap()).unwrap();
    cfg["budgets"]["trace_nodes"] = json!(1);
    std::fs::write(&mem_json, serde_json::to_vec_pretty(&cfg).unwrap()).unwrap();

    let c = seed_ref_record(&dir, "00c3", "C", vec![]);
    let b = seed_ref_record(&dir, "00b2", "B", vec![("derived_from", &c)]);
    let a = seed_ref_record(&dir, "00a1", "A", vec![("derived_from", &b)]);

    // 真实 MCP stdio：第一次 trace → 节点预算截断 resume{skip:1,limit:2}；
    // 第二次**原样携带 skip/limit 重放**——必须不重复 B 且到达 C（B7-2 关闭条件）。
    let req = |id: i64, args: Value| {
        json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": "mem_trace", "arguments": args}
        })
    };
    let resps = mcp_session(&s, &[
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        req(2, json!({"id": a, "dir": "up"})),
        req(3, json!({"id": a, "dir": "up", "skip": 1, "limit": 2})),
    ]);
    assert_eq!(resps.len(), 3, "三个请求三行应答");
    let first = tool_result(&resps[1]);
    let first_ids: Vec<&str> = first["nodes"].as_array().unwrap()
        .iter().map(|n| n["id"].as_str().unwrap()).collect();
    assert_eq!(first_ids, vec![b.as_str()], "节点预算 1：首次只返回 B\n{first}");
    let resume = &first["resume"];
    assert_eq!(resume["skip"].as_u64(), Some(1), "首次 resume.skip=1（可执行）\n{first}");
    assert_eq!(resume["limit"].as_u64(), Some(2), "首次 resume.limit=nodes+1（可执行）\n{first}");

    let second = tool_result(&resps[2]);
    let second_ids: Vec<&str> = second["nodes"].as_array().unwrap()
        .iter().map(|n| n["id"].as_str().unwrap()).collect();
    assert!(
        !second_ids.contains(&b.as_str()),
        "重放不得重复 B（B7-2：skip/limit 必须被输入端执行）：{second}"
    );
    assert_eq!(second_ids, vec![c.as_str()], "重放应到达后续节点 C：{second}");
}

// ---------- B7-3：MCP show/brief 字节预算与续读 ----------

#[test]
fn r12_mcp_show_brief_byte_budget() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    let s = dir.to_str().unwrap().to_string();
    mem().args(["--store", &s, "init", &s, "--principal", "codex_main"]).output().unwrap();
    let mem_json = dir.join("mem.json");
    let mut cfg: Value = serde_json::from_slice(&std::fs::read(&mem_json).unwrap()).unwrap();
    cfg["budgets"]["brief_bytes"] = json!(16);
    std::fs::write(&mem_json, serde_json::to_vec_pretty(&cfg).unwrap()).unwrap();

    // 205 字节正文（R12 复现场景）。
    let big = seed_memory(&dir, "00b1", vec![], json!({"title": "big-body"}), &"x".repeat(205));

    let req = |id: i64, name: &str, args: Value| {
        json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": name, "arguments": args}
        })
    };
    // mem_show：正文按 brief_bytes=16 截断 + resume.body_offset 续读（B7-3）。
    let resps = mcp_session(&s, &[
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        req(2, "mem_show", json!({"id": big})),
        req(3, "mem_show", json!({"id": big, "body_offset": 16})),
    ]);
    let show1 = tool_result(&resps[1]);
    assert_eq!(
        show1["body"].as_str().map(|b| b.len()), Some(16),
        "MCP show 正文必须按 brief_bytes 截断（R4.2 不因前端豁免）：{show1}"
    );
    assert_eq!(show1["body_truncated"], json!(true), "{show1}");
    assert_eq!(show1["body_bytes_total"], json!(205), "{show1}");
    assert_eq!(show1["resume"]["body_offset"], json!(16), "{show1}");
    let show2 = tool_result(&resps[2]);
    assert_eq!(show2["body"].as_str().map(|b| b.len()), Some(16), "续读第二页 16 字节：{show2}");
    assert_eq!(show2["resume"]["body_offset"], json!(32), "{show2}");

    // 续读到尾：拼接还原完整正文（逐页 resume 前进，直到 resume=null）。
    let mut assembled = String::new();
    let mut offset = 0usize;
    for _ in 0..40 {
        let resps = mcp_session(&s, &[
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
            req(2, "mem_show", json!({"id": big, "body_offset": offset})),
        ]);
        let v = tool_result(&resps[1]);
        assembled.push_str(v["body"].as_str().unwrap_or_default());
        match v["resume"]["body_offset"].as_u64() {
            Some(next) => offset = next as usize,
            None => break,
        }
    }
    assert_eq!(assembled.len(), 205, "MCP show 续读必须能还原完整正文（红线9）");
    assert!(assembled.chars().all(|c| c == 'x'));

    // mem_brief：items 数组受字节预算约束，超预算条目跳过并进入 skipped_over_budget
    // 清单；resume.offset 指向下一未处理项（r9 语义，与 CLI 同源）。
    let resps = mcp_session(&s, &[
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        req(2, "mem_brief", json!({})),
    ]);
    let brief = tool_result(&resps[1]);
    assert_eq!(brief["bytes_budget"], json!(16), "{brief}");
    let used = brief["bytes_used"].as_u64().unwrap_or(0);
    assert!(used <= 16, "bytes_used 不得超预算：{brief}");
    // 每个条目 JSON（id+type+modified+token）远超 16 字节 → 全部跳过。
    assert_eq!(brief["items"].as_array().map(|a| a.len()), Some(0), "16 字节预算装不下任何完整条目：{brief}");
    let skipped = brief["skipped_over_budget"].as_array().expect("skipped_over_budget 清单");
    assert!(!skipped.is_empty(), "超预算条目必须可见（R6.1）：{brief}");
    assert!(skipped.iter().all(|e| e["id"].as_str().is_some() && e["bytes"].as_u64().is_some()));
    assert_eq!(
        brief["resume"]["offset"].as_u64(),
        Some(skipped.len() as u64),
        "resume.offset = 下一未处理项（跳过项不阻塞分页）：{brief}"
    );
}

#[test]
fn r12_mcp_budget_below_minimum_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    let s = dir.to_str().unwrap().to_string();
    mem().args(["--store", &s, "init", &s, "--principal", "codex_main"]).output().unwrap();
    let mem_json = dir.join("mem.json");
    let mut cfg: Value = serde_json::from_slice(&std::fs::read(&mem_json).unwrap()).unwrap();
    cfg["budgets"]["brief_bytes"] = json!(1);
    std::fs::write(&mem_json, serde_json::to_vec_pretty(&cfg).unwrap()).unwrap();

    let resps = mcp_session(&s, &[
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
               "params": {"name": "mem_show", "arguments": {"id": "01a2"}}}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
               "params": {"name": "mem_brief", "arguments": {}}}),
    ]);
    // brief_bytes=1 是配置错误 → 响亮拒收（JSON-RPC error，r10 CLI 语义的 MCP 对齐）。
    assert!(resps[1]["error"].is_object(), "MCP show 在 brief_bytes=1 下必须拒收：{}", resps[1]);
    assert!(resps[2]["error"].is_object(), "MCP brief 在 brief_bytes=1 下必须拒收：{}", resps[2]);
    assert!(resps[1]["error"]["message"].as_str().unwrap_or("").contains("最小为 2"));
}
