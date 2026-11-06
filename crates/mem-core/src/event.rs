//! 事件日志 events.jsonl 与状态覆盖回填（D12 / 红线24）。
//!
//! 追加式、一行一事件、序号 = 行序；事件携带 `path`（ID↔路径映射）、新旧内容摘要、
//! 操作者；全库串行分配（序列锁下追加），commit-pending 不分配序号。
//!
//! 记录状态三元组：`state = (location ∈ {records, attic, absent}, version, digest)`，
//! **absent 是一等状态**。回填判据 = 最后事件"事件后状态" ≠ 锁内重查的当前状态
//! （三元组全等**且路径不变**——路径变化即移动，产生 backfill 事件更新 ID↔路径映射）；
//! 枚举域 = 当前记录集合 ∪ 事件日志历史集合（覆盖外部删除/重命名）。
//!
//! 并发纪律：回填逐记录——共享门闩 + 该记录规范路径锁（缺失记录同样可锁）→ 锁内
//! 重查 records/ 与 attic/ 复算当前状态 → 序列锁内复核最后事件与当前状态 →
//! `last != cur` 才追加；禁止锁外旧快照直接追加。链空洞由 verify 显式报告、不伪造；
//! 撕裂尾行截断，对应记录由回填覆盖。

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::{MemError, MemResult};
use crate::lock;
use crate::paths;
use crate::store::Store;
use crate::token;

pub type Seq = u64;

/// 事件操作（规范 envelope-format.md §4.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Create,
    Update,
    Link,
    Delete,
    Restore,
    ExternalDelete,
    Backfill,
    Fix,
    Merge,
}

impl fmt::Display for Op {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = serde_json::to_value(self).unwrap();
        write!(f, "{}", s.as_str().unwrap_or("?"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Op,
    Backfill,
}

/// 一行事件。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub seq: Seq,
    pub ts: String,
    pub id: String,
    /// ID↔路径映射（records/… 或 attic/…）。
    pub path: String,
    pub op: Op,
    pub old_v: Option<u64>,
    pub new_v: Option<u64>,
    pub old_digest: Option<String>,
    pub new_digest: Option<String>,
    pub by: String,
    pub kind: Kind,
}

impl Event {
    /// 规范行字节（键按字母序——经 BTreeMap 序列化）。
    pub fn to_line(&self) -> Vec<u8> {
        let v = serde_json::to_value(self).expect("事件序列化不会失败");
        let mut b = serde_json::to_vec(&v).expect("事件序列化不会失败");
        b.push(b'\n');
        b
    }
}

// ---------- 状态三元组 ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Location {
    Records,
    Attic,
    Absent,
}

/// 记录状态三元组（absent 是一等状态，B1）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateTriple {
    pub location: Location,
    pub version: Option<u64>,
    pub digest: Option<String>,
}

impl StateTriple {
    pub const ABSENT: StateTriple = StateTriple { location: Location::Absent, version: None, digest: None };
}

/// 锁内/扫描时复算当前状态三元组（records/ 与 attic/ 双查）。
///
/// 文件存在但损坏 → Err（调用方按 R6.2 隔离：跳过 + 告警）。
pub fn current_state(store: &Store, record_rel: &str) -> MemResult<StateTriple> {
    let rec_abs = store.abs(record_rel);
    if rec_abs.is_file() {
        let bytes = std::fs::read(&rec_abs).map_err(|e| MemError::io(rec_abs.display().to_string(), e))?;
        let rec = crate::record::Record::from_bytes(&bytes).map_err(|e| {
            MemError::Corrupt { path: record_rel.into(), reason: e.to_string() }
        })?;
        return Ok(StateTriple {
            location: Location::Records,
            version: Some(rec.envelope.version),
            digest: Some(token::sha256_hex(&bytes)),
        });
    }
    let attic = paths::attic_rel(record_rel)?;
    let attic_abs = store.abs(&attic);
    if attic_abs.is_file() {
        let bytes = std::fs::read(&attic_abs).map_err(|e| MemError::io(attic_abs.display().to_string(), e))?;
        let rec = crate::record::Record::from_bytes(&bytes).map_err(|e| {
            MemError::Corrupt { path: attic.into(), reason: e.to_string() }
        })?;
        return Ok(StateTriple {
            location: Location::Attic,
            version: Some(rec.envelope.version),
            digest: Some(token::sha256_hex(&bytes)),
        });
    }
    Ok(StateTriple::ABSENT)
}

/// 事件的"事件后状态"三元组。
pub fn triple_from_event(ev: &Event) -> StateTriple {
    if ev.op == Op::ExternalDelete {
        return StateTriple::ABSENT;
    }
    StateTriple {
        location: if ev.path.starts_with("attic/") { Location::Attic } else { Location::Records },
        version: ev.new_v,
        digest: ev.new_digest.clone(),
    }
}

// ---------- 日志读取 ----------

#[derive(Debug, Default)]
pub struct ReadLog {
    pub events: Vec<Event>,
    /// 末行 JSON 不可解析（崩溃撕裂/手改坏行）：读取时忽略并告警；追加例程先把
    /// 文件截断到 `good_bytes`（最后完整行界）再追加——坏行无论是否带换行都被
    /// 移除，对应记录由回填覆盖（规范 §4.2）。
    pub torn_tail: bool,
    /// 最后一个**完整合法行**之后的字节偏移（撕裂尾行修复的截断点；
    /// 全部行合法时 = 文件长度）。
    pub good_bytes: usize,
}

#[derive(Debug, Clone)]
pub struct EventLog<'a> {
    store: &'a Store,
}

impl<'a> EventLog<'a> {
    pub fn new(store: &'a Store) -> Self {
        Self { store }
    }

    fn path(&self) -> std::path::PathBuf {
        self.store.abs("events.jsonl")
    }

    /// 全量读取（撕裂尾行截断 + 告警；中部坏行 = 结构损坏，响亮错误）。
    pub fn read_log(&self) -> MemResult<ReadLog> {
        let p = self.path();
        let bytes = match std::fs::read(&p) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ReadLog { events: Vec::new(), torn_tail: false, good_bytes: 0 })
            }
            Err(e) => return Err(MemError::io(p.display().to_string(), e)),
        };
        let text = String::from_utf8(bytes.clone()).map_err(|e| {
            MemError::Corrupt { path: "events.jsonl".into(), reason: format!("非 UTF-8：{e}") }
        })?;
        let mut events = Vec::new();
        let mut torn_tail = false;
        let mut good_bytes = 0usize;
        let mut offset = 0usize;
        // 行号对 text.lines()（尾行可无换行）；字节偏移对原始 bytes（修复截断点）。
        let lines: Vec<&str> = text.lines().collect();
        let last = lines.len().saturating_sub(1);
        for (i, line) in lines.iter().enumerate() {
            let line_bytes = line.as_bytes().len();
            let has_newline = offset + line_bytes < bytes.len();
            let line_end = offset + line_bytes + if has_newline { 1 } else { 0 };
            if line.trim().is_empty() {
                offset = line_end;
                if i < last || has_newline {
                    good_bytes = good_bytes.max(offset);
                }
                continue;
            }
            match serde_json::from_str::<Event>(line) {
                Ok(ev) => {
                    events.push(ev);
                    offset = line_end;
                    good_bytes = line_end;
                }
                Err(e) => {
                    if i == last {
                        torn_tail = true; // 末行撕裂：截断语义（修复点 = good_bytes）
                        break;
                    } else {
                        return Err(MemError::Corrupt {
                            path: "events.jsonl".into(),
                            reason: format!("第 {} 行不是合法事件（中部损坏，响亮报告）：{e}", i + 1),
                        });
                    }
                }
            }
        }
        Ok(ReadLog { events, torn_tail, good_bytes })
    }

    pub fn max_seq(&self) -> MemResult<Seq> {
        Ok(self.read_log()?.events.last().map(|e| e.seq).unwrap_or(0))
    }

    pub fn last_event_for(&self, id: &str) -> MemResult<Option<Event>> {
        Ok(self
            .read_log()?
            .events
            .into_iter()
            .rev()
            .find(|e| e.id == id))
    }

    /// 普通写事件追加（序列锁内分配序号；撕裂尾行先补行界）。
    #[allow(clippy::too_many_arguments)]
    pub fn append_simple(
        &self,
        id: &str,
        path: &str,
        op: Op,
        old_v: Option<u64>,
        new_v: Option<u64>,
        old_digest: Option<&str>,
        new_digest: Option<&str>,
        by: &str,
    ) -> MemResult<Event> {
        let kind = if op == Op::Backfill { Kind::Backfill } else { Kind::Op };
        self.append_raw(
            id,
            path,
            op,
            kind,
            old_v,
            new_v,
            old_digest.map(|s| s.to_string()),
            new_digest.map(|s| s.to_string()),
            by,
        )
    }

    /// 任意事件追加（kind 显式；回填用）。
    #[allow(clippy::too_many_arguments)]
    pub fn append_raw(
        &self,
        id: &str,
        path: &str,
        op: Op,
        kind: Kind,
        old_v: Option<u64>,
        new_v: Option<u64>,
        old_digest: Option<String>,
        new_digest: Option<String>,
        by: &str,
    ) -> MemResult<Event> {
        lock::with_sequence_lock(self.store, || {
            let read = self.read_log()?;
            let mut ev = Event {
                seq: read.events.last().map(|e| e.seq).unwrap_or(0) + 1,
                ts: crate::clock::now_rfc3339(),
                id: id.to_string(),
                path: path.to_string(),
                op,
                old_v,
                new_v,
                old_digest,
                new_digest,
                by: by.to_string(),
                kind,
            };
            if ev.seq == 0 {
                ev.seq = 1;
            }
            let line = ev.to_line();
            use std::io::Write;
            let p = self.path();
            if read.torn_tail {
                // 撕裂尾行修复：截断到最后行界（坏行由回填覆盖，规范 §4.2），
                // 再追加新事件——中部坏行保持"响亮错误"语义不被稀释。
                repair_torn_tail(&p, &read)?;
            }
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&p)
                .map_err(|e| MemError::io(p.display().to_string(), e))?;
            f.write_all(&line).map_err(|e| MemError::io(p.display().to_string(), e))?;
            f.sync_all().map_err(|e| MemError::io(p.display().to_string(), e))?;
            Ok(ev)
        })?
    }
}

/// 撕裂尾行修复：把文件截断到**最后一个完整合法行界**（`good_bytes`）再由调用方
/// 追加——坏行无论是否自带换行都被移除（序列锁内调用；规范 §4.2）。
fn repair_torn_tail(p: &std::path::Path, read: &ReadLog) -> MemResult<()> {
    if !read.torn_tail {
        return Ok(());
    }
    let bytes = std::fs::read(p).map_err(|e| MemError::io(p.display().to_string(), e))?;
    let cut = read.good_bytes.min(bytes.len());
    if cut < bytes.len() {
        std::fs::write(p, &bytes[..cut])
            .map_err(|e| MemError::io(p.display().to_string(), e))?;
    }
    Ok(())
}

// ---------- 枚举 ----------

/// 递归枚举 store 子目录下全部 .md 文件（records/ 或 attic/）。
pub fn enumerate_md(store: &Store, prefix: &str) -> MemResult<Vec<String>> {
    let mut out = Vec::new();
    let base = store.abs(prefix);
    if !base.is_dir() {
        return Ok(out);
    }
    fn walk(dir: &std::path::Path, rel_base: &str, out: &mut Vec<String>) -> MemResult<()> {
        for entry in std::fs::read_dir(dir).map_err(|e| MemError::io(dir.display().to_string(), e))? {
            let entry = entry.map_err(|e| MemError::io(dir.display().to_string(), e))?;
            let name = entry.file_name().to_string_lossy().to_string();
            let p = entry.path();
            if p.is_dir() {
                walk(&p, &format!("{rel_base}/{name}"), out)?;
            } else if name.ends_with(".md") && !name.ends_with(".lock") {
                out.push(format!("{rel_base}/{name}"));
            }
        }
        Ok(())
    }
    walk(&base, prefix, &mut out)?;
    out.sort();
    Ok(out)
}

/// 当前记录集合 ∪ 事件日志历史集合 的候选（id → 当前路径或最后已知路径）。
pub struct Candidate {
    pub id: String,
    /// 当前实际所在（records/… 或 attic/…）；absent 时为 None。
    pub current_path: Option<String>,
    /// 锁定用规范路径（恒为 records/ 侧——同规范路径锁，红线24）。
    pub lock_rel: String,
}

pub fn enumerate_candidates(store: &Store, log: &ReadLog) -> MemResult<Vec<Candidate>> {
    // id → (当前路径, 最后事件 path)
    let mut by_id: std::collections::BTreeMap<String, (Option<String>, Option<String>)> =
        Default::default();
    for rel in enumerate_md(store, "records")? {
        let abs = store.abs(&rel);
        let bytes = std::fs::read(&abs).map_err(|e| MemError::io(abs.display().to_string(), e))?;
        // 坏文件的 id 不可得 → 无法回填（verify 报告）；此处跳过并告警由调用方汇总。
        if let Ok(rec) = crate::record::Record::from_bytes(&bytes) {
            by_id.entry(rec.envelope.id).or_insert((Some(rel), None));
        }
    }
    for rel in enumerate_md(store, "attic")? {
        let abs = store.abs(&rel);
        let bytes = std::fs::read(&abs).map_err(|e| MemError::io(abs.display().to_string(), e))?;
        if let Ok(rec) = crate::record::Record::from_bytes(&bytes) {
            let e = by_id.entry(rec.envelope.id).or_insert((None, None));
            if e.0.is_none() {
                e.0 = Some(rel);
            }
        }
    }
    for ev in &log.events {
        let e = by_id.entry(ev.id.clone()).or_insert((None, None));
        if e.1.is_none() {
            e.1 = Some(ev.path.clone());
        }
    }
    let mut out = Vec::new();
    for (id, (cur, last_path)) in by_id {
        // 锁路径：当前 records 路径；attic → records 侧；absent → 最后事件 path 的 records 侧。
        let lock_rel = match &cur {
            Some(p) if p.starts_with("records/") => p.clone(),
            Some(p) => paths::records_rel(p)?,
            None => {
                let lp = last_path.clone().ok_or_else(|| MemError::Corrupt {
                    path: "events.jsonl".into(),
                    reason: format!("候选 {id} 既无文件也无事件"),
                })?;
                if lp.starts_with("attic/") {
                    paths::records_rel(&lp)?
                } else {
                    lp
                }
            }
        };
        out.push(Candidate { id, current_path: cur, lock_rel });
    }
    Ok(out)
}

// ---------- 统一回填算法 ----------

#[derive(Debug, Default)]
pub struct BackfillReport {
    pub appended: Vec<Event>,
    pub skipped_stable: usize,
    pub warnings: Vec<String>,
}

/// 回填判定过程中的可控暂停点（验收用：比较后暂停 → 普通 update 完成 → 恢复回填
/// 不得追加过时事件）。在记录锁内、进入序列锁之前调用。
pub type PauseHook = Box<dyn Fn(&str) + Send + Sync>;

/// 状态覆盖回填（写例程；红线24）。
pub fn reconcile_backfill(
    store: &Store,
    by: &str,
    pause_hook: Option<PauseHook>,
) -> MemResult<BackfillReport> {
    store.check_writable()?;
    let mut report = BackfillReport::default();
    let log = EventLog::new(store).read_log()?;
    let candidates = enumerate_candidates(store, &log)?;
    for c in candidates {
        // 共享门闩 + 记录锁（缺失记录同规范路径锁）。
        let _barrier = lock::barrier_shared(store)?;
        let _rec_lock = lock::record_lock(store, &c.lock_rel, std::time::Duration::from_secs(30))?;
        if let Some(hook) = &pause_hook {
            hook(&c.id);
        }
        let appended = with_sequence_recheck(store, &c, by, &mut report.warnings)?;
        match appended {
            Some(ev) => report.appended.push(ev),
            None => report.skipped_stable += 1,
        }
        // 记录锁与门闩逐记录取放。
    }
    Ok(report)
}

/// 序列锁内复核：重读最后事件 + 重算当前状态，二者不等才追加（禁止锁外旧快照）。
#[allow(clippy::too_many_arguments)]
fn with_sequence_recheck(
    store: &Store,
    c: &Candidate,
    by: &str,
    warnings: &mut Vec<String>,
) -> MemResult<Option<Event>> {
    lock::with_sequence_lock(store, || {
        let log = EventLog::new(store).read_log()?;
        let last = log.events.iter().rev().find(|e| e.id == c.id).cloned();
        // 锁内重查当前状态（含"是否已被重建/恢复"复查）与当前路径。
        let cur = current_state_quiet(store, c, warnings)?;
        let cur_path = current_path_of(store, c)?;
        let last_state = last.as_ref().map(triple_from_event);
        let last_path = last.as_ref().map(|e| e.path.clone());

        // 稳定态判定：最后事件状态 == 当前状态（三元组全等）且路径不变。
        // absent == absent 是稳定态——absent 无"当前路径"可言，不比路径（B1）；
        // present 侧路径变化 = 移动，必须回填（更新 ID↔路径映射）。
        let equal = match &last_state {
            None => cur == crate::event::StateTriple::ABSENT,
            Some(l) if *l == crate::event::StateTriple::ABSENT => {
                cur == crate::event::StateTriple::ABSENT
            }
            Some(l) => *l == cur && last_path.as_deref() == cur_path.as_deref(),
        };
        if equal {
            return Ok(None); // 稳定态（含 absent==absent）
        }
        let ev = match cur.location {
            Location::Absent => Event {
                seq: 0,
                ts: crate::clock::now_rfc3339(),
                id: c.id.clone(),
                path: last_path.unwrap_or_else(|| c.lock_rel.clone()),
                op: Op::ExternalDelete,
                old_v: last_state.as_ref().and_then(|s| s.version),
                new_v: None,
                old_digest: last_state.as_ref().and_then(|s| s.digest.clone()),
                new_digest: None,
                by: by.to_string(),
                kind: Kind::Backfill,
            },
            _ => Event {
                seq: 0,
                ts: crate::clock::now_rfc3339(),
                id: c.id.clone(),
                path: cur_path.unwrap_or_else(|| c.lock_rel.clone()),
                op: Op::Backfill,
                old_v: last_state.as_ref().and_then(|s| s.version),
                new_v: cur.version,
                old_digest: last_state.as_ref().and_then(|s| s.digest.clone()),
                new_digest: cur.digest.clone(),
                by: by.to_string(),
                kind: Kind::Backfill,
            },
        };
        let seq = log.events.last().map(|e| e.seq).unwrap_or(0) + 1;
        let ev = Event { seq, ..ev };
        let line = ev.to_line();
        use std::io::Write;
        let p = store.abs("events.jsonl");
        if log.torn_tail {
            // 与 append_raw 同一修复纪律：截断到最后完整行界再追加（B1：reconcile
            // 路径此前只补 \n 不截断，坏行带换行即毒化为中部坏行）。
            repair_torn_tail(&p, &log)?;
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&p)
            .map_err(|e| MemError::io(p.display().to_string(), e))?;
        f.write_all(&line).map_err(|e| MemError::io(p.display().to_string(), e))?;
        f.sync_all().map_err(|e| MemError::io(p.display().to_string(), e))?;
        Ok(Some(ev))
    })?
}

fn current_path_of(store: &Store, c: &Candidate) -> MemResult<Option<String>> {
    let rec_abs = store.abs(&c.lock_rel);
    if rec_abs.is_file() {
        return Ok(Some(c.lock_rel.clone()));
    }
    let attic = paths::attic_rel(&c.lock_rel)?;
    if store.abs(&attic).is_file() {
        return Ok(Some(attic));
    }
    Ok(None)
}

/// current_state 的告警版：坏文件 → 跳过 + 告警（R6.2），视为"该路径不可用"。
fn current_state_quiet(
    store: &Store,
    c: &Candidate,
    warnings: &mut Vec<String>,
) -> MemResult<StateTriple> {
    match current_state(store, &c.lock_rel) {
        Ok(t) => Ok(t),
        Err(e) => {
            warnings.push(format!("回填跳过损坏记录 {}：{e}", c.id));
            // 损坏文件按"状态不可复算"处理：不追加事件（等 verify/fix），报告稳定。
            Ok(last_as_current(store, c))
        }
    }
}

/// 损坏记录的保守状态 = 最后事件状态（不产生事件、不伪造）。
fn last_as_current(store: &Store, c: &Candidate) -> StateTriple {
    let log = EventLog::new(store).read_log();
    match log {
        Ok(l) => l
            .events
            .iter()
            .rev()
            .find(|e| e.id == c.id)
            .map(triple_from_event)
            .unwrap_or(StateTriple::ABSENT),
        Err(_) => StateTriple::ABSENT,
    }
}

// ---------- 链连续性审计（verify 用；只报告不补造） ----------

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ChainHole {
    pub id: String,
    pub at_seq: Seq,
    pub reason: String,
}

/// 非 backfill 事件的 old→new 版本链审计：断裂 → 显式报告历史空洞。
pub fn chain_audit(events: &[Event]) -> Vec<ChainHole> {
    let mut holes = Vec::new();
    let mut expected: std::collections::HashMap<String, u64> = Default::default();
    let mut started: std::collections::HashSet<String> = Default::default();
    for ev in events {
        if ev.kind == Kind::Backfill {
            continue; // backfill 显式标注，不参与链（历史不可完整重建时不伪造）
        }
        match ev.op {
            Op::Create => {
                started.insert(ev.id.clone());
                expected.insert(ev.id.clone(), ev.new_v.unwrap_or(1));
            }
            _ => {
                if !started.contains(&ev.id) {
                    holes.push(ChainHole {
                        id: ev.id.clone(),
                        at_seq: ev.seq,
                        reason: "历史起点缺失（首个事件非 create；v1 创建事件缺失）".into(),
                    });
                    started.insert(ev.id.clone());
                    expected.insert(ev.id.clone(), ev.new_v.unwrap_or(1));
                } else {
                    let exp = expected.get(&ev.id).copied();
                    match exp {
                        Some(e) if ev.old_v != Some(e) => {
                            holes.push(ChainHole {
                                id: ev.id.clone(),
                                at_seq: ev.seq,
                                reason: format!(
                                    "版本链断裂：期望 old_v={e}，实际 old_v={:?}（中间空洞/伪造）",
                                    ev.old_v
                                ),
                            });
                        }
                        _ => {}
                    }
                    if let Some(nv) = ev.new_v {
                        expected.insert(ev.id.clone(), nv);
                    }
                }
            }
        }
    }
    holes
}
