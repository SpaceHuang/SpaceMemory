//! R11 MCP 协议档测试（验收 4：仅完整工具协议档的宿主完成"定位→钻取→取回"全链；
//! 验收 7 的对等半边——mem_restore_body 与 CLI restore-body 同批义务）。
//!
//! 直接驱动 mem_mcp::protocol::handle_request（与 mem bin 的 stdio 装配同一入口）。

use mem_core::store::Store;
use serde_json::{json, Value};

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn b64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16
            | (c.get(1).map(|b| *b as u32).unwrap_or(0)) << 8
            | c.get(2).map(|b| *b as u32).unwrap_or(0);
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

fn b64_decode(s: &str) -> Vec<u8> {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::new();
    let mut buf = 0u32;
    let mut bits = 0u32;
    for c in s.bytes() {
        if c == b'=' || c == b'\n' || c == b'\r' {
            continue;
        }
        let v = T.iter().position(|&t| t == c).unwrap() as u32;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    out
}

/// 建店 + 播种类型两件套（直接读仓库资产——机制路径仍走数据目录副本）。
fn init_store(dir: &std::path::Path) {
    Store::create(dir).unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/builtin-types");
    fn walk(src: &std::path::Path, dst: &std::path::Path) {
        for e in std::fs::read_dir(src).unwrap() {
            let e = e.unwrap();
            let to = dst.join(e.file_name());
            if e.path().is_dir() {
                std::fs::create_dir_all(&to).unwrap();
                walk(&e.path(), &to);
            } else {
                std::fs::copy(e.path(), &to).unwrap();
            }
        }
    }
    walk(&root, dir);
}

fn call(root: &std::path::Path, id: i64, name: &str, args: Value) -> Value {
    let req = json!({
        "jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": {"name": name, "arguments": args}
    });
    let resp = mem_mcp::handle_request(root, &req).expect("tools/call 必有应答");
    let text = resp["result"]["content"][0]["text"].as_str().unwrap_or_default();
    serde_json::from_str(text)
        .unwrap_or_else(|e| panic!("工具输出非 JSON：{e}\n{text}"))
}

/// PNG 头 + 非 UTF-8 字节 + "abs" 编巧合。
fn png_like_bytes() -> Vec<u8> {
    let mut b = vec![0x89u8, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    b.extend_from_slice(b"\x00\x00\x00\x0dIHDRabs-encode");
    b.push(0xFF);
    b.push(0xFE);
    b
}

// ---------- 验收 4：协议档全链（定位 → 钻取 → 取回，字节与写入一致） ----------

#[test]
fn acceptance4_protocol_full_chain_b64_paging() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    init_store(&dir);
    let src_bytes = png_like_bytes();

    // 写入：base64 保真通道解码后按原始字节落盘（既有裁决 M4）。
    let added = call(&dir, 1, "mem_add", json!({
        "type": "memory", "payload": {"title": "protocol-image"},
        "body_b64": b64_encode(&src_bytes),
    }));
    assert_eq!(added["created"], true, "{added}");
    let id = added["id"].as_str().unwrap().to_string();

    // 定位：类型过滤 list（检索能力本身归 R10，另一提案）。
    let listed = call(&dir, 2, "mem_list", json!({"type": "memory"}));
    assert!(listed["items"].as_array().unwrap()
        .iter().any(|i| i["id"] == added["id"]), "{listed}");

    // 钻取分页：页 = brief_bytes 个原始字节，base64 交付；body=null、body_utf8=false。
    let mem_json = dir.join("mem.json");
    let mut cfg: Value = serde_json::from_slice(&std::fs::read(&mem_json).unwrap()).unwrap();
    cfg["budgets"]["brief_bytes"] = json!(8);
    std::fs::write(&mem_json, serde_json::to_vec_pretty(&cfg).unwrap()).unwrap();

    let mut assembled = Vec::new();
    let mut offset = 0usize;
    for _ in 0..1000 {
        let v = call(&dir, 3, "mem_show", json!({"id": id, "body_offset": offset}));
        assert_eq!(v["body_utf8"], false, "恒有机判字段（BD3）：{v}");
        assert_eq!(v["body"], Value::Null, "非文本正文文本通道关闭（BD2）");
        let b64 = v["body_b64"].as_str().expect("body_b64 分页：{v}");
        assembled.extend_from_slice(&b64_decode(b64));
        if v["body_truncated"].as_bool().unwrap() {
            offset = v["resume"]["body_offset"].as_u64().unwrap() as usize;
        } else {
            break;
        }
    }
    // base64 解码哈希 == 写入哈希（验收 4）。
    assert_eq!(sha256_hex(&assembled), sha256_hex(&src_bytes));
    assert_eq!(assembled, src_bytes, "逐字节一致");
}

#[test]
fn mem_show_text_body_utf8_multipage_assembly() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    init_store(&dir);
    let body_text = "协议档正文分页".repeat(10);
    let added = call(&dir, 1, "mem_add", json!({
        "type": "memory", "payload": {"title": "text"}, "body": body_text,
    }));
    let id = added["id"].as_str().unwrap().to_string();
    let mem_json = dir.join("mem.json");
    let mut cfg: Value = serde_json::from_slice(&std::fs::read(&mem_json).unwrap()).unwrap();
    cfg["budgets"]["brief_bytes"] = json!(8);
    std::fs::write(&mem_json, serde_json::to_vec_pretty(&cfg).unwrap()).unwrap();

    let mut joined = String::new();
    let mut offset = 0usize;
    for _ in 0..1000 {
        let v = call(&dir, 2, "mem_show", json!({"id": id, "body_offset": offset}));
        assert_eq!(v["body_utf8"], true);
        assert_eq!(v["body_b64"], Value::Null, "合法正文不走 base64 通道（自适应，BD2）");
        joined.push_str(v["body"].as_str().unwrap());
        if v["body_truncated"].as_bool().unwrap() {
            offset = v["resume"]["body_offset"].as_u64().unwrap() as usize;
        } else {
            break;
        }
    }
    assert_eq!(joined, body_text, "文本通道切点码点对齐——跨页拼装恒等（G2）");
}

// ---------- mem_restore_body：与 CLI restore-body 同批义务（R11.5 对等） ----------

#[test]
fn mem_restore_body_file_source_and_echo() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    init_store(&dir);
    let src_bytes = png_like_bytes();
    let added = call(&dir, 1, "mem_add", json!({
        "type": "memory", "payload": {"title": "restore"}, "body_b64": b64_encode(&src_bytes),
    }));
    let id = added["id"].as_str().unwrap().to_string();
    let hit = call(&dir, 2, "mem_show", json!({"id": id}));
    let rel = hit["rel"].as_str().unwrap().to_string();
    let record_path = dir.join(&rel);
    let pristine = std::fs::read(&record_path).unwrap();
    // 外部翻转正文一个字节。
    let body = mem_core::record::find_body_after_first_delimiter(&pristine).unwrap();
    let mut corrupted = pristine.clone();
    corrupted[pristine.len() - body.len()] ^= 0x01;
    std::fs::write(&record_path, &corrupted).unwrap();
    // 钻取取当前（损坏）令牌——红线19 全令牌 CAS。
    let hit2 = call(&dir, 3, "mem_show", json!({"id": id}));
    let expect = hit2["token"].as_str().unwrap().to_string();
    // 店外备份（记录格式文件）。
    let backup = tmp.path().join("backup.md");
    std::fs::write(&backup, &pristine).unwrap();
    // 恢复（file 源；git 缺席的店 file 仍可用）。
    let out = call(&dir, 4, "mem_restore_body", json!({
        "id": id, "expect": expect,
        "source": format!("file:{}", backup.to_str().unwrap()),
    }));
    assert_eq!(out["source"]["form"], "file", "{out}");
    assert_eq!(out["source"]["body_sha256_source"], json!(sha256_hex(&body)));
    assert_eq!(out["version"], json!(2), "版本 +1");
    // 取回：恢复后字节 == 写入字节（协议档全链闭合）。
    let v = call(&dir, 5, "mem_show", json!({"id": id}));
    assert_eq!(sha256_hex(&b64_decode(v["body_b64"].as_str().unwrap())), sha256_hex(&src_bytes),
        "恢复后正文与写入字节一致：{v}");
}

// ---------- 工具面：mem_restore_body 定义在列（14→15，BD4 占位名） ----------

#[test]
fn tools_list_includes_restore_body() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    init_store(&dir);
    let req = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"});
    let resp = mem_mcp::handle_request(&dir, &req).unwrap();
    let tools = resp["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 15, "工具面 14→15（BD4）：{}", tools.len());
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(names.contains(&"mem_restore_body"), "{names:?}");
    // mem_show 描述声明 body_utf8/body_b64（BD2 形状随工具面可见）。
    let show = tools.iter().find(|t| t["name"] == "mem_show").unwrap();
    assert!(show["description"].as_str().unwrap().contains("body_b64"), "{show}");
}
