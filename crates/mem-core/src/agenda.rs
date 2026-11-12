//! 提案队列 agenda（D6 / R5.5 / R9.3）。
//!
//! sidecar 自主权 = 机械性动作：存疑打标（加性注记）、重复/断引用/环/未消化证据
//! 计数 → **agenda 提案队列**。语义判断由宿主侧维护 Agent 执行，落笔经确认；
//! **守护进程永不调用模型**（红线3），无监督的自动改写不可接受。
//!
//! 计划（plan）= 待执行的动作序列（merge/delete/update），apply 在独占门闩内逐
//! 记录令牌 CAS 执行；`status: executing` 持久化供 verify 检出半成品——崩溃续跑
//! 显式、经确认（D17 维护流程）。

use serde::{Deserialize, Serialize};

use crate::error::{MemError, MemResult};
use crate::ops;
use crate::store::Store;
use mem_schema::TypeSet;

// ---------- 提案（机械扫描的产物） ----------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProposalKind {
    /// 疑似重复（别名折叠碰撞；收敛动作 = merge，由人/Agent 确认）。
    Duplicate { ids: Vec<String>, key: String },
    /// 断引用（R3.4 显式报告语义；动作 = 重挂或删引用）。
    DanglingRef {
        from: String,
        rel: String,
        target: String,
    },
    /// 环（一般引用环由 agenda 报告，红线17；x-dag 类型在写路径已强制）。
    Cycle { cycle: Vec<String> },
    /// 存疑（R5.2 存疑规则机械打标；动作 = 加性注记/复核提案）。
    StaleByRule { id: String, rule: String },
    /// 未消化证据（R9.4：产物落后于证据；动作 = 增量再提炼）。
    UndigestedEvidence {
        product: String,
        evidence: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Proposal {
    pub id: String,
    pub kind: ProposalKind,
    pub note: String,
    pub created: String,
}

/// 机械性扫描（不落笔、不改记录——只产提案；D6 自主性的边界）。
pub fn scan_agenda(store: &Store, types: &TypeSet) -> MemResult<Vec<Proposal>> {
    let scan = ops::scan_all(store)?;
    let now = crate::clock::now_rfc3339();
    let mut out: Vec<Proposal> = Vec::new();
    let prop = |kind: ProposalKind, note: String, out: &mut Vec<Proposal>| {
        out.push(Proposal {
            id: format!(
                "prop-{}-{}",
                crate::token::sha256_hex(note.as_bytes())[..10].to_string(),
                out.len()
            ),
            kind,
            note,
            created: now.clone(),
        });
    };
    // 断引用。
    let ids: std::collections::HashSet<_> = scan
        .records
        .iter()
        .map(|r| r.record.envelope.id.clone())
        .collect();
    for r in &scan.records {
        for e in &r.record.envelope.refs {
            if !ids.contains(&e.target) {
                let (from, rel, target) = (
                    r.record.envelope.id.clone(),
                    e.rel.clone(),
                    e.target.clone(),
                );
                prop(
                    ProposalKind::DanglingRef {
                        from: from.clone(),
                        rel: rel.clone(),
                        target: target.clone(),
                    },
                    format!("断引用：{from} --{rel}--> {target}（重挂或删引用）"),
                    &mut out,
                );
            }
        }
    }
    // 重复（别名折叠碰撞）。
    let mut by_alias: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for r in &scan.records {
        for a in &r.record.envelope.aliases {
            by_alias
                .entry(crate::paths::fold(a))
                .or_default()
                .push(r.record.envelope.id.clone());
        }
    }
    for (key, ids) in by_alias {
        if ids.len() > 1 {
            let k = key.clone();
            prop(
                ProposalKind::Duplicate {
                    ids: ids.clone(),
                    key,
                },
                format!("别名重复（{k}）：{} —— merge 收敛（R5.4）", ids.join(", ")),
                &mut out,
            );
        }
    }
    // 环（一般引用环由 agenda 报告；红线17）。
    for c in crate::refgraph::audit_dag(store, types)? {
        let cycle = c.cycle.clone();
        prop(
            ProposalKind::Cycle { cycle: c.cycle },
            format!("引用环：{}", cycle.join(" → ")),
            &mut out,
        );
    }
    // 未消化证据（R9.4 增量再提炼提示）。
    for r in &scan.records {
        if r.record
            .envelope
            .refs
            .iter()
            .any(|e| e.rel == "derived_from")
        {
            let ev = crate::refgraph::undigested_evidence(store, &r.record.envelope.id)?;
            if !ev.is_empty() {
                let product = r.record.envelope.id.clone();
                prop(
                    ProposalKind::UndigestedEvidence {
                        product: product.clone(),
                        evidence: ev.clone(),
                    },
                    format!(
                        "产物 {} 有 {} 条未消化证据（增量再提炼）",
                        product,
                        ev.len()
                    ),
                    &mut out,
                );
            }
        }
    }
    // 存疑规则（R5.2：使用方声明 → 机械打标）。规则形态 v1：
    // {"type": "<t>", "older_than_days": N}——modified 超龄 → StaleByRule 提案。
    let rules = store.config().doubt_rules;
    for rule in &rules {
        let Some(t) = rule.get("type").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(days) = rule.get("older_than_days").and_then(|v| v.as_u64()) else {
            continue;
        };
        for r in &scan.records {
            if r.record.envelope.r#type != t {
                continue;
            }
            let age_days = age_in_days(&r.record.envelope.modified);
            if age_days > days {
                let id = r.record.envelope.id.clone();
                let rule_s = serde_json::to_string(rule).unwrap_or_default();
                prop(
                    ProposalKind::StaleByRule {
                        id: id.clone(),
                        rule: rule_s.clone(),
                    },
                    format!(
                        "存疑（规则 {rule_s}）：{id} 已 {age_days} 天未更新——行动前重验（R5.2/R5.3）"
                    ),
                    &mut out,
                );
            }
        }
    }
    Ok(out)
}

fn age_in_days(rfc3339: &str) -> u64 {
    let t = crate::clock::parse_rfc3339_to_epoch(rfc3339).unwrap_or(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    now.saturating_sub(t) / 86_400
}

/// 提案落盘（agenda/<id>.json；提案 = steps 为空的 plan，确认后可补 steps 执行）。
pub fn write_proposals(store: &Store, proposals: &[Proposal]) -> MemResult<usize> {
    store.check_writable()?;
    for p in proposals {
        let plan = Plan {
            id: p.id.clone(),
            kind: PlanKind::Proposal,
            status: PlanStatus::Draft,
            steps: Vec::new(),
            completed_steps: 0,
            in_progress: None,
            created: p.created.clone(),
        };
        save_plan(store, &plan)?;
    }
    Ok(proposals.len())
}

// ---------- 计划（可执行的动作序列） ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanKind {
    Proposal,
    Maintenance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    Draft,
    Executing,
    Done,
    Failed,
    RolledBack,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PlanStep {
    Merge {
        dup: String,
        canonical: String,
    },
    Delete {
        id: String,
        force: bool,
    },
    Update {
        id: String,
        payload: serde_json::Value,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    pub id: String,
    pub kind: PlanKind,
    pub status: PlanStatus,
    pub steps: Vec<PlanStep>,
    /// 已完成并已持久化的步骤数；旧计划反序列化时从 0 开始。
    #[serde(default)]
    pub completed_steps: usize,
    /// 动作前持久化的崩溃恢复检查点。
    #[serde(default)]
    pub in_progress: Option<StepCheckpoint>,
    pub created: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepCheckpoint {
    pub index: usize,
    pub start_seq: u64,
    pub records: Vec<CheckpointRecord>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckpointRecord {
    pub id: String,
    pub version: u64,
    pub digest: String,
}

pub fn new_plan_id() -> String {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!(
        "plan-{}-{:06x}",
        crate::clock::now_rfc3339().replace(['-', ':', 'T', 'Z'], ""),
        t & 0xFF_FFFF
    )
}

fn agenda_dir(store: &Store) -> std::path::PathBuf {
    store.abs("agenda")
}

fn plan_rel(store: &Store, id: &str) -> MemResult<String> {
    crate::paths::validate_component(id)?;
    let rel = format!("agenda/{id}.json");
    crate::paths::validate_store_relative_path(store, &rel)?;
    Ok(rel)
}

pub fn save_plan(store: &Store, plan: &Plan) -> MemResult<()> {
    store.check_writable()?;
    let rel = plan_rel(store, &plan.id)?;
    let dir = agenda_dir(store);
    std::fs::create_dir_all(&dir).map_err(|e| MemError::io(dir.display().to_string(), e))?;
    crate::paths::validate_store_relative_path(store, &rel)?;
    let p = store.abs(&rel);
    let bytes = serde_json::to_vec_pretty(plan).map_err(|e| MemError::PlanCorrupt {
        path: p.display().to_string(),
        reason: e.to_string(),
    })?;
    crate::platform::atomic_write(&p, &bytes).map_err(|e| MemError::io(p.display().to_string(), e))
}

pub fn load_plan(store: &Store, plan_id: &str) -> MemResult<Option<Plan>> {
    let rel = plan_rel(store, plan_id)?;
    let p = store.abs(&rel);
    if !p.is_file() {
        return Ok(None);
    }
    let bytes = std::fs::read(&p).map_err(|e| MemError::io(p.display().to_string(), e))?;
    let plan: Plan = serde_json::from_slice(&bytes).map_err(|e| MemError::PlanCorrupt {
        path: p.display().to_string(),
        reason: e.to_string(),
    })?;
    if plan.id != plan_id {
        return Err(MemError::PlanCorrupt {
            path: p.display().to_string(),
            reason: format!("计划内容 id {:?} 与文件 id {:?} 不匹配", plan.id, plan_id),
        });
    }
    Ok(Some(plan))
}

pub fn list_plans(store: &Store) -> MemResult<Vec<Plan>> {
    let dir = agenda_dir(store);
    crate::paths::validate_store_relative_path(store, "agenda")?;
    let mut out = Vec::new();
    if !dir.is_dir() {
        return Ok(out);
    }
    for e in std::fs::read_dir(&dir).map_err(|e| MemError::io(dir.display().to_string(), e))? {
        let e = e.map_err(|er| MemError::io(dir.display().to_string(), er))?;
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let Some(file_id) = p.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if plan_rel(store, file_id).is_err() {
            continue;
        }
        let bytes = std::fs::read(&p).map_err(|er| MemError::io(p.display().to_string(), er))?;
        match serde_json::from_slice::<Plan>(&bytes) {
            Ok(plan) if plan.id == file_id => out.push(plan),
            Ok(_) => {}
            Err(er) => {
                // R6.2 可隔离：坏计划跳过 + verify 报告（此处静默收集由 verify 扫）。
                let _ = er;
            }
        }
    }
    out.sort_by(|a, b| a.created.cmp(&b.created));
    Ok(out)
}

#[derive(Debug, Default)]
pub struct AppliedReport {
    pub executed: usize,
    pub warnings: Vec<String>,
}

/// 执行计划（D17 维护流程）：独占门闩内逐记录令牌 CAS；status: executing 持久化
/// 供 verify 检出半成品——崩溃后续跑（显式、经确认 = 再次 apply）。
pub fn apply(
    store: &Store,
    types: &TypeSet,
    plan_id: &str,
    committer: Option<&dyn ops::CommitSink>,
) -> MemResult<AppliedReport> {
    store.check_writable()?;
    let mut plan = load_plan(store, plan_id)?.ok_or_else(|| MemError::MissingRecord {
        id_or_path: format!("plan {plan_id}"),
    })?;
    if plan.status == PlanStatus::Done {
        return Ok(AppliedReport::default());
    }
    if plan.completed_steps > plan.steps.len()
        || plan
            .in_progress
            .as_ref()
            .is_some_and(|c| c.index != plan.completed_steps)
    {
        return Err(MemError::PlanCorrupt {
            path: store
                .abs(&format!("agenda/{plan_id}.json"))
                .display()
                .to_string(),
            reason: "计划步骤进度与步骤列表不一致".into(),
        });
    }
    // 屏障覆盖计划状态与每步检查点，普通写不能改变执行快照。
    let _barrier = crate::lock::barrier_exclusive(store)?;
    // executing 标记先落盘（崩溃可检出，D17）。
    plan.status = PlanStatus::Executing;
    save_plan(store, &plan)?;
    let mut report = AppliedReport::default();
    // 计划级独占门闩贯穿全部步骤；各 step 使用不自行取门闩的 locked 变体。
    let result = (|| -> MemResult<()> {
        while plan.completed_steps < plan.steps.len() {
            let index = plan.completed_steps;
            let step = &plan.steps[index];
            if let Some(checkpoint) = plan.in_progress.clone() {
                validate_checkpoint_for_step(step, &checkpoint, index)?;
                match reconcile_step(store, step, &checkpoint)? {
                    StepRecovery::Completed => {
                        plan.completed_steps += 1;
                        plan.in_progress = None;
                        save_plan(store, &plan)?;
                        report.executed += 1;
                        continue;
                    }
                    StepRecovery::NotStarted => {}
                    StepRecovery::Ambiguous(reason) => {
                        return Err(MemError::BadArgument(format!(
                            "计划第 {} 步结果与恢复检查点不一致，已停止以免重复写入：{reason}",
                            index + 1
                        )));
                    }
                }
            } else {
                plan.in_progress = Some(capture_step_checkpoint(store, step, index)?);
                save_plan(store, &plan)?;
            }
            let checkpoint = plan
                .in_progress
                .as_ref()
                .expect("步骤检查点已持久化")
                .clone();
            let ctx = ops::WriteContext {
                store,
                types,
                by: "mem apply".into(),
                committer,
                session_id: None,
            };
            match step {
                PlanStep::Merge { dup, canonical } => {
                    ops::merge_locked(
                        &ctx,
                        dup,
                        canonical,
                        &checkpoint_token(&checkpoint, dup)?,
                        &checkpoint_token(&checkpoint, canonical)?,
                    )?;
                }
                PlanStep::Delete { id, force } => {
                    let out =
                        ops::delete_locked(&ctx, id, &checkpoint_token(&checkpoint, id)?, *force)?;
                    report.warnings.extend(out.warnings);
                }
                PlanStep::Update { id, payload } => {
                    ops::update_locked(
                        &ctx,
                        id,
                        &checkpoint_token(&checkpoint, id)?,
                        payload.clone(),
                        None,
                    )?;
                }
            }
            plan.completed_steps += 1;
            plan.in_progress = None;
            save_plan(store, &plan)?;
            report.executed += 1;
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            plan.status = PlanStatus::Done;
            save_plan(store, &plan)?;
            Ok(report)
        }
        Err(e) => {
            plan.status = PlanStatus::Failed;
            let _ = save_plan(store, &plan);
            Err(e)
        }
    }
}

enum StepRecovery {
    Completed,
    NotStarted,
    Ambiguous(String),
}

fn step_record_ids(step: &PlanStep) -> Vec<&str> {
    match step {
        PlanStep::Merge { dup, canonical } => vec![dup, canonical],
        PlanStep::Delete { id, .. } | PlanStep::Update { id, .. } => vec![id],
    }
}

fn validate_checkpoint_for_step(
    step: &PlanStep,
    checkpoint: &StepCheckpoint,
    index: usize,
) -> MemResult<()> {
    let expected = step_record_ids(step);
    if checkpoint.index != index
        || checkpoint.records.len() != expected.len()
        || checkpoint
            .records
            .iter()
            .zip(expected)
            .any(|(r, id)| r.id != id)
    {
        return Err(MemError::PlanCorrupt {
            path: format!("agenda step {}", index + 1),
            reason: "步骤检查点中的记录身份与计划不一致".into(),
        });
    }
    Ok(())
}

fn capture_step_checkpoint(
    store: &Store,
    step: &PlanStep,
    index: usize,
) -> MemResult<StepCheckpoint> {
    let mut records = Vec::new();
    for id in step_record_ids(step) {
        let current = ops::get(store, id)?;
        if !current.rel.starts_with("records/") {
            return Err(MemError::BadArgument(format!(
                "计划第 {} 步要求现存记录，但 {id} 已位于 attic",
                index + 1
            )));
        }
        records.push(CheckpointRecord {
            id: id.into(),
            version: current.token.version,
            digest: current.token.digest,
        });
    }
    Ok(StepCheckpoint {
        index,
        start_seq: crate::event::EventLog::new(store).max_seq()?,
        records,
    })
}

fn checkpoint_token(checkpoint: &StepCheckpoint, id: &str) -> MemResult<crate::token::Token> {
    checkpoint
        .records
        .iter()
        .find(|r| r.id == id)
        .map(|r| crate::token::Token {
            version: r.version,
            digest: r.digest.clone(),
        })
        .ok_or_else(|| MemError::PlanCorrupt {
            path: format!("agenda step {}", checkpoint.index + 1),
            reason: format!("检查点缺少记录 {id}"),
        })
}

fn reconcile_step(
    store: &Store,
    step: &PlanStep,
    checkpoint: &StepCheckpoint,
) -> MemResult<StepRecovery> {
    let log = crate::event::EventLog::new(store).read_log()?;
    let latest = |id: &str| log.events.iter().rev().find(|e| e.id == id);
    let has_recent_merge = log
        .events
        .iter()
        .any(|e| e.seq > checkpoint.start_seq && e.op == crate::event::Op::Merge);
    let recovery = match step {
        PlanStep::Delete { id, .. } => {
            let before = checkpoint_token(checkpoint, id)?;
            let cur = ops::get(store, id)?;
            match latest(id) {
                Some(event)
                    if event.seq > checkpoint.start_seq
                        && event.op == crate::event::Op::Delete
                        && cur.rel.starts_with("attic/")
                        && cur.token == before
                        && event.new_digest.as_deref() == Some(before.digest.as_str()) =>
                {
                    StepRecovery::Completed
                }
                _ if cur.rel.starts_with("records/")
                    && cur.token == before
                    && !log.events.iter().any(|e| {
                        e.seq > checkpoint.start_seq
                            && e.id == *id
                            && e.op == crate::event::Op::Delete
                    }) =>
                {
                    StepRecovery::NotStarted
                }
                _ => StepRecovery::Ambiguous(format!("删除目标 {id} 的位置、令牌或事件不符合预期")),
            }
        }
        PlanStep::Update { id, payload } => {
            let before = checkpoint_token(checkpoint, id)?;
            let cur = ops::get(store, id)?;
            match latest(id) {
                Some(event)
                    if event.seq > checkpoint.start_seq
                        && event.op == crate::event::Op::Update
                        && event.old_digest.as_deref() == Some(before.digest.as_str())
                        && event.new_digest.as_deref() == Some(cur.token.digest.as_str())
                        && cur.rel.starts_with("records/")
                        && cur.record.payload == *payload =>
                {
                    StepRecovery::Completed
                }
                _ if cur.rel.starts_with("records/")
                    && cur.token == before
                    && !log.events.iter().any(|e| {
                        e.seq > checkpoint.start_seq
                            && e.id == *id
                            && e.op == crate::event::Op::Update
                    }) =>
                {
                    StepRecovery::NotStarted
                }
                _ => StepRecovery::Ambiguous(format!("更新目标 {id} 的内容、令牌或事件不符合预期")),
            }
        }
        PlanStep::Merge { dup, .. } => {
            let before = checkpoint_token(checkpoint, dup)?;
            let cur = ops::get(store, dup)?;
            match latest(dup) {
                Some(event)
                    if event.seq > checkpoint.start_seq
                        && event.op == crate::event::Op::Merge
                        && event.new_digest.as_deref() == Some(before.digest.as_str())
                        && cur.rel.starts_with("attic/")
                        && cur.token == before =>
                {
                    StepRecovery::Completed
                }
                _ if cur.rel.starts_with("records/")
                    && cur.token == before
                    && !has_recent_merge =>
                {
                    StepRecovery::NotStarted
                }
                _ => {
                    StepRecovery::Ambiguous(format!("合并目标 {dup} 尚未得到完整结果或期间已变化"))
                }
            }
        }
    };
    Ok(recovery)
}

/// verify 的维护域报告（半成品计划、坏计划文件）。
pub fn verify_report(store: &Store, types: &TypeSet, deep: bool) -> MemResult<Vec<String>> {
    let mut out = Vec::new();
    for plan in list_plans(store)? {
        if plan.status == PlanStatus::Executing {
            out.push(format!(
                "半成品计划 {} 处于 executing（崩溃残留）——verify 检出（D17）；续跑 = 显式 apply",
                plan.id
            ));
        }
    }
    for cycle in crate::refgraph::audit_dag(store, types)? {
        out.push(format!("派生 DAG 环：{}", cycle.cycle.join(" → ")));
    }
    // r5 B4：镜像一致性（payload x-ref 与 envelope.refs 双向核对 + payload 悬空）
    // ——merge 分裂曾显示全绿，verify 必须能检出。
    out.extend(crate::ops::mirror_consistency_audit(store, types)?);
    // R10 决策③配套：x-filter 声明形态审计（filter_shape_unsupported）。
    out.extend(crate::find::filter_declaration_audit(types));
    if deep {
        out.extend(crate::ops::deep_digest_audit(store)?);
    }
    Ok(out)
}
