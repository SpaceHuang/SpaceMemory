//! MCP server 前端（D7 / §3.4 L4 档）。
//!
//! 工具面要点：
//! - MCP initialize = 显式建立操作：建/接手台账（D13.1/红线25）；
//! - 工具 schema 携带 `expect` 全修订令牌（版本 + 64 hex）——把"读到的令牌原样
//!   带回"变成协议约束（红线19）；
//! - MCP 断连降 CLI（§3.4 降级路径）；principal 经 MCP 通道自我声明（防错报、
//!   不防伪装，红线12）。
//!
//! v1 实现形态：**手写最小 MCP stdio 协议**（JSON-RPC 2.0：initialize /
//! notifications/initialized / tools/list / tools/call），协议面与 CLI 命令面同构
//! （§3.3）。rmcp 装配为第二阶段（工具面随开放问题29 定稿后切换）。
//!
//! **M1 裁决（v1.2 评审归档）**：MCP initialize **只做协议握手，不建台账**——
//! D13.1 的"MCP initialize 触发器"以 mem_session_start 显式调用落地（principal
//! 必填）：身份不可从协议噪声推断，红线25 优先于触发器字面。

pub mod protocol;
pub mod tools;

pub use protocol::handle_request;
