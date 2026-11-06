//! 派生索引 index.sqlite（D5）。
//!
//! 索引是**派生缓存、可丢弃**：过滤/排序/分页/FTS/预算 LIMIT、引用边表多跳。
//! 三条纪律：
//! - **索引永不 gate 正确性**；索引约束仅作对账诊断，永不作为拒收依据（红线8）；
//! - **仅在新鲜性可证明时服务读**（stat 一致 ∧ 变更日志订阅覆盖无缺口），否则
//!   直读真相层并标注（红线21；判定协议在 mem-core::freshness）；
//! - **读路径零写盘**（红线18）：索引持久化只发生在写例程（写路径 / reconcile /
//!   watcher 例程）。
//!
//! FTS 仅在快路径可用；直读模式标注"全文检索需要 sidecar 在场"。
//! 索引上不建唯一性/完整性强制点——强制点必须在真相层原子原语（红线8）。

pub mod checkpoint;
pub mod fts;
pub mod sqlite;

pub use sqlite::{Index, IndexRow, RefRow};
