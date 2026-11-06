//! 检查点与 stat 一致性（D5 证明式新鲜性条件 (a)）。
//!
//! records/ 目录枚举（路径 + mtime + size）与索引检查点的比对原语；完整判定
//! （含变更日志订阅覆盖检查，条件 (b)）在 mem-core::freshness。
//! 检查点持久化只发生在写例程/sidecar（读路径零写盘，红线18）。

use mem_core::error::{MemError, MemResult};
use mem_core::freshness::{self, Checkpoint};
use mem_core::store::Store;

/// 物化后刷新检查点（sidecar/reconcile 调用；读路径禁用）。
pub fn refresh(store: &Store) -> MemResult<Checkpoint> {
    let cp = Checkpoint {
        ts: mem_core::clock::now_rfc3339(),
        entries: freshness::stat_snapshot(store),
        index_epoch: 0, // 由收敛原语覆盖为真实代际；此路径仅为兼容保留
    };
    freshness::save_checkpoint(store, &cp)
        .map_err(|e| MemError::Corrupt { path: "index/checkpoint.json".into(), reason: e.to_string() })?;
    Ok(cp)
}

/// 读取检查点（读路径安全；None = 无检查点 → 直读形态）。
pub fn load(store: &Store) -> Option<Checkpoint> {
    freshness::load_checkpoint(store)
}
