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
    DanglingRef { from: String, rel: String, target: String },
    /// 环（一般引用环由 agenda 报告，红线17；x-dag 类型在写路径已强制）。
    Cycle { cycle: Vec<String> },
    /// 存疑（R5.2 存疑规则机械打标；动作 = 加性注记/复核提案）。
    StaleByRule { id: String, rule: String },
    /// 未消化证据（R9.4：产物落后于证据；动作 = 增量再提炼）。
    UndigestedEvidence { product: String, evidence: Vec<String> },
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
            id: format!("prop-{}-{}", crate::token::sha256_hex(note.as_bytes())[..10].to_string(), out.len()),
            kind,
            note,
            created: now.clone(),
        });
    };
    // 断引用。
    let ids: std::collections::HashSet<_> =
        scan.records.iter().map(|r| r.record.envelope.id.clone()).collect();
    for r in &scan.records {
        for e in &r.record.envelope.refs {
            if !ids.contains(&e.target) {
                let (from, rel, target) =
                    (r.record.envelope.id.clone(), e.rel.clone(), e.target.clone());
                prop(
                    ProposalKind::DanglingRef { from: from.clone(), rel: rel.clone(), target: target.clone() },
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
                ProposalKind::Duplicate { ids: ids.clone(), key },
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
        if r.record.envelope.refs.iter().any(|e| e.rel == "derived_from") {
            let ev = crate::refgraph::undigested_evidence(store, &r.record.envelope.id)?;
            if !ev.is_empty() {
                let product = r.record.envelope.id.clone();
                prop(
                    ProposalKind::UndigestedEvidence { product: product.clone(), evidence: ev.clone() },
                    format!("产物 {} 有 {} 条未消化证据（增量再提炼）", product, ev.len()),
                    &mut out,
                );
            }
        }
    }
    // 存疑规则（R5.2：使用方声明 → 机械打标）。规则形态 v1：
    // {"type": "<t>", "older_than_days": N}——modified 超龄 → StaleByRule 提案。
    let rules = store.config().doubt_rules;
    for rule in &rules {
        let Some(t) = rule.get("type").and_then(|v| v.as_str()) else { continue };
        let Some(days) = rule.get("older_than_days").and_then(|v| v.as_u64()) else { continue };
        for r in &scan.records {
            if r.record.envelope.r#type != t {
                continue;
            }
            let age_days = age_in_days(&r.record.envelope.modified);
            if age_days > days {
                let id = r.record.envelope.id.clone();
                let rule_s = serde_json::to_string(rule).unwrap_or_default();
                prop(
                    ProposalKind::StaleByRule { id: id.clone(), rule: rule_s.clone() },
                    format!("存疑（规则 {rule_s}）：{id} 已 {age_days} 天未更新——行动前重验（R5.2/R5.3）"),
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
    Merge { dup: String, canonical: String },
    Delete { id: String, force: bool },
    Update { id: String, payload: serde_json::Value },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    pub id: String,
    pub kind: PlanKind,
    pub status: PlanStatus,
    pub steps: Vec<PlanStep>,
    pub created: String,
}

pub fn new_plan_id() -> String {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("plan-{}-{:06x}", crate::clock::now_rfc3339().replace(['-', ':', 'T', 'Z'], ""), t & 0xFF_FFFF)
}

fn agenda_dir(store: &Store) -> std::path::PathBuf {
    store.abs("agenda")
}

pub fn save_plan(store: &Store, plan: &Plan) -> MemResult<()> {
    store.check_writable()?;
    let dir = agenda_dir(store);
    std::fs::create_dir_all(&dir).map_err(|e| MemError::io(dir.display().to_string(), e))?;
    let p = dir.join(format!("{}.json", plan.id));
    let bytes = serde_json::to_vec_pretty(plan)
        .map_err(|e| MemError::PlanCorrupt { path: p.display().to_string(), reason: e.to_string() })?;
    crate::platform::atomic_write(&p, &bytes)
        .map_err(|e| MemError::io(p.display().to_string(), e))
}

pub fn load_plan(store: &Store, plan_id: &str) -> MemResult<Option<Plan>> {
    let p = agenda_dir(store).join(format!("{plan_id}.json"));
    if !p.is_file() {
        return Ok(None);
    }
    let bytes = std::fs::read(&p).map_err(|e| MemError::io(p.display().to_string(), e))?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|e| MemError::PlanCorrupt { path: p.display().to_string(), reason: e.to_string() })
}

pub fn list_plans(store: &Store) -> MemResult<Vec<Plan>> {
    let dir = agenda_dir(store);
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
        let bytes = std::fs::read(&p).map_err(|er| MemError::io(p.display().to_string(), er))?;
        match serde_json::from_slice::<Plan>(&bytes) {
            Ok(plan) => out.push(plan),
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
    let mut plan = load_plan(store, plan_id)?
        .ok_or_else(|| MemError::MissingRecord { id_or_path: format!("plan {plan_id}") })?;
    if plan.status == PlanStatus::Done {
        return Ok(AppliedReport::default());
    }
    // executing 标记先落盘（崩溃可检出，D17）。
    plan.status = PlanStatus::Executing;
    save_plan(store, &plan)?;
    let mut report = AppliedReport::default();
    // H4：计划级独占门闩贯穿全部步骤（D17 维护流程：步与步之间不得插入普通写；
    // 各 step 使用不自行取门闩的 locked 变体——记录锁/序列锁照取，锁序不变）。
    let _barrier = crate::lock::barrier_exclusive(store)?;
    let result = (|| -> MemResult<()> {
        for step in &plan.steps {
            let ctx = ops::WriteContext {
                store,
                types,
                by: "mem apply".into(),
                committer,
                session_id: None,
            };
            match step {
                PlanStep::Merge { dup, canonical } => {
                    let d = ops::get(store, dup)?;
                    let c = ops::get(store, canonical)?;
                    ops::merge_locked(&ctx, dup, canonical, &d.token, &c.token)?;
                    report.executed += 1;
                }
                PlanStep::Delete { id, force } => {
                    let cur = ops::get(store, id)?;
                    let out = ops::delete_locked(&ctx, id, &cur.token, *force)?;
                    report.warnings.extend(out.warnings);
                    report.executed += 1;
                }
                PlanStep::Update { id, payload } => {
                    let cur = ops::get(store, id)?;
                    ops::update_locked(&ctx, id, &cur.token, payload.clone(), None)?;
                    report.executed += 1;
                }
            }
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
    // r5 B4：镜像一致性（payload x-ref 与 envelope.refs 双向核对 + payload 悬空）
    // ——merge 分裂曾显示全绿，verify 必须能检出。
    out.extend(crate::ops::mirror_consistency_audit(store, types)?);
    if deep {
        out.extend(crate::ops::deep_digest_audit(store)?);
    }
    Ok(out)
}
