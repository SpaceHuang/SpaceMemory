//! `mem` 机制核心库。
//!
//! 承载设计文档（docs/develop/agent-memory-design.md）§1.1 的"机制层"：
//! 身份与信封、并发协议（门闩 → 记录锁 → 全摘要令牌 CAS）、事件日志与状态覆盖回填、
//! 证明式新鲜性、会话台账、agenda、预算、引用与派生维护。
//! 宿主前端（CLI / serve / MCP）只是本库的薄壳（D7）。
//!
//! 并发与正确性铁律（摘要，全文见设计文档 §5 红线）：
//! - 读路径零写盘（红线18）；
//! - 一切读改写必须携带修订令牌（版本 + 完整 SHA-256）并在锁内比较（红线19）；
//! - 维护屏障必须持有而非探测；锁序全库统一：门闩 → 记录锁 → 序列锁（红线20）；
//! - 索引仅在新鲜性可证明时服务读，否则直读真相层（红线21）；
//! - 全库事件序号由序列锁分配；记录状态为三元组 (location, version, digest)（红线24）；
//! - 机制再生成 frontmatter 时正文按字节保持不变（红线27）。
//!
//! 内容类型与载荷结构禁止编译进本库（红线1/R8.2）；载荷仅由类型声明解释，
//! 校验见 `mem-schema`。

pub mod agenda;
pub mod body;
pub mod budget;
pub mod clock;
pub mod drift;
pub mod envelope;
pub mod error;
pub mod event;
pub mod freshness;
pub mod gitattrs;
pub mod lock;
pub mod ops;
pub mod paths;
pub mod platform;
pub mod record;
pub mod refgraph;
pub mod search;
pub mod session;
pub mod sidecar;
pub mod store;
pub mod token;
pub mod view;
