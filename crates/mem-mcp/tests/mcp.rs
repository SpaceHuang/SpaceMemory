//! §8-7 MCP 前端测试：initialize / tools/list / tools/call 闭环（D7/§3.4 L4）。

use serde_json::{json, Value};

const PROJECT: &str = "0198f7a9-0000-7cd2-9e3f-000000000001";

fn fresh_store() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("store");
    mem_core::store::Store::create(&root).unwrap();
    let asset_root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/builtin-types");
    fn walk(dir: &std::path::Path, rel: &str, out: &mut Vec<(String, Vec<u8>)>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let name = e.file_name().to_string_lossy().to_string();
            let p = e.path();
            let r = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
            if p.is_dir() {
                walk(&p, &r, out);
            } else {
                out.push((r, std::fs::read(p).unwrap()));
            }
        }
    }
    let mut files = Vec::new();
    walk(&asset_root, "", &mut files);
    for (p, bytes) in files {
        let abs = root.join(&p);
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(abs, bytes).unwrap();
    }
    (tmp, root)
}

fn call(root: &std::path::Path, method: &str, params: Value) -> Value {
    let req = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    let resp = mem_mcp::handle_request(root, &req).expect("有应答");
    assert_eq!(resp["id"], 1);
    resp["result"].clone()
}

fn tool_text(result: &Value) -> Value {
    let text = result["content"][0]["text"].as_str().unwrap();
    serde_json::from_str(text).unwrap()
}

#[test]
fn initialize_and_tools_list() {
    let (_t, root) = fresh_store();
    let init = call(&root, "initialize", json!({"clientInfo": {"name": "test-host", "version": "1"}}));
    assert_eq!(init["protocolVersion"], "2024-11-05");
    assert_eq!(init["serverInfo"]["name"], "mem");
    // 通知无应答。
    let req = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
    assert!(mem_mcp::handle_request(&root, &req).is_none());
    let tools = call(&root, "tools/list", json!({}));
    let names: Vec<&str> = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"mem_update"));
    // expect schema 带 64 hex 全令牌约束（红线19）。
    let update = tools["tools"].as_array().unwrap().iter().find(|t| t["name"] == "mem_update").unwrap();
    let pattern = update["inputSchema"]["properties"]["expect"]["properties"]["version"]["pattern"]
        .as_str()
        .unwrap();
    assert!(pattern.contains("{64}"), "{pattern}");
}

#[test]
fn session_and_record_flow_via_tools() {
    let (_t, root) = fresh_store();
    // 自举 principal（MCP 通道要求显式 principal；先手工播种一条）。
    let types = mem_schema::TypeSet::load_dir(&root.join("types")).unwrap();
    let store = mem_core::store::Store::open(&root, mem_core::store::OpenMode::Auto).unwrap();
    let ctx = mem_core::ops::WriteContext {
        store: &store,
        types: &types,
        by: "seed".into(),
        committer: None,
        session_id: None,
    };
    mem_core::ops::add(&ctx, mem_core::ops::AddArgs {
        type_name: "principal".into(),
        payload: json!({"slug": "codex_main", "label": "codex_main"}),
        body: b"".to_vec(),
        principal: None,
    })
    .unwrap();
    // session start（显式建立，红线25）。
    let r = tool_text(&call(&root, "tools/call", json!({
        "name": "mem_session_start",
        "arguments": { "principal": "codex_main", "task_ref": "task-1" }
    })));
    assert_eq!(r["status"], "active");
    // brief：identity 块出现。
    let r2 = tool_text(&call(&root, "tools/call", json!({ "name": "mem_brief", "arguments": {} })));
    assert!(r2["identity"].is_object());
    // add + update（expect 全令牌）。
    let r3 = tool_text(&call(&root, "tools/call", json!({
        "name": "mem_add",
        "arguments": { "type": "project", "payload": {"slug":"proj","title":"P","status":"active"}, "session": r["session"] }
    })));
    assert_eq!(r3["created"], true);
    let pid = r3["id"].as_str().unwrap();
    let token = r3["token"].as_str().unwrap();
    let r4 = tool_text(&call(&root, "tools/call", json!({
        "name": "mem_update",
        "arguments": { "id": pid, "expect": {"version": token},
            "payload": {"slug":"proj","title":"P2","status":"paused"} }
    })));
    assert_eq!(r4["version"], 2);
    // 陈旧令牌 → isError + 冲突信息。
    let _resp = call(&root, "tools/call", json!({
        "name": "mem_update",
        "arguments": { "id": pid, "expect": {"version": token},
            "payload": {"slug":"proj","title":"P3","status":"paused"} }
    }));
    // dispatch_tool 返回 Err → 协议层 error 对象。
    let req = json!({ "jsonrpc": "2.0", "id": 9, "method": "tools/call",
        "params": { "name": "mem_update", "arguments": { "id": pid, "expect": {"version": token},
            "payload": {"slug":"proj","title":"P3","status":"paused"} } } });
    let resp = mem_mcp::handle_request(&root, &req).unwrap();
    assert!(resp["error"]["message"].as_str().unwrap().contains("conflict"), "{resp}");
    let _ = r4;
}

#[test]
fn diff_and_verify_tools() {
    let (_t, root) = fresh_store();
    let types = mem_schema::TypeSet::load_dir(&root.join("types")).unwrap();
    let store = mem_core::store::Store::open(&root, mem_core::store::OpenMode::Auto).unwrap();
    let ctx = mem_core::ops::WriteContext {
        store: &store,
        types: &types,
        by: "seed".into(),
        committer: None,
        session_id: None,
    };
    mem_core::ops::add(&ctx, mem_core::ops::AddArgs {
        type_name: "memory".into(),
        payload: json!({"title": "m"}),
        body: b"".to_vec(),
        principal: None,
    })
    .unwrap();
    let r = tool_text(&call(&root, "tools/call", json!({ "name": "mem_verify", "arguments": {} })));
    assert_eq!(r["ok"], true, "{r}");
    let r2 = tool_text(&call(&root, "tools/call", json!({ "name": "mem_diff", "arguments": {"since": 0} })));
    assert_eq!(r2["current_drift"].as_array().unwrap().len(), 0);
    assert_eq!(r2["changes_since"].as_array().unwrap().len(), 1);
    let _ = PROJECT;
}
