//! r13 回归：B9-1 MCP trace 深度截断 resume 可执行、B9-2 MCP diff 组级 offset 续读、
//! B9-3 MCP verify 镜像/计划/deep 审计接入（分裂店不得误报 ok:true）、B9-4 RDC 哨兵名
//! 子串过滤吞记录变更（含哨兵子串文件名的记录第一次读取必须命中新内容，红线21）。

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

fn mcp_req(id: i64, name: &str, args: Value) -> Value {
    json!({
        "jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": {"name": name, "arguments": args}
    })
}

/// 手写落盘带 refs 的 memory（A→B→C 链）。
fn seed_ref_record(dir: &std::path::Path, id_tail: &str, title: &str, refs: Vec<(&str, &str)>) -> String {
    let store = mem_core::store::Store::open(dir, mem_core::store::OpenMode::Auto).unwrap();
    let id = format!("01a3d600-0000-7000-8000-0000000000{id_tail}");
    let env = mem_core::envelope::Envelope {
        id: id.clone(),
        r#type: "memory".into(),
        created: "2026-11-07T00:00:00Z".into(),
        modified: "2026-11-07T00:00:00Z".into(),
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

/// 手写落盘 memory（可指定 rel——含哨兵子串的文件名，外部放置覆盖域）。
fn seed_memory_at(dir: &std::path::Path, rel: &str, id_tail: &str, title: &str) -> String {
    let store = mem_core::store::Store::open(dir, mem_core::store::OpenMode::Auto).unwrap();
    let id = format!("01a3d700-0000-7000-8000-0000000000{id_tail}");
    let env = mem_core::envelope::Envelope {
        id: id.clone(),
        r#type: "memory".into(),
        created: "2026-11-07T00:00:00Z".into(),
        modified: "2026-11-07T00:00:00Z".into(),
        version: 1,
        principal: None,
        refs: vec![],
        aliases: vec![],
        evidence_cutoff_seq: None,
        commit: None,
        extra: Default::default(),
    };
    let bytes = mem_core::record::serialize_new(&env, &json!({"title": title}), b"");
    let abs = store.abs(rel);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(abs, bytes).unwrap();
    id
}

// ---------- B9-1：MCP mem_trace 深度截断 resume ----------

#[test]
fn r13_mcp_trace_depth_resume_executable() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    let s = dir.to_str().unwrap().to_string();
    mem().args(["--store", &s, "init", &s, "--principal", "codex_main"]).output().unwrap();

    let c = seed_ref_record(&dir, "00c3", "C", vec![]);
    let b = seed_ref_record(&dir, "00b2", "B", vec![("derived_from", &c)]);
    let a = seed_ref_record(&dir, "00a1", "A", vec![("derived_from", &b)]);

    // depth=1 低于全局预算（显式 trace_depth=4）：首次返回 [B]，truncated=true，
    // resume 必须携带可执行续读（与 CLI 的 --depth+1 建议同构）——不得为 null。
    let resps = mcp_session(&s, &[
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        mcp_req(2, "mem_trace", json!({"id": a, "dir": "up", "depth": 1})),
        mcp_req(3, "mem_trace", json!({"id": a, "dir": "up", "depth": 2})),
    ]);
    let first = tool_result(&resps[1]);
    let first_ids: Vec<&str> = first["nodes"].as_array().unwrap()
        .iter().map(|n| n["id"].as_str().unwrap()).collect();
    assert_eq!(first_ids, vec![b.as_str()], "depth=1 只到 B：{first}");
    assert_eq!(first["truncated"], json!(true), "{first}");
    let resume = &first["resume"];
    assert!(
        !resume.is_null(),
        "深度截断（depth<预算）必须有续读路径（B9-1/红线9）：{first}"
    );
    assert_eq!(resume["reason"], json!("深度预算截断"), "{first}");
    assert_eq!(resume["depth"], json!(2), "resume 携带 depth+1 可执行续读：{first}");

    // 按 resume 重放 depth=2：到达 C（重跑语义与 CLI 同）。
    let second = tool_result(&resps[2]);
    let second_ids: Vec<&str> = second["nodes"].as_array().unwrap()
        .iter().map(|n| n["id"].as_str().unwrap()).collect();
    assert!(second_ids.contains(&c.as_str()), "depth=2 重放应到达 C：{second}");
}

// ---------- B9-2：MCP mem_diff 组级 offset 续读 ----------

#[test]
fn r13_mcp_diff_offset_resume_covers_all_groups() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    let s = dir.to_str().unwrap().to_string();
    mem().args(["--store", &s, "init", &s, "--principal", "codex_main"]).output().unwrap();
    // 组级预算 1：每次只吐 1 组 changes_since。
    let mem_json = dir.join("mem.json");
    let mut cfg: Value = serde_json::from_slice(&std::fs::read(&mem_json).unwrap()).unwrap();
    cfg["budgets"]["diff_since_limit"] = json!(1);
    std::fs::write(&mem_json, serde_json::to_vec_pretty(&cfg).unwrap()).unwrap();

    // 4 组变更：init 自举 principal + 3 次 add。
    let mut added = Vec::new();
    for i in 0..3 {
        let o = mem().args(["--store", &s, "add", "memory", "--payload",
            &format!(r#"{{"title":"d{i}"}}"#)]).output().unwrap();
        added.push(j(&o)["id"].as_str().unwrap().to_string());
    }

    // 第一页：截断 + resume.offset 可执行（B9-2：此前无 resume、schema 无 offset）。
    let resps = mcp_session(&s, &[
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        mcp_req(2, "mem_diff", json!({"since": 0})),
        mcp_req(3, "mem_diff", json!({"since": 0, "offset": 1})),
    ]);
    let page1 = tool_result(&resps[1]);
    assert_eq!(page1["changes_since"].as_array().unwrap().len(), 1, "{page1}");
    assert_eq!(page1["changes_truncated"], json!(true), "{page1}");
    assert_eq!(page1["total_groups"], json!(4), "{page1}");
    assert_eq!(page1["resume"]["offset"], json!(1), "截断必须附组级续读偏移：{page1}");
    let first_id = page1["changes_since"][0]["id"].as_str().unwrap().to_string();

    let page2 = tool_result(&resps[2]);
    let second_id = page2["changes_since"][0]["id"].as_str().unwrap().to_string();
    assert_ne!(first_id, second_id, "第二页不得重复第一页：{page2}");

    // 逐页续读到收尾：4 组全部可达、无重复。
    let mut seen = vec![first_id.clone(), second_id.clone()];
    let mut offset = 2usize;
    for _ in 0..10 {
        let resps = mcp_session(&s, &[
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
            mcp_req(2, "mem_diff", json!({"since": 0, "offset": offset})),
        ]);
        let v = tool_result(&resps[1]);
        for g in v["changes_since"].as_array().unwrap() {
            seen.push(g["id"].as_str().unwrap().to_string());
        }
        match v["resume"]["offset"].as_u64() {
            Some(next) => offset = next as usize,
            None => break,
        }
    }
    let mut uniq = seen.clone();
    uniq.sort();
    uniq.dedup();
    assert_eq!(uniq.len(), 4, "4 组变更经续读全部可达且无重复（实得 {}）：{seen:?}", uniq.len());
    for id in &added {
        assert!(uniq.contains(id), "变更组 {id} 必须在续读覆盖内：{uniq:?}");
    }
}

// ---------- B9-3：MCP mem_verify 镜像/计划/deep 审计 ----------

#[test]
fn r13_mcp_verify_split_store_not_green() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    let s = dir.to_str().unwrap().to_string();
    mem().args(["--store", &s, "init", &s, "--principal", "codex_main"]).output().unwrap();

    // 分裂店：memory 的 payload.topic 保留、envelope.refs 镜像边被外部删掉
    //（R13 立法场景——payload 与信封分裂）。topic 指向真实存在的 principal id
    //（避免 payload 悬空噪声，只留"镜像缺失"这一项）。
    let store = mem_core::store::Store::open(&dir, mem_core::store::OpenMode::Auto).unwrap();
    let principal_id = mem_core::ops::scan_all(&store).unwrap()
        .records.iter()
        .find(|r| r.record.envelope.r#type == "principal")
        .map(|r| r.record.envelope.id.clone())
        .unwrap();
    let id = "01a3d800-0000-7000-8000-0000000000m01".to_string();
    let env = mem_core::envelope::Envelope {
        id: id.clone(),
        r#type: "memory".into(),
        created: "2026-11-07T00:00:00Z".into(),
        modified: "2026-11-07T00:00:00Z".into(),
        version: 1,
        principal: None,
        refs: vec![], // 分裂点：payload.topic 的镜像边缺失
        aliases: vec![],
        evidence_cutoff_seq: None,
        commit: None,
        extra: Default::default(),
    };
    let bytes = mem_core::record::serialize_new(
        &env, &json!({"title": "M", "topic": principal_id}), b"");
    let abs = store.abs(&format!("records/memory/{id}.md"));
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(abs, bytes).unwrap();

    // MCP mem_verify：必须 ok:false 且镜像不一致可见（B9-3：此前误报 ok:true）。
    let resps = mcp_session(&s, &[
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        mcp_req(2, "mem_verify", json!({})),
        mcp_req(3, "mem_verify", json!({"deep": true})),
    ]);
    let v = tool_result(&resps[1]);
    assert_eq!(v["ok"], json!(false), "分裂店 MCP verify 不得误报全绿（B9-3）：{v}");
    let mirror = v["payload_mirror_inconsistencies"].as_array().expect("镜像审计字段");
    assert!(
        mirror.iter().any(|m| m.as_str().unwrap_or("").contains("镜像缺失")),
        "payload.topic 无 envelope 镜像必须被报告：{v}"
    );
    // deep 参数被实现读取（此前 schema 声明却被忽略）：应答携带摘要审计字段。
    let deep = tool_result(&resps[2]);
    assert!(deep["digest_mismatches"].is_array(), "deep:true 应执行摘要对照：{deep}");
    assert!(deep["plans"].is_array(), "deep:true 应执行计划审计：{deep}");
}

// ---------- B9-4：RDC 哨兵名子串过滤吞记录变更（e2e） ----------

#[cfg(windows)]
fn file_mtime_nanos(path: &std::path::Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, GENERIC_READ};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, GetFileTime, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    wide.push(0);
    unsafe {
        let h = CreateFileW(
            wide.as_ptr(), GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(), OPEN_EXISTING, 0, std::ptr::null_mut(),
        );
        if h.is_null() || h == -1isize as _ {
            return None;
        }
        let mut ft: FILETIME = std::mem::zeroed();
        let ok = GetFileTime(h, std::ptr::null_mut(), std::ptr::null_mut(), &mut ft);
        CloseHandle(h);
        if ok == 0 {
            return None;
        }
        Some(((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64)
    }
}

#[cfg(not(windows))]
fn file_mtime_nanos(_path: &std::path::Path) -> Option<u64> {
    None
}

#[cfg(windows)]
fn set_mtime_nanos(path: &std::path::Path, ft100: u64) {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, GENERIC_WRITE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, SetFileTime, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE,
        OPEN_EXISTING,
    };
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    wide.push(0);
    unsafe {
        let h = CreateFileW(
            wide.as_ptr(), GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(), OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, std::ptr::null_mut(),
        );
        if h.is_null() || h == -1isize as _ {
            return;
        }
        let ft = FILETIME {
            dwLowDateTime: (ft100 & 0xFFFF_FFFF) as u32,
            dwHighDateTime: (ft100 >> 32) as u32,
        };
        SetFileTime(h, std::ptr::null(), std::ptr::null(), &ft);
        CloseHandle(h);
    }
}

#[cfg(not(windows))]
fn set_mtime_nanos(_path: &std::path::Path, _ft100: u64) {}

#[test]
fn r13_sentinel_named_record_first_read_fresh() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    let s = dir.to_str().unwrap().to_string();
    mem().args(["--store", &s, "init", &s, "--principal", "codex_main"]).output().unwrap();

    // 外部放置覆盖域：记录文件名含哨兵子串（写路径不可生成，外部重命名/放置可）。
    let id = seed_memory_at(&dir, "records/memory/ext.rdc-sentinel.md", "0s01", "sentt");

    let mut serve = std::process::Command::new(env!("CARGO_BIN_EXE_mem"))
        .args(["serve", "--store", &s, "--idle-secs", "9"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    assert!(wait_until(|| dir.join("index/index.sqlite").is_file(), 50, 100));
    // 收敛：初始物化收录该记录。
    assert!(
        wait_until(|| {
            let v = j(&mem().args(["--store", &s, "find", "sentt"]).output().unwrap());
            v["hits"].as_array().map(|a| !a.is_empty()).unwrap_or(false)
        }, 50, 100),
        "初始物化应收录哨兵名记录"
    );

    // 同尺寸修改 sentt→sentu + mtime 纳秒还原（刻意绕过 stat 条件，B4/B8 同手法）。
    let store = mem_core::store::Store::open(&dir, mem_core::store::OpenMode::Auto).unwrap();
    let rel = "records/memory/ext.rdc-sentinel.md";
    let path = dir.join(rel);
    let mtime = file_mtime_nanos(&path);
    let mut edited = std::fs::read(&path).unwrap();
    let pos = edited.windows(5).position(|w| w == b"sentt").expect("title 字节存在");
    edited[pos..pos + 5].copy_from_slice(b"sentu");
    std::fs::write(&path, &edited).unwrap();
    if let Some(ns) = mtime {
        set_mtime_nanos(&path, ns);
    }
    let _ = store;

    // 红线21：第一次读取必须命中新内容（修复前：事件被当哨兵吞掉 → fresh Some(0)
    // → indexed 旧索引 → 漏命中）。
    let v = j(&mem().args(["--store", &s, "find", "sentu"]).output().unwrap());
    let ids: Vec<&str> = v["hits"].as_array().unwrap()
        .iter().map(|h| h["id"].as_str().unwrap()).collect();
    assert!(
        ids.contains(&id.as_str()),
        "含哨兵子串文件名的记录：同尺寸修改后第一次读取必须命中新内容（B9-4/红线21）\n{v}"
    );
    serve.kill().ok();
}
