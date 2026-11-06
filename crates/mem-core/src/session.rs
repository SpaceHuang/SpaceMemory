//! 会话台账：协作状态的机制侧原件（D13 / 红线14 / 红线25）。
//!
//! 创建触发器没有一个是读命令：显式建立（MCP initialize / `mem session start`）或
//! 写路径兜底；**brief 纯读，永不创建或修改台账**（红线25）。
//! 幂等键 = principal + view + task_ref；接手选择作用域 = view + task_ref，
//! 优先序：显式 id > 同 view 且 task_ref 相等 > 同 view 且 task_ref 为空 >
//! **task_ref 不同永不接手**（删除跨任务回退）。活跃会话不可自动接手（peers 呈现）。
//! 接手 = 新台账 + 落盘 handoff_from + 继承水位；来源原样保留（R9.6 原件不可侵）。

use serde::{Deserialize, Serialize};

use crate::error::{MemError, MemResult};
use crate::lock;
use crate::store::Store;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Active,
    Flushed,
    Closed,
}

/// 会话台账（规范 envelope-format.md §7；机制所有）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ledger {
    pub id: String,
    pub principal: String,
    pub view: Option<String>,
    pub task_ref: Option<String>,
    pub conventions_version: u64,
    /// 事件序号水位（增量刷新游标，R4.3）。
    pub watermark: u64,
    /// trail 写通道（写路径登记）。
    pub trail: Vec<String>,
    /// 读通道增量（`session flush --trail` 显式提交）。
    pub read_trail: Vec<String>,
    pub handoff_from: Option<String>,
    pub status: Status,
    pub created: String,
    pub last_write: String,
}

#[derive(Debug, Clone)]
pub enum HandoffSpec {
    None,
    Auto,
    Explicit(String),
}

#[derive(Debug, Clone)]
pub struct StartArgs {
    pub principal: String,
    pub view: Option<String>,
    pub task_ref: Option<String>,
    pub handoff: HandoffSpec,
    /// 台账空闲阈值（活跃判定；mem.json session_idle_secs）。
    pub idle_secs: u64,
}

#[derive(Debug)]
pub struct StartOutcome {
    pub ledger: Ledger,
    pub resumed_from: Option<Ledger>,
    /// 同 view 的活跃（不可接手）会话摘要。
    pub peers: Vec<Ledger>,
    /// 幂等命中（同键既有台账）。
    pub idempotent: bool,
}

fn ledgers_dir(store: &Store) -> std::path::PathBuf {
    store.abs("sessions")
}

// M6b 归档裁决（v1.2 评审）：scan_ledgers 对缺失 sessions/ 目录做 mkdir——写的是
// 运行时目录骨架（锁文件/台账的宿主），非真相层数据；红线18 管辖"真相与派生缓存"
// 的内容写。避免它需要把目录保障挪进 session start（写路径）——brief 扫描空目录
// 返回空即可，此处保留 mkdir 仅为让"无台账"与"店未建"错误可区分，且发生在
// sessions/（台账域）内，语义安全。

/// 扫描台账（坏条目跳过 + 告警，R6.2）。
pub fn scan_ledgers(store: &Store) -> MemResult<(Vec<Ledger>, Vec<String>)> {
    let mut out = Vec::new();
    let mut warnings = Vec::new();
    let dir = ledgers_dir(store);
    if !dir.is_dir() {
        std::fs::create_dir_all(&dir).map_err(|e| MemError::io(dir.display().to_string(), e))?;
        return Ok((out, warnings));
    }
    for e in std::fs::read_dir(&dir).map_err(|e| MemError::io(dir.display().to_string(), e))? {
        let e = e.map_err(|er| MemError::io(dir.display().to_string(), er))?;
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        match std::fs::read(&p).map_err(|er| MemError::io(p.display().to_string(), er)) {
            Ok(bytes) => match serde_json::from_slice::<Ledger>(&bytes) {
                Ok(l) => out.push(l),
                Err(er) => warnings.push(format!("跳过坏台账 {}：{er}", p.display())),
            },
            Err(er) => warnings.push(format!("跳过不可读台账 {}：{er}", p.display())),
        }
    }
    out.sort_by(|a, b| a.created.cmp(&b.created).then(a.id.cmp(&b.id)));
    Ok((out, warnings))
}

pub fn get(store: &Store, session_id: &str) -> MemResult<Option<Ledger>> {
    let p = ledgers_dir(store).join(format!("{session_id}.json"));
    if !p.is_file() {
        return Ok(None);
    }
    let bytes = std::fs::read(&p).map_err(|e| MemError::io(p.display().to_string(), e))?;
    Ok(Some(serde_json::from_slice(&bytes).map_err(|e| {
        MemError::Corrupt { path: p.display().to_string(), reason: e.to_string() }
    })?))
}

fn persist(store: &Store, ledger: &Ledger) -> MemResult<()> {
    let dir = ledgers_dir(store);
    std::fs::create_dir_all(&dir).map_err(|e| MemError::io(dir.display().to_string(), e))?;
    let p = dir.join(format!("{}.json", ledger.id));
    let bytes = serde_json::to_vec_pretty(ledger).expect("台账序列化不会失败");
    crate::platform::atomic_write(&p, &bytes)
        .map_err(|e| MemError::io(p.display().to_string(), e))
}

fn new_session_id() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let days = secs / 86_400;
    let (y, m, d) = civil(days as i64);
    let rand: u32 = now.subsec_nanos() ^ (std::process::id() << 8);
    format!("session_{y:04}{m:02}{d:02}_{rand:08x}")
}

fn civil(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn parse_rfc3339_secs(s: &str) -> u64 {
    // RFC3339 UTC 秒级 → epoch（容错：解析失败按 0 = 永远"活跃不久"）。
    crate::clock::parse_rfc3339_to_epoch(s).unwrap_or(0)
}

fn is_resumable(l: &Ledger, idle_secs: u64, now_epoch: u64) -> bool {
    match l.status {
        Status::Flushed | Status::Closed => true,
        Status::Active => {
            let lw = parse_rfc3339_secs(&l.last_write);
            now_epoch.saturating_sub(lw) > idle_secs
        }
    }
}

/// 显式建立/接手（红线25）。幂等：同键活跃台账 → 返回既有。
pub fn start(store: &Store, args: StartArgs) -> MemResult<StartOutcome> {
    store.check_writable()?;
    // 幂等键锁（锁文件独立于台账存在；键哈希防路径注入）。
    let key = format!(
        "{}|{}|{}",
        args.principal,
        args.view.clone().unwrap_or_default(),
        args.task_ref.clone().unwrap_or_default()
    );
    let key_hash = crate::token::sha256_hex(key.as_bytes())[..16].to_string();
    let start_lock_rel = format!("sessions/.start-{key_hash}");
    let _slock = lock::record_lock(store, &start_lock_rel, std::time::Duration::from_secs(30))?;

    let (all, _warnings) = scan_ledgers(store)?;
    let same_key = |l: &Ledger| {
        l.principal == args.principal
            && l.view == args.view
            && l.task_ref == args.task_ref
    };
    // 1) 幂等命中。
    if let Some(active) = all
        .iter()
        .find(|l| same_key(l) && l.status == Status::Active)
    {
        return Ok(StartOutcome {
            ledger: active.clone(),
            resumed_from: None,
            peers: Vec::new(),
            idempotent: true,
        });
    }
    // 2) 接手选择。
    let now_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let mut resumed_from = None;
    let mut inherited_watermark = None;
    let mut peers = Vec::new();
    match &args.handoff {
        HandoffSpec::None => {}
        HandoffSpec::Explicit(id) => {
            let src = get(store, id)?.ok_or_else(|| MemError::MissingRecord {
                id_or_path: format!("session {id}"),
            })?;
            if src.view != args.view {
                return Err(MemError::BadArgument(format!(
                    "显式接手跨 view：来源 view={:?}，请求 view={:?}",
                    src.view, args.view
                )));
            }
            if !is_resumable(&src, args.idle_secs, now_epoch) {
                return Err(MemError::BadArgument(format!("来源会话 {id} 活跃，不可接手")));
            }
            inherited_watermark = Some(src.watermark);
            resumed_from = Some(src);
        }
        HandoffSpec::Auto => {
            // 优先序：同 view 且 task_ref 相等 > 同 view 且 task_ref 为空。
            let candidates = |tr: &Option<String>| {
                all.iter()
                    .filter(|l| l.view == args.view && &l.task_ref == tr)
                    .filter(|l| is_resumable(l, args.idle_secs, now_epoch))
                    .cloned()
                    .collect::<Vec<_>>()
            };
            let pool = candidates(&args.task_ref);
            let pool = if pool.is_empty() { candidates(&None) } else { pool };
            resumed_from = pool.last().cloned();
            if let Some(src) = &resumed_from {
                inherited_watermark = Some(src.watermark);
            }
            // 活跃同 view 会话 → peers 摘要。
            peers = all
                .iter()
                .filter(|l| l.view == args.view && l.status == Status::Active)
                .filter(|l| !is_resumable(l, args.idle_secs, now_epoch))
                .cloned()
                .collect();
        }
    }
    // 3) 新台账（接手 = 新建，原件保留）。
    let watermark =
        inherited_watermark.unwrap_or_else(|| crate::event::EventLog::new(store).max_seq().unwrap_or(0));
    let now = crate::clock::now_rfc3339();
    let ledger = Ledger {
        id: new_session_id(),
        principal: args.principal,
        view: args.view,
        task_ref: args.task_ref,
        conventions_version: read_conventions_version(store),
        watermark,
        trail: Vec::new(),
        read_trail: Vec::new(),
        handoff_from: resumed_from.as_ref().map(|l| l.id.clone()),
        status: Status::Active,
        created: now.clone(),
        last_write: now,
    };
    persist(store, &ledger)?;
    Ok(StartOutcome { ledger, resumed_from, peers, idempotent: false })
}

fn read_conventions_version(store: &Store) -> u64 {
    std::fs::read_to_string(store.abs("conventions/CONVENTIONS.md"))
        .ok()
        .and_then(|t| {
            t.lines().find_map(|l| l.trim().strip_prefix("- version:")).and_then(|v| v.trim().parse().ok())
        })
        .unwrap_or(1)
}

/// 写路径兜底（D13.1）：无台账时建立（不是读命令的触发器——只被写路径调用）。
pub fn ensure_for_write(
    store: &Store,
    principal: &str,
    view: Option<String>,
    task_ref: Option<String>,
) -> MemResult<Ledger> {
    let (all, _) = scan_ledgers(store)?;
    if let Some(active) = all.iter().find(|l| {
        l.principal == principal
            && l.view == view
            && l.task_ref == task_ref
            && l.status == Status::Active
    }) {
        return Ok(active.clone());
    }
    Ok(start(
        store,
        StartArgs {
            principal: principal.to_string(),
            view,
            task_ref,
            handoff: HandoffSpec::None,
            idle_secs: store.config().session_idle_secs,
        },
    )?
    .ledger)
}

/// 写通道登记（写路径顺带；幂等去重）。
pub fn register_trail(store: &Store, session_id: &str, record_id: &str) -> MemResult<()> {
    let rel = format!("sessions/{session_id}.json");
    let _l = lock::record_lock(store, &rel, std::time::Duration::from_secs(30))?;
    let Some(mut ledger) = get(store, session_id)? else {
        return Ok(()); // 台账不存在（如测试直写）：静默跳过——trail 是尽力登记
    };
    if !ledger.trail.iter().any(|t| t == record_id) {
        ledger.trail.push(record_id.to_string());
    }
    ledger.last_write = crate::clock::now_rfc3339();
    persist(store, &ledger)
}

/// 读通道显式提交（D14 end hook）：置 flushed 可接续；水位推进。
pub fn flush(
    store: &Store,
    session_id: &str,
    read_trail: Vec<String>,
    committer: Option<&dyn crate::ops::CommitSink>,
) -> MemResult<Ledger> {
    store.check_writable()?;
    let rel = format!("sessions/{session_id}.json");
    let _l = lock::record_lock(store, &rel, std::time::Duration::from_secs(30))?;
    let mut ledger =
        get(store, session_id)?.ok_or_else(|| MemError::MissingRecord { id_or_path: rel.clone() })?;
    for t in read_trail {
        if !ledger.read_trail.contains(&t) {
            ledger.read_trail.push(t);
        }
    }
    ledger.status = Status::Flushed;
    ledger.watermark = crate::event::EventLog::new(store).max_seq().unwrap_or(ledger.watermark);
    ledger.last_write = crate::clock::now_rfc3339();
    persist(store, &ledger)?;
    // git 策略：随 flush 合并提交（开放问题27 → v1：随 flush 提交台账文件）。
    if let Some(c) = committer {
        let _ = c.commit(&[rel, "events.jsonl".into()]);
    }
    Ok(ledger)
}

/// 显式关闭。
pub fn close(store: &Store, session_id: &str) -> MemResult<Ledger> {
    store.check_writable()?;
    let rel = format!("sessions/{session_id}.json");
    let _l = lock::record_lock(store, &rel, std::time::Duration::from_secs(30))?;
    let mut ledger =
        get(store, session_id)?.ok_or_else(|| MemError::MissingRecord { id_or_path: rel })?;
    ledger.status = Status::Closed;
    ledger.last_write = crate::clock::now_rfc3339();
    persist(store, &ledger)?;
    Ok(ledger)
}
