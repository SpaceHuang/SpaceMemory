//! `mem` —— 多调用二进制（D7）：机制层的唯一交付形态。
//!
//! 三前端合一：CLI 子命令（§3.3）、`mem serve`（sidecar，D2）、`mem mcp`（MCP
//! server）。同一机制核心，无守护义务；sidecar 任意时刻可 kill，冷启动重扫重建。

mod cli;
mod commands;
mod mcp;
mod mcp_install;
mod serve;

fn main() {
    let mut args = std::env::args();
    let first = args.nth(1);
    let second = args.next();
    match (first.as_deref(), second.as_deref()) {
        (Some("serve"), _) => serve::run(),
        (Some("mcp"), None) => mcp::run(),
        (Some("mcp"), Some("--help" | "-h" | "help")) => mcp_install::print_help(),
        (Some("mcp"), Some(_)) => cli::run(),
        _ => cli::run(),
    }
}
