//! 引用完整性与派生关系（D16 / D17 / R9 / 红线17）。
//!
//! 节点 = 实体记录；边 = 信封引用（**永不引入图数据库引擎**，红线16）；有属性的边
//! 建模为记录（edge 类型）。本模块提供：反向索引（R9.2"从哪来"）、x-dag 派生图审计
//! （写路径强制的兜底审计 + agenda 报告一般环）、未消化证据查询（R9.4：证据引用
//! 逐项比较事件序号 vs `evidence_cutoff_seq`，非全库 diff）。

use std::collections::{BTreeMap, VecDeque};

use crate::error::MemResult;
use crate::event::EventLog;
use crate::ops;
use crate::store::Store;
use mem_schema::TypeSet;

/// 反向索引：`-> Vec<(依赖者 id, rel)>`（R9.2：删除/变更被依赖记录时的显式警示来源）。
pub fn reverse_refs(store: &Store, id: &str) -> MemResult<Vec<(String, String)>> {
    ops::reverse_refs(store, id)
}

#[derive(Debug, Clone, PartialEq)]
pub struct CycleReport {
    /// 环上的记录 id（按发现顺序，首尾相接）。
    pub cycle: Vec<String>,
}

/// 派生 DAG 审计（verify/agenda 用）：x-dag 类型 + 派生 rel 构成的图不得有环。
/// 一般类型的环也在此报告（agenda 提案，不拒收）。
pub fn audit_dag(store: &Store, types: &TypeSet) -> MemResult<Vec<CycleReport>> {
    let scan = ops::scan_all(store)?;
    // rel → 邻接（child → parents）。同一 rel 可能被多个类型使用；按类型+rel 分图。
    let mut cycles = Vec::new();
    for decl in types.decls() {
        // 意图（L3 清理）：x-dag 类型的环在写路径已强制，这里兜底审计；
        // 一般类型的环由 agenda 报告（红线17 分工）——两类都审，不做类型豁免。
        let rel = decl.derived_rel();
        let mut adj: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        let mut nodes: Vec<&str> = Vec::new();
        for r in &scan.records {
            if r.record.envelope.r#type != decl.type_name() {
                continue;
            }
            nodes.push(&r.record.envelope.id);
            for e in &r.record.envelope.refs {
                if e.rel == rel {
                    adj.entry(&r.record.envelope.id).or_default().push(&e.target);
                }
            }
        }
        // 全图找环：DFS 三色标记。
        let mut color: BTreeMap<&str, u8> = BTreeMap::new(); // 0=白 1=灰 2=黑
        let mut stack: Vec<&str> = Vec::new();
        fn dfs<'a>(
            n: &'a str,
            adj: &BTreeMap<&'a str, Vec<&'a str>>,
            color: &mut BTreeMap<&'a str, u8>,
            stack: &mut Vec<&'a str>,
            out: &mut Vec<CycleReport>,
        ) {
            color.insert(n, 1);
            stack.push(n);
            if let Some(nexts) = adj.get(n) {
                for &m in nexts {
                    match color.get(m).copied().unwrap_or(0) {
                        0 => dfs(m, adj, color, stack, out),
                        1 => {
                            // 灰 → 环：从 stack 中 m 起截取。
                            if let Some(pos) = stack.iter().position(|&x| x == m) {
                                let cycle: Vec<String> =
                                    stack[pos..].iter().map(|s| s.to_string()).collect();
                                out.push(CycleReport { cycle });
                            }
                        }
                        _ => {}
                    }
                }
            }
            stack.pop();
            color.insert(n, 2);
        }
        for n in &nodes {
            if color.get(n).copied().unwrap_or(0) == 0 {
                dfs(n, &adj, &mut color, &mut stack, &mut cycles);
            }
        }
    }
    cycles.dedup();
    Ok(cycles)
}

/// 未消化证据查询（R9.4）：产物 record 的证据引用（rel = 该类型的派生 rel）
/// 逐项比较其最后事件 seq vs 产物 `evidence_cutoff_seq`。
/// cutoff 缺省（null）→ 全部证据视为未消化（R9.7 保守语义）。
pub fn undigested_evidence(store: &Store, product_id: &str) -> MemResult<Vec<String>> {
    let product = ops::get(store, product_id)?;
    let cutoff = product.record.envelope.evidence_cutoff_seq;
    // 证据 = 指向证据的引用；rel 取类型声明的派生 rel（缺声明时用缺省 derived_from）。
    let evidence_rel = "derived_from";
    let log = EventLog::new(store).read_log()?;
    let last_seq = |id: &str| -> u64 {
        log.events
            .iter()
            .rev()
            .find(|e| e.id == id)
            .map(|e| e.seq)
            .unwrap_or(0)
    };
    let mut out = Vec::new();
    for e in &product.record.envelope.refs {
        if e.rel != evidence_rel {
            continue;
        }
        let seq = last_seq(&e.target);
        let digested = match cutoff {
            Some(c) => seq <= c,
            None => false, // 未声明 → 保守：未消化
        };
        if !digested {
            out.push(e.target.clone());
        }
    }
    Ok(out)
}

/// 多跳遍历（trace，R9.2"这条产物从哪来"）：沿引用边扩展，预算内截断（R4.2）。
/// 方向：`up` = 沿 refs 找证据/来源（产物→证据）；`down` = 反向（被谁引用）。
#[derive(Debug, Clone, PartialEq)]
pub struct TraceNode {
    pub id: String,
    pub rel: Option<String>, // 到达该节点经过的边
    pub depth: usize,
}

pub fn trace(
    store: &Store,
    start_id: &str,
    direction: TraceDirection,
    max_depth: usize,
    max_nodes: usize,
) -> MemResult<TraceOutcome> {
    trace_from(store, start_id, direction, max_depth, max_nodes, 0)
}

/// r5 B7：带 skip 的遍历（续读 = 上一页 `skip: nodes_seen` + 提高 limit 重跑，
/// 已见节点不再重复返回）。
/// 遍历结果（r11）：`truncated_by_nodes` 区分截断原因——节点预算截断时 resume
/// 给 skip/limit 续读；深度截断时给 frontier 续读。CLI/MCP 据此构造 resume，
/// 不再出现"节点截断返回 resume:null"（r11 复现）。
pub struct TraceOutcome {
    pub nodes: Vec<TraceNode>,
    pub truncated: bool,
    pub frontier: Vec<String>,
    pub truncated_by_nodes: bool,
}

pub fn trace_from(
    store: &Store,
    start_id: &str,
    direction: TraceDirection,
    max_depth: usize,
    max_nodes: usize,
    skip: usize,
) -> MemResult<TraceOutcome> {
    let scan = ops::scan_all(store)?;
    // 邻接表（正/反两个方向）。
    let mut forward: BTreeMap<&str, Vec<(&str, &str)>> = BTreeMap::new(); // id → (rel, target)
    let mut backward: BTreeMap<&str, Vec<(&str, &str)>> = BTreeMap::new(); // id → (rel, from)
    for r in &scan.records {
        for e in &r.record.envelope.refs {
            forward
                .entry(&r.record.envelope.id)
                .or_default()
                .push((&e.rel, &e.target));
            backward
                .entry(&e.target)
                .or_default()
                .push((&e.rel, &r.record.envelope.id));
        }
    }
    let start = scan
        .records
        .iter()
        .find(|r| r.record.envelope.id == start_id)
        .map(|r| r.record.envelope.id.as_str())
        .ok_or_else(|| crate::error::MemError::MissingRecord { id_or_path: start_id.into() })?;
    let mut out = Vec::new();
    let mut skipped = 0usize;
    let mut depth_truncated = false;
    // r9：深度预算用尽时的**可执行续读**素材——被 max_depth 挡住的未访问邻居
    //（frontier）：对每个 frontier 节点 `trace <id> --dir <同向> --depth <预算>`
    // 即可继续深入，天然不重复（各子树独立）。
    let mut frontier: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    seen.insert(start);
    let mut queue = VecDeque::new();
    queue.push_back((start, None::<String>, 0usize));
    while let Some((n, rel, depth)) = queue.pop_front() {
        if depth > 0 {
            // r5 B7：skip 语义 = 跳过已见节点（续读不重复）。
            if skipped < skip {
                skipped += 1;
                // 注意：仍要扩展其邻居（BFS 结构不因跳过输出而中断）。
                if depth >= max_depth {
                    let nexts: Vec<(&str, &str)> = match direction {
                        TraceDirection::Up => forward
                            .get(n)
                            .map(|v| v.iter().map(|(rel, t)| (*rel, *t)).collect())
                            .unwrap_or_default(),
                        TraceDirection::Down => backward
                            .get(n)
                            .map(|v| v.iter().map(|(rel, f)| (*rel, *f)).collect())
                            .unwrap_or_default(),
                    };
                    for (_, nxt) in nexts {
                        if !seen.contains(nxt) {
                            depth_truncated = true;
                            frontier.push(nxt.to_string());
                        }
                    }
                } else {
                    let nexts: Vec<(&str, &str)> = match direction {
                        TraceDirection::Up => forward
                            .get(n)
                            .map(|v| v.iter().map(|(rel, t)| (*rel, *t)).collect())
                            .unwrap_or_default(),
                        TraceDirection::Down => backward
                            .get(n)
                            .map(|v| v.iter().map(|(rel, f)| (*rel, *f)).collect())
                            .unwrap_or_default(),
                    };
                    for (rel2, nxt) in nexts {
                        if seen.insert(nxt) {
                            queue.push_back((nxt, Some(rel2.to_string()), depth + 1));
                        }
                    }
                }
                continue;
            }
            out.push(TraceNode { id: n.to_string(), rel, depth });
        }
        if out.len() >= max_nodes {
            return Ok(TraceOutcome {
                nodes: out,
                truncated: true,
                frontier,
                truncated_by_nodes: true,
            });
        }
        // r6 B7：深度预算处停止扩展时，若存在未访问邻居 = **深度截断**——
        // 必须如实标注（此前静默 continue，A→B→C 在深度 1 只返回 B 却报 truncated:false）。
        if depth >= max_depth {
            // r9：收集被深度挡住的未访问邻居（frontier——可执行续读的起点）。
            let nexts: Vec<(&str, &str)> = match direction {
                TraceDirection::Up => forward
                    .get(n)
                    .map(|v| v.iter().map(|(rel, t)| (*rel, *t)).collect())
                    .unwrap_or_default(),
                TraceDirection::Down => backward
                    .get(n)
                    .map(|v| v.iter().map(|(rel, f)| (*rel, *f)).collect())
                    .unwrap_or_default(),
            };
            for (_, nxt) in nexts {
                if !seen.contains(nxt) {
                    depth_truncated = true;
                    frontier.push(nxt.to_string());
                }
            }
            continue;
        }
        let nexts: Vec<(&str, &str)> = match direction {
            TraceDirection::Up => forward
                .get(n)
                .map(|v| v.iter().map(|(rel, t)| (*rel, *t)).collect())
                .unwrap_or_default(),
            TraceDirection::Down => backward
                .get(n)
                .map(|v| v.iter().map(|(rel, f)| (*rel, *f)).collect())
                .unwrap_or_default(),
        };
        for (rel, nxt) in nexts {
            if seen.insert(nxt) {
                queue.push_back((nxt, Some(rel.to_string()), depth + 1));
            }
        }
    }
    Ok(TraceOutcome {
        nodes: out,
        truncated: depth_truncated,
        frontier,
        truncated_by_nodes: false,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceDirection {
    /// 沿 refs 找证据/来源。
    Up,
    /// 反向：被谁引用。
    Down,
}
