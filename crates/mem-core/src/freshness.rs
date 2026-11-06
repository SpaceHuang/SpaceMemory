//! 证明式新鲜性协议（D5 / 红线21）。
//!
//! 索引服务读请求，当且仅当其新鲜性**可证明**：
//! - (a) stat 一致：records/ 目录枚举（路径+mtime+size）与索引检查点一致；
//! - (b) 变更覆盖无缺口：自检查点时刻起存在覆盖该区间的 OS 变更日志订阅
//!   （NTFS USN journal / macOS FSEvents，由 sidecar 常驻维护），且无未消化的
//!   本店路径修改事件——内容级修改即使保 mtime、同尺寸也被捕获。
//!
//! (a)∧(b) → 快路径；任一不成立（sidecar 不在/嵌入模式、卷不支持、stat 失配、只读）
//! → **直读真相层应答**（内存扫描，标注"直读模式"；FTS 仅快路径可用）。
//!
//! 不承认任何"已知可能返回旧值"的快路径；同尺寸保 mtime 盲区结构性消除。
//! 读路径零写盘（红线18）——判定与直读全在内存。直读成本如实为 O(库规模)，
//! 记入开放问题29 性能基线。

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{MemError, MemResult};
use crate::store::Store;

/// 索引检查点（由 sidecar/写例程持久化在 index/ 下；读路径只读它）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Checkpoint {
    /// 检查点时刻（RFC 3339 带毫秒——内部派生物，需要亚秒单调性；
    /// 记录/事件层的规范时间戳仍是秒级，见 envelope-format §2.2/§4.1）。
    pub ts: String,
    /// records/ 枚举快照：rel → (mtime_epoch_nanos, size)。
    pub entries: std::collections::BTreeMap<String, (u128, u64)>,
    /// r4 B2：发布该检查点时的索引物化代际（mem-index meta.epoch）。
    /// 读路径核对一致才认定"检查点与索引同代"；索引被删除重建（epoch 重置）
    /// 而检查点记旧代 → 直读，空/旧索引不会被误信。
    #[serde(default)]
    pub index_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadMode {
    /// 快路径：新鲜性可证明（stat 一致 ∧ 订阅覆盖无缺口）。
    Indexed { via_subscription: String },
    /// 直读真相层（内存、零写盘）：reason 说明为何不可证明。
    Direct { reason: String },
}

impl ReadMode {
    pub fn is_direct(&self) -> bool {
        matches!(self, ReadMode::Direct { .. })
    }
    /// 直读模式下 FTS 不可用（标注面）。
    pub fn fts_available(&self) -> bool {
        matches!(self, ReadMode::Indexed { .. })
    }
}

/// 变更日志订阅的最小接口（D5）。
///
/// sidecar（§8-6）提供 USN/FSEvents 实现；嵌入模式为 None。
/// `undigested_changes_since` 返回：None = 订阅不存在/无覆盖（不可证明）；
/// Some(n) = 自 checkpoint 以来未消化的本店路径修改事件数（n>0 也不可证明——
/// 说明索引落后于真相）。
pub trait Subscription: Send + Sync {
    fn undigested_changes_since(&self, checkpoint_ts: &str) -> Option<u64>;
    fn backend(&self) -> &'static str;
}

/// 嵌入模式的空订阅：永远不可证明 → 永久直读。
pub struct NullSubscription;

impl Subscription for NullSubscription {
    fn undigested_changes_since(&self, _checkpoint_ts: &str) -> Option<u64> {
        None
    }
    fn backend(&self) -> &'static str {
        "none"
    }
}

/// stat 快照（内存；比较用）。
pub fn stat_snapshot(store: &Store) -> std::collections::BTreeMap<String, (u128, u64)> {
    let mut out = std::collections::BTreeMap::new();
    for rel in crate::event::enumerate_md(store, "records").unwrap_or_default() {
        if let Ok(md) = std::fs::metadata(store.abs(&rel)) {
            let mtime = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            out.insert(rel, (mtime, md.len()));
        }
    }
    out
}

/// 新鲜性证明判定（读路径；内存比较，零写盘）。
pub fn consult(
    store: &Store,
    checkpoint: Option<&Checkpoint>,
    subscription: Option<&dyn Subscription>,
) -> ReadMode {
    // 只读存储：索引维护例程不可写 → 检查点无法推进 → 不可证明（D5）。
    if !store.writable() {
        return ReadMode::Direct { reason: "只读存储（检查点不可推进）".into() };
    }
    let Some(cp) = checkpoint else {
        return ReadMode::Direct { reason: "无索引检查点（嵌入模式/冷启动）".into() };
    };
    let Some(sub) = subscription else {
        return ReadMode::Direct { reason: "sidecar 不在（嵌入模式 = 直读 + 无 FTS）".into() };
    };
    // (a) stat 一致。
    let now = stat_snapshot(store);
    if now != cp.entries {
        return ReadMode::Direct { reason: "stat 失配（records/ 与检查点不一致）".into() };
    }
    // (b) 订阅覆盖无缺口。
    match sub.undigested_changes_since(&cp.ts) {
        None => ReadMode::Direct { reason: "变更日志订阅不存在/不覆盖该区间".into() },
        Some(0) => ReadMode::Indexed { via_subscription: sub.backend().to_string() },
        Some(n) => ReadMode::Direct {
            reason: format!("订阅存在 {n} 条未消化变更（索引落后于真相）"),
        },
    }
}

/// 检查点文件路径（index/checkpoint.json；可丢弃派生物）。
pub fn checkpoint_path(store: &Store) -> std::path::PathBuf {
    store.abs("index/checkpoint.json")
}

pub fn load_checkpoint(store: &Store) -> Option<Checkpoint> {
    let p = checkpoint_path(store);
    let bytes = std::fs::read(&p).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// 持久化检查点（**只发生在写例程/sidecar 维护**，不在读路径）。
pub fn save_checkpoint(store: &Store, cp: &Checkpoint) -> std::io::Result<()> {
    let p = checkpoint_path(store);
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let bytes = serde_json::to_vec_pretty(cp)?;
    crate::platform::atomic_write(&p, &bytes)
}

/// B3 接线的证明判定：订阅状态经 **sidecar 握手**（Hello.undigested）远端取得。
///
/// (a) stat 一致 ∧ (b) Hello.undigested == Some(0)（订阅在场且无未消化变更）→ 快路径；
/// 任一不成立 → 直读（reason 如实标注）。读路径零写盘（红线18）。
pub fn consult_with_hello(
    store: &Store,
    checkpoint: Option<&Checkpoint>,
    hello: Option<&crate::sidecar::Hello>,
) -> ReadMode {
    if !store.writable() {
        return ReadMode::Direct { reason: "只读存储（检查点不可推进）".into() };
    }
    let Some(cp) = checkpoint else {
        return ReadMode::Direct { reason: "无索引检查点（sidecar 从未维护过）".into() };
    };
    let Some(h) = hello else {
        return ReadMode::Direct { reason: "sidecar 不在（嵌入模式 = 直读 + 无 FTS）".into() };
    };
    if h.backend == "none" {
        return ReadMode::Direct { reason: "卷不支持变更日志订阅（永久直读形态）".into() };
    }
    if h.undigested != Some(0) {
        return ReadMode::Direct {
            reason: format!("订阅存在未消化变更（{:?}）——索引落后于真相", h.undigested),
        };
    }
    let now = stat_snapshot(store);
    if now != cp.entries {
        return ReadMode::Direct { reason: "stat 失配（records/ 与检查点不一致）".into() };
    }
    ReadMode::Indexed { via_subscription: h.backend.clone() }
}

/// **统一维护收敛原语**（r4 B3/H2）：执行维护（物化索引）→ 新鲜查询确认收敛 →
/// 才发布检查点。CLI reconcile / MCP reconcile / sidecar 维护**必须共用本函数**，
/// 不得各自实现"物化与检查点"的顺序——任何其他顺序都允许"维护期间二次写入被
/// 检查点提前确认"（红线21 立法场景）。
///
/// 收敛语义：每轮维护后做一次**新鲜订阅查询**（不是用轮询缓存）；为 0 才发布；
/// >0 继续下一轮（维护吸收维护期间的新写入）；有界 4 轮后仍未收敛 → **不发布**
/// （返回 None，读路径直读——诚实降级优于错误证明）。
/// `fresh == None`（无订阅可查，如脱离 sidecar 的 reconcile）→ 只维护、**不发布**
/// 检查点——无证据不得签发证明。
///
/// 返回：`Some(cp)` = 已收敛并发布；`None` = 未收敛/无证据（索引可能已更新，
/// 但检查点保持不变）。
/// 维护结果（r5 B3）：物化代际 + **与索引同一次扫描**的文件 stat 集。
pub struct MaintainOutcome {
    pub index_epoch: u64,
    pub scan_entries: std::collections::BTreeMap<String, (u128, u64)>,
}

pub fn publish_converged_checkpoint(
    store: &Store,
    maintain: &dyn Fn(&Store) -> MemResult<MaintainOutcome>,
    mut fresh: Option<&mut dyn FnMut() -> Option<u64>>,
) -> MemResult<Option<Checkpoint>> {
    for _round in 0..4 {
        let outcome = maintain(store)?;
        match fresh.as_mut() {
            Some(f) => match f() {
                Some(0) => {
                    // 收敛：索引包含截至本轮新鲜查询的全部已消化事件。
                    // r5 B3：检查点 entries 采自**与索引同一次扫描**的文件集
                    //（outcome.scan_entries）——绝不在此刻重新枚举磁盘。
                    // "最后 fresh 查询之后"的写入：新文件不在 entries（实时
                    // 枚举失配 → 直读）；改既有文件 → 订阅事件 → 直读/再维护。
                    let cp = Checkpoint {
                        ts: crate::clock::format_rfc3339_millis(std::time::SystemTime::now()),
                        entries: outcome.scan_entries,
                        index_epoch: outcome.index_epoch,
                    };
                    save_checkpoint(store, &cp).map_err(|e| {
                        MemError::Corrupt {
                            path: "index/checkpoint.json".into(),
                            reason: e.to_string(),
                        }
                    })?;
                    return Ok(Some(cp));
                }
                Some(_) => continue, // 维护期间有新写入：再维护一轮
                None => return Ok(None), // 订阅失效/gap：不可证明，不发布
            },
            None => return Ok(None), // 无订阅证据：只维护不发布（H2）
        }
    }
    Ok(None) // 有界轮次内未收敛：不发布
}

/// 卷订阅能力探测（开放问题39 的 v1：NTFS → "usn"；其余 → None = 永久直读）。
pub fn probe_subscription_backend(store_root: &Path) -> Option<&'static str> {
    match crate::platform::fs_name_for(store_root) {
        Some(name) if name.eq_ignore_ascii_case("NTFS") => Some("usn"),
        _ => None,
    }
}
