//! `mem` —— 多调用二进制（D7）：机制层的唯一交付形态。
//!
//! 三前端合一：CLI 子命令（§3.3）、`mem serve`（sidecar，D2）、`mem mcp`（MCP
//! server）。同一机制核心，无守护义务；sidecar 任意时刻可 kill，冷启动重扫重建。

mod cli;
mod commands;
mod mcp;
mod serve;

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("serve") => serve::run(),
        Some("mcp") => mcp::run(),
        _ => cli::run(),
    }
}
