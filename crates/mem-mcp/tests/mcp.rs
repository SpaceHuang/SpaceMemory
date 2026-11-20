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
        pause_hook: None,
    };
    mem_core::ops::add(&ctx, mem_core::ops::AddArgs {
        type_name: "principal".into(),
        payload: json!({"slug": "codex_main", "label": "codex_main"}),
        body: b"".to_vec(),
        principal: Some(seed_owner(&store, &types)),
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
        pause_hook: None,
    };
    mem_core::ops::add(&ctx, mem_core::ops::AddArgs {
        type_name: "memory".into(),
        payload: json!({"title": "m"}),
        body: b"".to_vec(),
        principal: Some(seed_owner(&store, &types)),
    })
    .unwrap();
    let r = tool_text(&call(&root, "tools/call", json!({ "name": "mem_verify", "arguments": {} })));
    assert_eq!(r["ok"], true, "{r}");
    let r2 = tool_text(&call(&root, "tools/call", json!({ "name": "mem_diff", "arguments": {"since": 0} })));
    // 播种的归属身份记录（直写、无事件）以漂移形式出现——过滤后断言本意。
    let owner = "0198f7a9-0000-7000-8000-0000000000a1";
    let drift = r2["current_drift"].as_array().unwrap();
    assert_eq!(
        drift.iter().filter(|d| d["id"] != owner).count(),
        0,
        "漂移只剩播种的身份记录：{drift:?}"
    );
    assert_eq!(r2["changes_since"].as_array().unwrap().len(), 1);
    let _ = PROJECT;
}

// 幂等播种归属身份（唯一键路径独占：重复播种返回既有记录——同一 id）。
// 方案 principal-binding 决策①：受管创建需显式归属（principal 类型豁免）。
fn seed_owner(store: &mem_core::store::Store, types: &mem_schema::TypeSet) -> String {
    let _ = types; // 直写不经类型声明（外部写入者形态，同 seed_project 先例；不产生事件）
    let id = "0198f7a9-0000-7000-8000-0000000000a1".to_string();
    let env = mem_core::envelope::Envelope {
        id: id.clone(),
        r#type: "principal".into(),
        created: "2026-11-19T00:00:00Z".into(),
        modified: "2026-11-19T00:00:00Z".into(),
        version: 1,
        principal: None,
        refs: vec![],
        aliases: vec!["test-owner".into()],
        evidence_cutoff_seq: None,
        commit: None,
        extra: Default::default(),
    };
    let payload = serde_json::json!({"slug": "test-owner", "label": "测试归属"});
    let bytes = mem_core::record::serialize_new(&env, &payload, b"");
    let rel = "records/principal/test-owner.md";
    let abs = store.abs(rel);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(abs, bytes).unwrap();
    id
}


// ---------- 写路径归属绑定（方案 principal-binding §4.2 双端 e2e） ----------

#[test]
fn principal_binding_mcp_inherit_and_reject() {
    let (_t, root) = fresh_store();
    {
        let store = mem_core::store::Store::open(&root, mem_core::store::OpenMode::Auto).unwrap();
        let types = mem_schema::TypeSet::load_dir(&root.join("types")).unwrap();
        seed_owner(&store, &types);
    }
    // 继承态：session start（身份短名）→ mem_add 无 principal → 归属继承。
    let ss = tool_text(&call(&root, "tools/call",
        json!({ "name": "mem_session_start", "arguments": { "principal": "test-owner" } })));
    let sid = ss["session"].as_str().unwrap().to_string();
    let added = tool_text(&call(&root, "tools/call", json!({
        "name": "mem_add",
        "arguments": { "type": "memory", "payload": {"title": "继承"}, "session": sid }
    })));
    assert_eq!(added["created"], true, "{added}");
    // 拒绝态：无 session 无 principal → 响亮拒绝，错误含出路（JSON-RPC error 层）。
    let err_resp = mem_mcp::handle_request(&root, &json!({
        "jsonrpc": "2.0", "id": 9, "method": "tools/call",
        "params": { "name": "mem_add",
            "arguments": { "type": "memory", "payload": {"title": "拒绝"} } }
    })).expect("有应答");
    let msg = err_resp["error"]["message"].as_str().unwrap_or_default();
    assert!(msg.contains("归属缺失"), "{err_resp}");
    // 显式态：显式 principal（短名）→ 成功。
    let out_e = tool_text(&call(&root, "tools/call", json!({
        "name": "mem_add",
        "arguments": { "type": "memory", "payload": {"title": "显式"}, "principal": "test-owner" }
    })));
    assert_eq!(out_e["created"], true, "{out_e}");
}
