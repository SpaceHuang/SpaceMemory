//! `mem mcp` —— MCP server 前端（D7/§3.4 L4 档）。
//!
//! stdio JSON-RPC 2.0 循环：stdin 每行一个请求 → mem_mcp::handle_request → stdout
//! 应答。initialize 建/接手台账经 mem_session_start 显式完成（红线25）；断连时宿主
//! 降级 CLI（§3.4 降级路径）。

pub fn run() {
    let root = std::env::var("MEM_STORE").unwrap_or_else(|_| {
        eprintln!(r#"{{"error":"mem mcp 需要 MEM_STORE 环境变量"}}"#);
        std::process::exit(1);
    });
    let root = std::path::PathBuf::from(root);
    mem_mcp::protocol::remember_root(&root);
    use std::io::BufRead;
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(req) = serde_json::from_str::<serde_json::Value>(&line) else {
            let resp = serde_json::json!({
                "jsonrpc": "2.0", "id": null,
                "error": { "code": -32700, "message": "解析错误（非 JSON 行）" }
            });
            println!("{resp}");
            continue;
        };
        match mem_mcp::handle_request(&root, &req) {
            Some(resp) => {
                use std::io::Write;
                println!("{resp}");
                let _ = out.flush();
            }
            None => {} // 通知：无应答
        }
    }
}
