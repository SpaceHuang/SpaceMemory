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

// R11.2：`body_slice`（字节切 + 有损解码的单一文本通道）已废除——正文分页收敛
// 为 mem_core::body 双通道单源：`body_page_text`（合法 UTF-8，切点码点对齐）与
// `body_page_bytes`（任意字节，CLI 落文件 / MCP base64）。四个有损点位中的
// 正文分页点位由此收口（binary-content-design §1 表第 1 行）。

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
