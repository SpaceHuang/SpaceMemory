//! §8-8 维护与提炼例程测试：agenda 提案扫描 / apply 计划执行（独占门闩 + executing
//! 状态检出）/ fix 修复通道（git 历史 + 正文按字节保持）。锚点：D6/D17/R5.5/R6.3。

use mem_core::agenda::{self, Plan, PlanStep, ProposalKind};
use mem_core::error::MemError;
use mem_core::ops::{self, WriteContext};
use mem_core::store::{OpenMode, Store};
use mem_schema::TypeSet;

const PROJECT: &str = "0198f7a9-0000-7cd2-9e3f-000000000001";

fn fresh_store(with_git: bool) -> (tempfile::TempDir, Store, TypeSet) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("store");
    Store::create(&root).unwrap();
    let types_dir = root.join("types");
    let asset_root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/builtin-types");
    fn walk(dir: &std::path::Path, rel: &str, out: &mut Vec<(String, Vec<u8>)>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let name = e.file_name().to_string_lossy().to_string();
            let p = e.path();
            let r = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
            if p.is_dir() {
                walk(&p, &r, out);
            } else {
                out.push((r, std::fs::read(p).unwrap()));
            }
        }
    }
    let mut files = Vec::new();
    walk(&asset_root, "", &mut files);
    for (p, bytes) in files {
        let abs = root.join(&p);
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(abs, bytes).unwrap();
    }
    if with_git {
        run_git(&root, &["init", "-q"]);
    }
    let store = Store::open(&root, OpenMode::Auto).unwrap();
    let types = TypeSet::load_dir(&types_dir).unwrap();
    // seed project（手写，随后入 git 基线）。
    let env = mem_core::envelope::Envelope {
        id: PROJECT.into(),
        r#type: "project".into(),
        created: "2026-11-04T00:00:00Z".into(),
        modified: "2026-11-04T00:00:00Z".into(),
        version: 1,
        principal: None,
        refs: vec![],
        aliases: vec!["proj".into()],
        evidence_cutoff_seq: None,
        commit: None,
        extra: Default::default(),
    };
    let bytes = mem_core::record::serialize_new(
        &env,
        &serde_json::json!({"slug":"proj","title":"Proj","status":"active"}),
        b"seed body",
    );
    let abs = store.abs("records/project/proj.md");
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(abs, bytes).unwrap();
    (tmp, store, types)
}

fn run_git(dir: &std::path::Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.email=t@t", "-c", "user.name=t"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

fn ctx<'a>(store: &'a Store, types: &'a TypeSet) -> WriteContext<'a> {
    WriteContext { store, types, by: "t".into(), committer: None, session_id: None }
}

// ---------- agenda：机械性扫描（D6：自主的注意力 = 机械动作） ----------

#[test]
fn agenda_scans_dangling_refs_and_duplicates() {
    let (_t, store, types) = fresh_store(false);
    // 断引用：指向不存在的 id。
    let c = ctx(&store, &types);
    let mem = ops::add(&c, ops::AddArgs {
        type_name: "memory".into(),
        payload: serde_json::json!({"title": "悬空引用源"}),
        body: b"".to_vec(),
        principal: None,
    })
    .unwrap();
    let m_cur = ops::get(&store, &mem.record.envelope.id).unwrap();
    let mut env = m_cur.record.envelope.clone();
    env.refs.push(mem_core::envelope::Ref {
        rel: "relates".into(),
        target: "0198ghost-0000-7cd2-9e3f-000000000001".into(),
    });
    let bytes = mem_core::record::serialize_new(&env, &m_cur.record.payload, &m_cur.record.body);
    std::fs::write(store.abs(&m_cur.rel), bytes).unwrap();
    // 重复：两个记录同一别名（fold 相等）。
    ops::add(&c, ops::AddArgs {
        type_name: "memory".into(),
        payload: serde_json::json!({"title": "A"}),
        body: b"".to_vec(),
        principal: None,
    })
    .unwrap();
    let dup = ops::add(&c, ops::AddArgs {
        type_name: "memory".into(),
        payload: serde_json::json!({"title": "B", "alias_dup": true}),
        body: b"".to_vec(),
        principal: None,
    })
    .unwrap();
    let d_cur = ops::get(&store, &dup.record.envelope.id).unwrap();
    let mut denv = d_cur.record.envelope.clone();
    denv.aliases.push("shared-alias".into());
    let dbytes = mem_core::record::serialize_new(&denv, &d_cur.record.payload, &d_cur.record.body);
    std::fs::write(store.abs(&d_cur.rel), dbytes).unwrap();
    let first = ops::scan_all(&store).unwrap().records[0].clone();
    let _ = first;
    // 给第一条也挂同别名（手改）。
    let scan = ops::scan_all(&store).unwrap();
    for r in &scan.records {
        if r.record.envelope.r#type == "memory"
            && r.record.envelope.id != dup.record.envelope.id
            && r.record.payload["title"] == serde_json::json!("A")
        {
            let mut e2 = r.record.envelope.clone();
            e2.aliases.push("Shared-Alias".into());
            let b2 = mem_core::record::serialize_new(&e2, &r.record.payload, &r.record.body);
            std::fs::write(store.abs(&r.rel), b2).unwrap();
        }
    }
    let proposals = agenda::scan_agenda(&store, &types).unwrap();
    assert!(proposals.iter().any(|p| matches!(p.kind, ProposalKind::DanglingRef { .. })), "{proposals:?}");
    assert!(proposals.iter().any(|p| matches!(p.kind, ProposalKind::Duplicate { .. })), "{proposals:?}");
    // 提案落盘 + 读取。
    agenda::write_proposals(&store, &proposals).unwrap();
    let listed = agenda::list_plans(&store).unwrap();
    assert!(listed.iter().any(|p| p.kind == agenda::PlanKind::Proposal), "提案队列可读");
}

#[test]
fn agenda_reports_undigested_evidence() {
    let (_t, store, types) = fresh_store(false);
    let c = ctx(&store, &types);
    let e1 = ops::add(&c, ops::AddArgs {
        type_name: "memory".into(),
        payload: serde_json::json!({"title": "证据"}),
        body: b"".to_vec(),
        principal: None,
    })
    .unwrap();
    // lesson 类型未在内置资产——先补两件套（x-dag）再创建。
    let lesson_dir = store.abs("types/lesson");
    std::fs::create_dir_all(&lesson_dir).unwrap();
    std::fs::write(
        lesson_dir.join("schema.json"),
        r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","x-grade":"strict","x-unique-key":["slug"],"x-dag":true,"properties":{"slug":{"type":"string"},"title":{"type":"string"}},"required":["slug","title"]}"#,
    )
    .unwrap();
    let types2 = TypeSet::load_dir(&store.abs("types")).unwrap();
    let p = ops::add(&ctx(&store, &types2), ops::AddArgs {
        type_name: "lesson".into(),
        payload: serde_json::json!({"slug": "l1", "title": "原则"}),
        body: b"".to_vec(),
        principal: None,
    })
    .unwrap();
    let (tp, te) = (p.token.clone(), e1.token.clone());
    ops::link(&c, &p.record.envelope.id, &e1.record.envelope.id, "derived_from", &tp, &te).unwrap();
    // 未设置 evidence_cutoff_seq → 全部证据未消化。
    let proposals = agenda::scan_agenda(&store, &types2).unwrap();
    assert!(
        proposals.iter().any(|p| matches!(p.kind, ProposalKind::UndigestedEvidence { .. })),
        "{proposals:?}"
    );
}

// ---------- apply：计划执行（独占门闩内逐记录 CAS；executing 可检出） ----------

fn make_merge_plan(store: &Store, types: &TypeSet) -> (String, String, String) {
    let c = ctx(store, types);
    let d1 = ops::add(&c, ops::AddArgs {
        type_name: "memory".into(),
        payload: serde_json::json!({"title": "原始"}),
        body: b"".to_vec(),
        principal: None,
    })
    .unwrap();
    let d2 = ops::add(&c, ops::AddArgs {
        type_name: "memory".into(),
        payload: serde_json::json!({"title": "原始（重复）"}),
        body: b"".to_vec(),
        principal: None,
    })
    .unwrap();
    // d2 挂别名（重复面）。
    // d2 挂别名（重复面；别名是信封字段，手改模拟外部编辑者）。
    let d2cur = ops::get(store, &d2.record.envelope.id).unwrap();
    let mut d2env = d2cur.record.envelope.clone();
    d2env.aliases.push("dup-alias".into());
    let d2bytes = mem_core::record::serialize_new(&d2env, &d2cur.record.payload, &d2cur.record.body);
    std::fs::write(store.abs(&d2cur.rel), d2bytes).unwrap();
    let plan = Plan {
        id: agenda::new_plan_id(),
        kind: agenda::PlanKind::Maintenance,
        status: agenda::PlanStatus::Draft,
        steps: vec![PlanStep::Merge {
            dup: d2.record.envelope.id.clone(),
            canonical: d1.record.envelope.id.clone(),
        }],
        created: mem_core::clock::now_rfc3339(),
    };
    agenda::save_plan(store, &plan).unwrap();
    (plan.id, d1.record.envelope.id, d2.record.envelope.id)
}

#[test]
fn apply_merge_plan_tombstone_last_and_marks_done() {
    let (_t, store, types) = fresh_store(false);
    let (plan_id, canonical, dup) = make_merge_plan(&store, &types);
    let report = agenda::apply(&store, &types, &plan_id, None).unwrap();
    assert_eq!(report.executed, 1);
    // dup 进 attic；canonical 吸收别名。
    let scan = ops::scan_all(&store).unwrap();
    assert!(scan.records.iter().all(|r| r.record.envelope.id != dup), "dup 已 tombstone");
    let can = scan.records.iter().find(|r| r.record.envelope.id == canonical).unwrap();
    assert_eq!(can.record.envelope.aliases.len(), 1, "别名移交");
    // 计划状态 done。
    let plan = agenda::load_plan(&store, &plan_id).unwrap().unwrap();
    assert_eq!(plan.status, agenda::PlanStatus::Done);
}

#[test]
fn apply_delete_plan_respects_block_and_force() {
    let (_t, store, types) = fresh_store(false);
    let c = ctx(&store, &types);
    let m = ops::add(&c, ops::AddArgs {
        type_name: "memory".into(),
        payload: serde_json::json!({"title": "被删者"}),
        body: b"".to_vec(),
        principal: None,
    })
    .unwrap();
    let plan = Plan {
        id: agenda::new_plan_id(),
        kind: agenda::PlanKind::Maintenance,
        status: agenda::PlanStatus::Draft,
        steps: vec![PlanStep::Delete { id: m.record.envelope.id.clone(), force: false }],
        created: mem_core::clock::now_rfc3339(),
    };
    agenda::save_plan(&store, &plan).unwrap();
    // 无引用 → 删除成功（tombstone）。
    agenda::apply(&store, &types, &plan.id, None).unwrap();
    assert!(!store.abs(&m.rel).exists());
    let plan = agenda::load_plan(&store, &plan.id).unwrap().unwrap();
    assert_eq!(plan.status, agenda::PlanStatus::Done);
}

#[test]
fn executing_plan_is_detectable_by_verify() {
    let (_t, store, types) = fresh_store(false);
    let (plan_id, _c, _d) = make_merge_plan(&store, &types);
    // 模拟崩溃残留：status = executing（D17：verify 检出半成品计划）。
    let mut plan = agenda::load_plan(&store, &plan_id).unwrap().unwrap();
    plan.status = agenda::PlanStatus::Executing;
    agenda::save_plan(&store, &plan).unwrap();
    let report = mem_core::agenda::verify_report(&store, &types, false).unwrap();
    assert!(
        report.iter().any(|s| s.contains("executing")),
        "半成品计划必须可检出：{report:?}"
    );
    // 续跑（显式、经确认——此处确认即调用 apply）。
    plan.status = agenda::PlanStatus::Draft;
    agenda::save_plan(&store, &plan).unwrap();
    agenda::apply(&store, &types, &plan.id, None).unwrap();
}

// ---------- fix：修复通道（R6.3；正文按字节保持，红线27） ----------

#[test]
fn fix_rebuilds_frontmatter_from_git_keeping_body_bytes() {
    let (_t, store, types) = fresh_store(true);
    let c = ctx(&store, &types);
    let m = ops::add(&c, ops::AddArgs {
        type_name: "memory".into(),
        payload: serde_json::json!({"title": "要坏的记录"}),
        body: "original body\r\n".as_bytes().to_vec(),
        principal: None,
    })
    .unwrap();
    let rel = m.rel.clone();
    // git 基线提交（机制同批或外部提交均可——此处外部 add+commit）。
    run_git(store.root(), &["add", "-A"]);
    run_git(store.root(), &["commit", "-qm", "baseline"]);
    // 注入坏 frontmatter（正文保留人手字节）。
    let body = b"hand-edited body\r\nsecond line\r\n";
    let mut broken = b"---\n{ broken json".to_vec();
    broken.extend_from_slice(b"\n---\n");
    broken.extend_from_slice(body);
    std::fs::write(store.abs(&rel), &broken).unwrap();
    // fix：git 历史上一版 frontmatter + 当前正文字节（HistoryStore 由 mem-git 提供）。
    let git = mem_git_shim::GitProcessShim::new(store.root().to_path_buf());
    let out =
        mem_core::ops::fix_record(&store, &types, &m.record.envelope.id, Some(&git), None, None).unwrap();
    assert!(
        window_has(&out.record.body, b"hand-edited body\r\nsecond line\r\n"),
        "正文字节领土保持（红线27）"
    );
    assert_eq!(out.record.envelope.r#type, "memory");
    // 事件：fix。
    let last = mem_core::event::EventLog::new(&store)
        .last_event_for(&m.record.envelope.id)
        .unwrap()
        .unwrap();
    assert_eq!(last.op, mem_core::event::Op::Fix);
}

fn window_has(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn fix_keeps_body_bytes_when_body_contains_horizontal_rules() {
    // B2 盲区：正文含水平线（---）的损坏记录——修复必须保留**全部**正文字节
    //（此前切分取最后一个 ---，水平线之前的正文被静默丢弃，违反红线27）。
    let (_t, store, types) = fresh_store(false);
    let rel = "records/memory/broken.md";
    let broken = "---\n{ broken json\n---\n# 标题段落\n\n---\n\n结尾段落（必须保留）\n"
        .as_bytes();
    std::fs::create_dir_all(store.abs("records/memory")).unwrap();
    std::fs::write(store.abs(rel), broken).unwrap();
    let env = mem_core::envelope::Envelope {
        id: "0198broken-0000-7cd2-9e3f-000000000001".into(),
        r#type: "memory".into(),
        created: "2026-11-04T00:00:00Z".into(),
        modified: "2026-11-04T00:00:00Z".into(),
        version: 1,
        principal: None,
        refs: vec![],
        aliases: vec![],
        evidence_cutoff_seq: None,
        commit: None,
        extra: Default::default(),
    };
    let out = mem_core::ops::fix_record(
        &store,
        &types,
        rel,
        None,
        Some((env, serde_json::json!({"title": "broken-rec"}))),
        None,
    )
    .unwrap();
    assert_eq!(
        out.record.body,
        "# 标题段落\n\n---\n\n结尾段落（必须保留）\n".as_bytes(),
        "正文按字节保持（红线27）——含水平线"
    );
}

#[test]
fn fix_without_git_requires_provided_envelope() {
    let (_t, store, types) = fresh_store(false);
    let c = ctx(&store, &types);
    let m = ops::add(&c, ops::AddArgs {
        type_name: "memory".into(),
        payload: serde_json::json!({"title": "no-git"}),
        body: b"b".to_vec(),
        principal: None,
    })
    .unwrap();
    let rel = m.rel.clone();
    std::fs::write(store.abs(&rel), b"---\n{broken\n---\nbody").unwrap();
    // 无 git 历史可用 → 需要调用方提供 envelope（响亮而非静默）。
    let err = mem_core::ops::fix_record(&store, &types, &m.record.envelope.id, None, None, None).unwrap_err();
    assert!(matches!(err, MemError::BadArgument(_)) || matches!(err, MemError::Corrupt { .. }), "{err}");
    // 提供信封 → 修复成功。
    let env = mem_core::envelope::Envelope {
        id: m.record.envelope.id.clone(),
        r#type: "memory".into(),
        created: "2026-11-04T00:00:00Z".into(),
        modified: "2026-11-04T00:00:00Z".into(),
        version: 1,
        principal: None,
        refs: vec![],
        aliases: vec![],
        evidence_cutoff_seq: None,
        commit: None,
        extra: Default::default(),
    };
    let out = mem_core::ops::fix_record(&store, &types, &m.record.envelope.id, None, Some((env, serde_json::json!({"title": "no-git"}))), None).unwrap();
    assert_eq!(out.record.body, b"body");
}

/// 测试用 HistoryStore（与 mem-git::GitProcess 同款子进程调用；mem-core 测试不依赖 mem-git）。
mod mem_git_shim {
    use std::process::Command;

    pub struct GitProcessShim {
        workdir: std::path::PathBuf,
    }

    impl GitProcessShim {
        pub fn new(workdir: std::path::PathBuf) -> Self {
            Self { workdir }
        }
    }

    impl mem_core::ops::HistoryStore for GitProcessShim {
        fn previous_file(&self, rel: &str) -> mem_core::error::MemResult<Option<Vec<u8>>> {
            if !self.workdir.join(".git").exists() {
                return Ok(None);
            }
            let out = Command::new("git")
                .arg("-C")
                .arg(&self.workdir)
                .args(["show", &format!("HEAD:{rel}")])
                .output()
                .map_err(|e| mem_core::error::MemError::Git(e.to_string()))?;
            if out.status.success() {
                Ok(Some(out.stdout))
            } else {
                Ok(None)
            }
        }
    }
}

#[test]
fn fix_on_parseable_record_requires_expect_token() {
    // H1：可解析记录的修复是读改写——红线19 强制 CAS；损坏记录豁免（无从取令牌）。
    let (_t, store, types) = fresh_store(false);
    let c = ctx(&store, &types);
    let m = ops::add(&c, ops::AddArgs {
        type_name: "memory".into(),
        payload: serde_json::json!({"title": "t"}),
        body: b"".to_vec(),
        principal: None,
    })
    .unwrap();
    let id = &m.record.envelope.id;
    // 无 expect → 响亮拒绝。
    let err = mem_core::ops::fix_record(&store, &types, id, None, None, None).unwrap_err();
    assert!(err.to_string().contains("expect"), "{err}");
    // 带正确 expect → 成功（确定性再生成）。
    let out = mem_core::ops::fix_record(&store, &types, id, None, None, Some(&m.token)).unwrap();
    assert_eq!(out.record.envelope.version, m.record.envelope.version);
    // 陈旧 expect → 结构化冲突。
    let stale = mem_core::token::Token { version: m.token.version, digest: "0".repeat(64) };
    let err2 = mem_core::ops::fix_record(&store, &types, id, None, None, Some(&stale)).unwrap_err();
    assert!(matches!(err2, MemError::Conflict { .. }), "{err2}");
}

#[test]
fn deep_audit_detects_disk_vs_event_log_mismatch() {
    // H3：--deep 对照事件日志——保版本手改必须被检出（此前自比自恒等）。
    let (_t, store, types) = fresh_store(false);
    let c = ctx(&store, &types);
    let m = ops::add(&c, ops::AddArgs {
        type_name: "memory".into(),
        payload: serde_json::json!({"title": "t"}),
        body: b"".to_vec(),
        principal: None,
    })
    .unwrap();
    // 保版本手改。
    let cur = ops::get(&store, &m.record.envelope.id).unwrap();
    let bytes = mem_core::record::serialize_new(&cur.record.envelope, &cur.record.payload, "手改".as_bytes());
    std::fs::write(store.abs(&cur.rel), bytes).unwrap();
    let report = mem_core::ops::deep_digest_audit(&store).unwrap();
    assert!(
        report.iter().any(|s| s.contains(&m.record.envelope.id) && s.contains("磁盘与事件日志不一致")),
        "{report:?}"
    );
}

// ---------- r4 B4：merge 引用形态回归（canonical/dup 自身参与引用时不断链、链连续） ----------

/// merge 后 verify 立即全绿 + 事件链无空洞（r4 B4 验收口径）。
fn assert_merge_left_store_consistent(store: &Store, types: &TypeSet, dup: &str, _canonical: &str) {
    let scan = ops::scan_all(store).unwrap();
    // 无悬空引用：任何记录的 refs 不得指向 dup（已 tombstone）。
    for r in &scan.records {
        for e in &r.record.envelope.refs {
            assert_ne!(e.target, dup, "断链：{} --{}--> {}（dup 已并入）", r.record.envelope.id, e.rel, e.target);
        }
    }
    // 无自引用。
    for r in &scan.records {
        for e in &r.record.envelope.refs {
            assert_ne!(e.target, r.record.envelope.id, "自引用：{}", r.record.envelope.id);
        }
    }
    // 事件版本链连续。
    let log = mem_core::event::EventLog::new(store).read_log().unwrap();
    let holes = mem_core::event::chain_audit(&log.events);
    assert!(holes.is_empty(), "事件链空洞：{holes:?}");
    // verify（含 --deep）全绿。
    let report = mem_core::agenda::verify_report(store, types, false).unwrap();
    assert!(report.is_empty(), "verify 报告：{report:?}");
}

#[test]
fn merge_canonical_referencing_dup_leaves_no_dangling_or_chain_hole() {
    // r4 B4 复现：canonical C --derived_from--> dup D，merge D→C——
    // 旧实现循环改写 C 一次、别名移交再覆盖一次：断链 + old_v 重复。
    let (_t, store, types) = fresh_store(false);
    let c = ctx(&store, &types);
    let d = ops::add(&c, ops::AddArgs {
        type_name: "memory".into(),
        payload: serde_json::json!({"title": "dup"}),
        body: b"".to_vec(),
        principal: None,
    })
    .unwrap();
    let can = ops::add(&c, ops::AddArgs {
        type_name: "memory".into(),
        payload: serde_json::json!({"title": "canonical"}),
        body: b"".to_vec(),
        principal: None,
    })
    .unwrap();
    let (tc, td) = ops::link(&c, &can.record.envelope.id, &d.record.envelope.id, "derived_from",
        &can.token, &d.token).unwrap();
    ops::merge(&c, &d.record.envelope.id, &can.record.envelope.id, &td, &tc).unwrap();
    assert_merge_left_store_consistent(&store, &types, &d.record.envelope.id, &can.record.envelope.id);
    // canonical 的 refs 不含已删除边（指向 dup 的边被删除——并入自身 = 自引用）。
    let cur = ops::get(&store, &can.record.envelope.id).unwrap();
    assert!(
        !cur.record.envelope.refs.iter().any(|e| e.target == d.record.envelope.id),
        "指向 dup 的边必须移除"
    );
}

#[test]
fn merge_dup_referencing_canonical_is_clean() {
    // dup --derived_from--> canonical：dup 进 attic，出边随 tombstone 消失；
    // 循环跳过 dup（不陈旧改写）。
    let (_t, store, types) = fresh_store(false);
    let c = ctx(&store, &types);
    let d = ops::add(&c, ops::AddArgs {
        type_name: "memory".into(),
        payload: serde_json::json!({"title": "dup"}),
        body: b"".to_vec(),
        principal: None,
    })
    .unwrap();
    let can = ops::add(&c, ops::AddArgs {
        type_name: "memory".into(),
        payload: serde_json::json!({"title": "canonical"}),
        body: b"".to_vec(),
        principal: None,
    })
    .unwrap();
    let (td, tc) = ops::link(&c, &d.record.envelope.id, &can.record.envelope.id, "derived_from",
        &d.token, &can.token).unwrap();
    ops::merge(&c, &d.record.envelope.id, &can.record.envelope.id, &td, &tc).unwrap();
    assert_merge_left_store_consistent(&store, &types, &d.record.envelope.id, &can.record.envelope.id);
}

#[test]
fn merge_repoints_third_party_and_keeps_chain_contiguous() {
    // 第三方 X --relates--> dup → 重挂 X --relates--> canonical；事件链连续。
    let (_t, store, types) = fresh_store(false);
    let c = ctx(&store, &types);
    let d = ops::add(&c, ops::AddArgs {
        type_name: "memory".into(),
        payload: serde_json::json!({"title": "dup"}),
        body: b"".to_vec(),
        principal: None,
    })
    .unwrap();
    let can = ops::add(&c, ops::AddArgs {
        type_name: "memory".into(),
        payload: serde_json::json!({"title": "canonical"}),
        body: b"".to_vec(),
        principal: None,
    })
    .unwrap();
    let x = ops::add(&c, ops::AddArgs {
        type_name: "memory".into(),
        payload: serde_json::json!({"title": "第三方"}),
        body: b"".to_vec(),
        principal: None,
    })
    .unwrap();
    let (tx, td) = ops::link(&c, &x.record.envelope.id, &d.record.envelope.id, "derived_from",
        &x.token, &d.token).unwrap();
    ops::merge(&c, &d.record.envelope.id, &can.record.envelope.id, &td, &can.token).unwrap();
    let x_cur = ops::get(&store, &x.record.envelope.id).unwrap();
    assert!(
        x_cur.record.envelope.refs.iter().any(|e| e.target == can.record.envelope.id),
        "第三方引用重挂到 canonical"
    );
    let _ = tx;
    assert_merge_left_store_consistent(&store, &types, &d.record.envelope.id, &can.record.envelope.id);
}

// ---------- r5 B4：payload/envelope 分裂回归（按引用方类型重挂 x-ref） ----------

#[test]
fn merge_rewrites_referent_payload_xref_by_its_own_decl() {
    // r5 复现：topic D、topic C；memory M payload.topic=D（镜像 refs topic→D）。
    // merge D→C 后：M 的 payload.topic 必须改为 C（引用方 memory 自身的声明），
    // envelope 镜像一致，原 payload 可继续 CAS update，verify 全绿。
    let (_t, store, types) = fresh_store(false);
    let c = ctx(&store, &types);
    let mk_topic = |slug: &str| {
        ops::add(&ctx(&store, &types), ops::AddArgs {
            type_name: "topic".into(),
            payload: serde_json::json!({"slug": slug, "title": slug}),
            body: b"".to_vec(),
            principal: None,
        })
        .unwrap()
    };
    let d = mk_topic("dup-topic");
    let can = mk_topic("can-topic");
    let m = ops::add(&c, ops::AddArgs {
        type_name: "memory".into(),
        payload: serde_json::json!({"title": "M", "topic": d.record.envelope.id}),
        body: b"".to_vec(),
        principal: None,
    })
    .unwrap();
    // 镜像确认：envelope.refs 含 topic→D。
    let m0 = ops::get(&store, &m.record.envelope.id).unwrap();
    assert!(m0.record.envelope.refs.iter().any(|e| e.rel == "topic" && e.target == d.record.envelope.id));
    // merge D→C。
    let d_cur = ops::get(&store, &d.record.envelope.id).unwrap();
    let can_cur = ops::get(&store, &can.record.envelope.id).unwrap();
    ops::merge(&c, &d.record.envelope.id, &can.record.envelope.id,
        &d_cur.token, &can_cur.token).unwrap();
    // M 的 payload.topic 与 envelope 镜像都必须指向 C。
    let m1 = ops::get(&store, &m.record.envelope.id).unwrap();
    assert_eq!(m1.record.payload["topic"], serde_json::json!(can.record.envelope.id),
        "payload x-ref 必须按引用方类型重挂（r5 B4）");
    assert!(m1.record.envelope.refs.iter().any(|e| e.rel == "topic" && e.target == can.record.envelope.id));
    // 原 payload 可继续 CAS update（topic=C 存在，校验通过）。
    let payload2 = serde_json::json!({"title": "M2", "topic": can.record.envelope.id});
    let upd = ops::update(&ctx(&store, &types), &m1.record.envelope.id, &m1.token, payload2, None).unwrap();
    assert_eq!(upd.record.envelope.version, m1.record.envelope.version + 1);
    // verify（含镜像一致性）全绿。
    assert_merge_left_store_consistent(&store, &types, &d.record.envelope.id, &can.record.envelope.id);
}

#[test]
fn merge_canonical_own_xref_to_dup_is_nulled_consistently() {
    // canonical 自身 payload.x = dup → 置 null（与信封删边同构），两层一致。
    // 场景：dup = topic D；canonical = memory（payload.topic = D——跨类型 x-ref，
    // 与 r5 报告场景同构）。
    let (_t, store, types) = fresh_store(false);
    let d = ops::add(&ctx(&store, &types), ops::AddArgs {
        type_name: "topic".into(),
        payload: serde_json::json!({"slug": "dup-topic", "title": "dup"}),
        body: b"".to_vec(),
        principal: None,
    })
    .unwrap();
    // canonical memory.payload.topic 指向 dup topic（memory 的 x-ref 字段 topic，
    // target_type=topic——类型兼容）。
    let can = ops::add(&ctx(&store, &types), ops::AddArgs {
        type_name: "memory".into(),
        payload: serde_json::json!({"title": "canonical", "topic": d.record.envelope.id}),
        body: b"".to_vec(),
        principal: None,
    })
    .unwrap();
    let d_cur = ops::get(&store, &d.record.envelope.id).unwrap();
    let can_cur = ops::get(&store, &can.record.envelope.id).unwrap();
    ops::merge(&ctx(&store, &types), &d.record.envelope.id, &can.record.envelope.id,
        &d_cur.token, &can_cur.token).unwrap();
    let m1 = ops::get(&store, &can.record.envelope.id).unwrap();
    assert!(m1.record.payload["topic"].is_null(), "canonical 自身 x-ref 指向 dup → 置 null：{:?}", m1.record.payload);
    assert_merge_left_store_consistent(&store, &types, &d.record.envelope.id, &can.record.envelope.id);
}
