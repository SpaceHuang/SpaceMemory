//! 漂移扫描 current_drift 与 diff 契约（D12 / B3 / 红线26）。
//!
//! `diff --since N` = 两部分、语义分开呈现：
//! - **A. changes_since_N**：seq > N 的事件（日志过滤，按记录去重附变更史）；
//! - **B. current_drift**：枚举域同回填（当前集合 ∪ 日志历史集合）——当前状态 ≠
//!   最后事件状态者（含 location 变化与路径变化/移动）。absent 已登记（最后事件即
//!   absent）→ 不报告（稳定态，与回填收敛一致）。
//!
//! current_drift **不受游标过滤**（尚无序号）、独立命名、独立分页预算，不混入
//! changes_since_N；写例程回填后获得序号转入 A，从此不再出现。与 git 完全无关。
//! 只读存储下 B 项标注"只读无法登记，将持续出现直至可写回填"。
//! 全程内存、只读安全（读路径零写盘，红线18）。

use crate::error::MemResult;
use crate::event::{self, Event, EventLog, Location};
use crate::store::Store;
use crate::token::Token;

#[derive(Debug, Clone)]
pub struct DriftItem {
    pub id: String,
    /// 最后已知路径（ID↔路径映射）。
    pub last_path: String,
    /// 当前实际路径（absent = None）。
    pub cur_path: Option<String>,
    pub last: event::StateTriple,
    pub cur: event::StateTriple,
    /// 人读标注："backfill 待执行" / "记录缺失（external_delete 待登记）"。
    pub note: String,
    /// 当前令牌（absent 项无令牌）。
    pub token: Option<Token>,
    /// 文件 mtime（信息性，RFC 3339）。
    pub mtime: Option<String>,
    /// 只读存储标注。
    pub readonly_note: Option<String>,
}

#[derive(Debug, Default)]
pub struct DriftScan {
    pub items: Vec<DriftItem>,
    pub truncated: bool,
}

/// 漂移扫描（内存、只读安全；枚举域 = 当前记录集合 ∪ 日志历史集合）。
pub fn scan_current_drift(store: &Store, limit: usize) -> MemResult<DriftScan> {
    let log = EventLog::new(store).read_log()?;
    let candidates = event::enumerate_candidates(store, &log)?;
    let readonly_note = if store.writable() {
        None
    } else {
        Some("只读存储无法登记，将持续出现直至可写回填".to_string())
    };
    let mut items = Vec::new();
    for c in candidates {
        let last = log.events.iter().rev().find(|e| e.id == c.id);
        let last_state = last.map(event::triple_from_event);
        let last_path = last.map(|e| e.path.clone()).unwrap_or_else(|| c.lock_rel.clone());
        // 损坏记录：verify 域，不进漂移（R6.2 隔离）。
        let cur = match event::current_state_for_id(store, &c.lock_rel, Some(&c.id)) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let cur_path = event::current_path_for_id(store, &c.lock_rel, &c.id)?;
        // 稳定态判定与回填一致：absent==absent 不比路径；present 路径变化 = 漂移。
        let equal = match &last_state {
            None => cur == event::StateTriple::ABSENT,
            Some(l) if *l == event::StateTriple::ABSENT => cur == event::StateTriple::ABSENT,
            Some(l) => *l == cur && last_path == cur_path.clone().unwrap_or_default(),
        };
        if equal {
            continue; // 稳定态（含 absent 已登记 → 不报告）
        }
        let _note = match (&cur, &last_state) {
            (cstate, Some(lstate))
                if cstate.location == Location::Records
                    && lstate.location == Location::Records
                    && cur_path.as_deref() != Some(last_path.as_str()) =>
            {
                "路径变化（backfill 待执行）"
            }
            (cstate, _) if cstate.location == Location::Absent => {
                "记录缺失（external_delete 待登记）"
            }
            _ => "未登记变更（backfill 待执行）",
        };
        let token = match (&cur.version, &cur.digest) {
            (Some(v), Some(d)) => Some(Token { version: *v, digest: d.clone() }),
            _ => None,
        };
        let mtime = cur_path
            .as_ref()
            .and_then(|p| std::fs::metadata(store.abs(p)).ok())
            .and_then(|m| m.modified().ok())
            .map(crate::clock::format_rfc3339);
        let note = match (&cur, &last_state) {
            // L6：location 变化（records↔attic）是位置语义，不是普通未登记变更。
            (cstate, Some(lstate)) if cstate.location != lstate.location => {
                "位置变化（backfill 待执行）"
            }
            (cstate, Some(lstate))
                if cstate.location == Location::Records
                    && lstate.location == Location::Records
                    && cur_path.as_deref() != Some(last_path.as_str()) =>
            {
                "路径变化（backfill 待执行）"
            }
            (cstate, _) if cstate.location == Location::Absent => {
                "记录缺失（external_delete 待登记）"
            }
            _ => "未登记变更（backfill 待执行）",
        };
        items.push(DriftItem {
            id: c.id,
            last_path,
            cur_path,
            last: last_state.unwrap_or(event::StateTriple::ABSENT),
            cur,
            note: note.to_string(),
            token,
            mtime,
            readonly_note: readonly_note.clone(),
        });
    }
    // M10：先全量收集再截断——恰好等于 limit 不误报 truncated（与 diff_since 对齐）。
    let truncated = items.len() > limit;
    items.truncate(limit);
    Ok(DriftScan { items, truncated })
}

/// 按记录去重的变更史。
#[derive(Debug, Clone)]
pub struct RecordChanges {
    pub id: String,
    pub last_event: Event,
    pub history: Vec<Event>,
}

#[derive(Debug)]
pub struct DiffResult {
    /// A. changes_since_N（日志过滤，按记录去重附变更史）。
    pub changes_since: Vec<RecordChanges>,
    pub changes_truncated: bool,
    /// B. current_drift（独立命名、独立预算、不受游标过滤）。
    pub current_drift: DriftScan,
}

/// `diff --since N` 契约（红线26）：A 与 B 分开呈现，互不混入。
pub fn diff_since(
    store: &Store,
    since: u64,
    changes_limit: usize,
    drift_limit: usize,
) -> MemResult<DiffResult> {
    let events = EventLog::new(store).read_log()?.events;
    let mut groups: std::collections::BTreeMap<String, Vec<Event>> = Default::default();
    let mut changes_truncated = false;
    let mut n = 0usize;
    for ev in events.into_iter().filter(|e| e.seq > since) {
        if n >= changes_limit {
            changes_truncated = true;
            break;
        }
        groups.entry(ev.id.clone()).or_default().push(ev);
        n += 1;
    }
    let changes_since = groups
        .into_iter()
        .map(|(id, history)| {
            let last_event = history.last().cloned().expect("分组非空");
            RecordChanges { id, last_event, history }
        })
        .collect();
    let current_drift = scan_current_drift(store, drift_limit)?;
    Ok(DiffResult { changes_since, changes_truncated, current_drift })
}
