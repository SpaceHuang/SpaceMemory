//! 硬预算与分层访问（R4 / 红线9）。
//!
//! 每次访问的返回规模有确定上限；超限必须截断并**显式标注**（含继续读取的方式），
//! 不得静默丢弃。预算的执行者是基础设施（R4.4）：默认值来自 mem.json `budgets`，
//! 调用方可请求更小，不得超过 `list_limit_max`。
//!
//! 分层：brief → list/find → show → trace；跨类型通用操作只依赖信封（R8.4）。
//! current_drift 的独立分页预算在 drift::diff_since（D12/红线26）。

use crate::error::{MemError, MemResult};
use crate::store::Store;

/// r12 B7-3：brief_bytes 预算下限校验单源（CLI/MCP 共用）——<2 响亮拒收（R6.1/R4.4：
/// 配置错误不静默放宽；2 = 空 JSON 数组 "[]" 的最小开销，r10 复现）。
pub fn brief_bytes_budget(store: &Store) -> MemResult<usize> {
    let raw = store.config().budgets.brief_bytes as usize;
    if raw < 2 {
        return Err(MemError::BadArgument(
            "brief_bytes 预算最小为 2（空响应数组的开销）——请修正 mem.json".into(),
        ));
    }
    Ok(raw)
}

/// r9 语义的条目字节预算（CLI/MCP 共用，r12 B7-3 下沉单源）：条目完整 JSON 超预算
/// → **跳过**（不返回、不降级——任何降级形式仍可能超预算），记录到 skipped 清单
///（id + 字节数，R6.1 可识别）；保留条目重排进 items（严格 ≤ 预算）。
/// 返回 (保留条目, 跳过清单, 是否发生截断)。
pub fn apply_byte_budget(
    items: Vec<serde_json::Value>,
    byte_budget: usize,
) -> (Vec<serde_json::Value>, Vec<serde_json::Value>, bool) {
    let mut estimated = 2usize; // "[]"
    let mut skipped = Vec::new();
    let mut kept = Vec::new();
    let mut truncated = false;
    for item in items {
        let sz = serde_json::to_string(&item).map(|s| s.len() + 1).unwrap_or(usize::MAX);
        if estimated + sz <= byte_budget {
            estimated += sz;
            kept.push(item);
        } else {
            truncated = true;
            skipped.push(serde_json::json!({
                "id": item["id"],
                "bytes": sz,
                "note": "条目超 bytes 预算被跳过——mem show <id> 单条读取",
            }));
        }
    }
    (kept, skipped, truncated)
}

/// r12 B7-3：show 正文分页切片单源（CLI/MCP 共用）——offset 为字节偏移（resume
/// 的 body_offset 原样回传），返回 (本页文本, 是否截断, 下一偏移)。
/// 字节切点可能落在 UTF-8 序列中间 → lossy（与 CLI 既有行为一致）。
pub fn body_slice(body: &[u8], offset: usize, byte_budget: usize) -> (String, bool, Option<usize>) {
    let off = offset.min(body.len());
    let rest = &body[off..];
    let take = rest.len().min(byte_budget);
    let truncated = rest.len() > take;
    let next = if truncated { Some(off + take) } else { None };
    (String::from_utf8_lossy(&rest[..take]).to_string(), truncated, next)
}

/// 裁定有效 limit：clamp 到 [1, list_limit_max]；None → 默认 list_limit。
pub fn effective_limit(store: &Store, requested: Option<usize>) -> usize {
    let cfg = store.config().budgets;
    let max = cfg.list_limit_max as usize;
    match requested {
        None => (cfg.list_limit as usize).clamp(1, max),
        Some(r) => r.clamp(1, max),
    }
}

/// trace 遍历预算（深度/节点上限）。
pub fn trace_budget(store: &Store) -> (usize, usize) {
    let cfg = store.config().budgets;
    (cfg.trace_depth as usize, cfg.trace_nodes as usize)
}

/// brief 预算（条数 / 字节）。
pub fn brief_budget(store: &Store) -> (usize, usize) {
    let cfg = store.config().budgets;
    (cfg.brief_items as usize, cfg.brief_bytes as usize)
}

/// 分页原语：返回 (页, 截断, 续读 offset)。
pub fn paginate<T>(all: Vec<T>, offset: usize, limit: usize) -> (Vec<T>, bool, Option<usize>) {
    let total = all.len();
    let page: Vec<T> = all.into_iter().skip(offset).take(limit).collect();
    let taken = page.len();
    let remaining = total.saturating_sub(offset + taken);
    (page, remaining > 0, if remaining > 0 { Some(offset + taken) } else { None })
}
