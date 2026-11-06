//! §8-4d 会话台账 / 接手 + 视图 / 预算 / 引用图查询测试（D13/D10/R4/D16/R9）。

use mem_core::envelope::{Envelope, Ref};
use mem_core::event::EventLog;
use mem_core::ops::{self, AddArgs, WriteContext};
use mem_core::refgraph;
use mem_core::session::{self, HandoffSpec, StartArgs};
use mem_core::store::{OpenMode, Store};
use mem_core::view::{self, ViewFilter};
use mem_schema::TypeSet;

fn fresh_store() -> (tempfile::TempDir, Store, TypeSet) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("store");
    Store::create(&root).unwrap();
    // 资产相对路径以数据目录根为基准（types/... 前缀已含）。
    let types_dir = root.join("types");
    for (p, bytes) in asset_bytes() {
        let abs = root.join(&p);
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(abs, &bytes).unwrap();
    }
    // lesson（x-dag 测试类型，与 ops 夹具一致）。
    let lesson = types_dir.join("lesson");
    std::fs::create_dir_all(&lesson).unwrap();
    std::fs::write(
        lesson.join("schema.json"),
        r#"{
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object", "x-grade": "strict", "x-unique-key": ["slug"], "x-dag": true,
        "properties": {
          "slug": { "type": "string", "minLength": 1 },
          "title": { "type": "string" }
        },
        "required": ["slug", "title"]
      }"#,
    )
    .unwrap();
    let store = Store::open(&root, OpenMode::Auto).unwrap();
    seed_project(&store, "0198f7a9-0000-7cd2-9e3f-000000000001", "0198f7a9-0000-7cd2-9e3f-000000000001");
    seed_project(&store, "0198f7a9-0000-7cd2-9e3f-000000000002", "0198f7a9-0000-7cd2-9e3f-000000000002");
    let types = TypeSet::load_dir(&types_dir).unwrap();
    (tmp, store, types)
}

fn asset_bytes() -> Vec<(String, Vec<u8>)> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/builtin-types");
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
    let mut out = Vec::new();
    walk(&root, "", &mut out);
    out.sort();
    out
}


/// 手写一条固定 id 的 project 记录（x-ref 存在性的目标；绕过机制写路径=外部写入者）。
fn seed_project(store: &Store, id: &str, slug: &str) {
    let env = mem_core::envelope::Envelope {
        id: id.into(),
        r#type: "project".into(),
        created: "2026-11-04T00:00:00Z".into(),
        modified: "2026-11-04T00:00:00Z".into(),
        version: 1,
        principal: None,
        refs: vec![],
        aliases: vec![slug.into()],
        evidence_cutoff_seq: None,
        commit: None,
        extra: Default::default(),
    };
    let payload = serde_json::json!({"slug": slug, "title": slug, "status": "active"});
    let bytes = mem_core::record::serialize_new(&env, &payload, b"");
    let rel = format!("records/project/{slug}.md");
    let abs = store.abs(&rel);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(abs, bytes).unwrap();
}

fn ctx<'a>(store: &'a Store, types: &'a TypeSet) -> WriteContext<'a> {
    WriteContext { store, types, by: "w".into(), committer: None, session_id: None }
}

fn add_principal(store: &Store, types: &TypeSet, slug: &str) -> String {
    ops::add(&ctx(store, types), AddArgs {
        type_name: "principal".into(),
        payload: serde_json::json!({"slug": slug, "label": slug}),
        body: b"".to_vec(),
        principal: None,
    })
    .unwrap()
    .record
    .envelope
    .id
}

fn add_view(store: &Store, types: &TypeSet, slug: &str, filter: serde_json::Value) -> String {
    ops::add(&ctx(store, types), AddArgs {
        type_name: "view".into(),
        payload: serde_json::json!({"slug": slug, "title": slug, "filter": filter}),
        body: b"".to_vec(),
        principal: None,
    })
    .unwrap()
    .record
    .envelope
    .id
}

fn add_task(store: &Store, types: &TypeSet, project: &str, slug: &str) -> String {
    ops::add(&ctx(store, types), AddArgs {
        type_name: "task".into(),
        payload: serde_json::json!({"project": project, "slug": slug, "title": slug, "status": "todo"}),
        body: b"".to_vec(),
        principal: None,
    })
    .unwrap()
    .record
    .envelope
    .id
}

// ---------- 台账：显式建立 / 幂等键 / 接手（D13 / 红线25） ----------

#[test]
fn start_creates_ledger_with_watermark_and_is_idempotent() {
    let (_t, store, _types) = fresh_store();
    // 先写一条记录，让事件日志有水位。
    let log = EventLog::new(&store);
    log.append_simple("x", "records/x.md", mem_core::event::Op::Create, None, Some(1), None, Some("d"), "t").unwrap();
    let principal = "0198p";
    let o1 = session::start(&store, StartArgs {
        principal: principal.into(),
        view: Some("view-backend".into()),
        task_ref: Some("task-1".into()),
        handoff: HandoffSpec::None,
        idle_secs: 86_400,
    })
    .unwrap();
    assert_eq!(o1.ledger.watermark, 1, "水位 = 建立时事件序号");
    assert_eq!(o1.ledger.status, session::Status::Active);
    assert!(o1.resumed_from.is_none());
    // 幂等键 = principal + view + task_ref：同键 start 返回既有台账。
    let o2 = session::start(&store, StartArgs {
        principal: principal.into(),
        view: Some("view-backend".into()),
        task_ref: Some("task-1".into()),
        handoff: HandoffSpec::None,
        idle_secs: 86_400,
    })
    .unwrap();
    assert_eq!(o2.ledger.id, o1.ledger.id, "同键幂等");
    // 不同 task_ref → 新台账（同 view 不同任务的并行会话各得台账，不互串）。
    let o3 = session::start(&store, StartArgs {
        principal: principal.into(),
        view: Some("view-backend".into()),
        task_ref: Some("task-2".into()),
        handoff: HandoffSpec::None,
        idle_secs: 86_400,
    })
    .unwrap();
    assert_ne!(o3.ledger.id, o1.ledger.id);
}

#[test]
fn handoff_auto_picks_same_view_same_task_ref_resumable() {
    let (_t, store, _types) = fresh_store();
    let args = |handoff| StartArgs {
        principal: "0198p".into(),
        view: Some("view-b".into()),
        task_ref: Some("task-9".into()),
        handoff,
        idle_secs: 86_400,
    };
    let s1 = session::start(&store, args(HandoffSpec::None)).unwrap();
    session::flush(&store, &s1.ledger.id, vec!["read-1".into()], None).unwrap();
    // 接手：新台账 + 落盘 handoff_from + 继承水位。
    let s2 = session::start(&store, args(HandoffSpec::Auto)).unwrap();
    assert_eq!(s2.ledger.handoff_from.as_deref(), Some(s1.ledger.id.as_str()));
    assert_eq!(s2.ledger.watermark, s1.ledger.watermark, "继承水位");
    assert_ne!(s2.ledger.id, s1.ledger.id, "接手 = 新台账（R9.6 原件不可侵）");
    // 来源原样保留（flushed）。
    let src = session::get(&store, &s1.ledger.id).unwrap().unwrap();
    assert_eq!(src.status, session::Status::Flushed);
    assert_eq!(src.read_trail, vec!["read-1".to_string()], "flush 提交读通道");
}

#[test]
fn handoff_never_crosses_task_ref_but_falls_back_to_null() {
    let (_t, store, _types) = fresh_store();
    let base = StartArgs {
        principal: "0198p".into(),
        view: Some("view-b".into()),
        task_ref: Some("task-A".into()),
        handoff: HandoffSpec::None,
        idle_secs: 86_400,
    };
    let s1 = session::start(&store, base.clone()).unwrap();
    session::flush(&store, &s1.ledger.id, vec![], None).unwrap();
    // task_ref 不同 → 永不接手（跨任务不误接）。
    let s2 = session::start(&store, StartArgs {
        task_ref: Some("task-B".into()),
        handoff: HandoffSpec::Auto,
        ..base.clone()
    })
    .unwrap();
    assert!(s2.ledger.handoff_from.is_none(), "task_ref 不同永不接手");
    // 无相等 task_ref 可接 → 回退到同 view 且 task_ref 为空的可接续台账。
    let s3 = session::start(&store, StartArgs {
        task_ref: None,
        handoff: HandoffSpec::None,
        ..base.clone()
    })
    .unwrap();
    session::flush(&store, &s3.ledger.id, vec![], None).unwrap();
    let s4 = session::start(&store, StartArgs {
        task_ref: Some("task-C".into()),
        handoff: HandoffSpec::Auto,
        ..base.clone()
    })
    .unwrap();
    assert_eq!(s4.ledger.handoff_from.as_deref(), Some(s3.ledger.id.as_str()), "删除跨任务回退：相等 > 为空 > 永不接");
}

#[test]
fn active_session_is_not_auto_handoffable_but_listed_as_peer() {
    let (_t, store, _types) = fresh_store();
    let args = StartArgs {
        principal: "0198p".into(),
        view: Some("view-b".into()),
        task_ref: Some("task-1".into()),
        handoff: HandoffSpec::None,
        idle_secs: 86_400,
    };
    let s1 = session::start(&store, args.clone()).unwrap();
    // 另一会话（不同 task_ref，避免同键幂等命中）尝试接手：活跃会话（未 flush、
    // 未超空闲）不可自动接手，但以 peers 摘要呈现。
    let s2 = session::start(&store, StartArgs {
        task_ref: Some("task-2".into()),
        handoff: HandoffSpec::Auto,
        ..args
    })
    .unwrap();
    assert!(s2.ledger.handoff_from.is_none());
    assert!(s2.peers.iter().any(|p| p.id == s1.ledger.id), "活跃会话以 peers 摘要呈现");
}

#[test]
fn write_path_fallback_creates_ledger_and_registers_trail() {
    let (_t, store, types) = fresh_store();
    let principal = add_principal(&store, &types, "codex_main");
    // 写路径兜底：无台账时建（D13.1）。
    let ledger = session::ensure_for_write(&store, &principal, None, None).unwrap();
    let mut c = ctx(&store, &types);
    c.session_id = Some(&ledger.id);
    let out = ops::add(&c, AddArgs {
        type_name: "memory".into(),
        payload: serde_json::json!({"title": "m"}),
        body: b"".to_vec(),
        principal: Some(principal),
    })
    .unwrap();
    // trail 写通道登记。
    let updated = session::get(&store, &ledger.id).unwrap().unwrap();
    assert!(updated.trail.contains(&out.record.envelope.id), "{:?}", updated.trail);
    // 纯读 brief 不在此测试（CLI 层）；台账创建触发器没有一个是读命令。
}

// ---------- 视图：声明式查询模板（D10，只依赖信封 R8.4） ----------

fn seed_for_view(store: &Store, types: &TypeSet) -> (String, String, String, String) {
    let proj1 = "0198f7a9-0000-7cd2-9e3f-000000000001";
    let proj2 = "0198f7a9-0000-7cd2-9e3f-000000000002";
    let t1 = add_task(store, types, proj1, "alpha");
    let t2 = add_task(store, types, proj1, "beta");
    let t3 = add_task(store, types, proj2, "gamma");
    let m1 = ops::add(&ctx(store, types), AddArgs {
        type_name: "memory".into(),
        payload: serde_json::json!({"title": "一条记忆"}),
        body: b"".to_vec(),
        principal: None,
    })
    .unwrap()
    .record
    .envelope
    .id;
    (t1, t2, t3, m1)
}

#[test]
fn view_filters_by_type_and_ref_envelope_only() {
    let (_t, store, types) = fresh_store();
    let (t1, t2, _t3, _m1) = seed_for_view(&store, &types);
    let view_id = add_view(&store, &types, "proj1-tasks", serde_json::json!({
        "types": ["task"],
        "refs": [{"rel": "project", "target": "0198f7a9-0000-7cd2-9e3f-000000000001"}]
    }));
    // task 的 payload.project 是 x-ref 字段——机制写路径把它落进 envelope.refs
    // （belongs_to）？不——project 是 payload 字段；引用进 envelope 只在显式 link 时。
    // 视图按信封引用过滤 → 此视图匹配 0 条（project 不在信封）。
    // 为了让"类型 × 引用"可过滤，跨类型通用操作只依赖信封：x-ref 字段同时镜像进
    // envelope.refs（belong 关系是信封级引用，见 ops.add 的镜像纪律）。
    let items = view::execute(&store, &view_id, 100).unwrap();
    let ids: Vec<_> = items.items.iter().map(|i| i.envelope.id.clone()).collect();
    assert!(ids.contains(&t1) && ids.contains(&t2) && !ids.contains(&_t3), "{ids:?}");
}

#[test]
fn view_filter_by_principal_and_since() {
    let (_t, store, types) = fresh_store();
    let principal = add_principal(&store, &types, "codex_main");
    let _m1 = ops::add(&ctx(&store, &types), AddArgs {
        type_name: "memory".into(),
        payload: serde_json::json!({"title": "有主"}),
        body: b"".to_vec(),
        principal: Some(principal.clone()),
    })
    .unwrap();
    let _m2 = ops::add(&ctx(&store, &types), AddArgs {
        type_name: "memory".into(),
        payload: serde_json::json!({"title": "无主"}),
        body: b"".to_vec(),
        principal: None,
    })
    .unwrap();
    let view_id = add_view(&store, &types, "mine", serde_json::json!({
        "principals": [principal],
        "types": ["memory"]
    }));
    let items = view::execute(&store, &view_id, 100).unwrap();
    assert_eq!(items.items.len(), 1);
    assert_eq!(items.items[0].envelope.principal.as_deref(), Some(principal.as_str()));
}

// ---------- 预算：截断标注 + 续读（R4.2 / 红线9） ----------

#[test]
fn budget_truncates_with_annotation_and_resume() {
    let (_t, store, types) = fresh_store();
    for i in 0..5 {
        add_task(&store, &types, "0198f7a9-0000-7cd2-9e3f-000000000001", &format!("budget-{i}"));
    }
    let view_id = add_view(&store, &types, "all-tasks", serde_json::json!({"types": ["task"]}));
    // limit=2 → 截断 + 续读路径。
    let p1 = view::execute(&store, &view_id, 2).unwrap();
    assert_eq!(p1.items.len(), 2);
    assert!(p1.truncated, "截断必须显式标注（红线9）");
    assert!(p1.resume_hint.is_some(), "必须给出继续读取的方式");
    // 续读取到剩余。
    let offset = p1.resume_hint.as_ref().unwrap().offset;
    let p2 = view::execute_from(&store, &view_id, 100, offset).unwrap();
    assert_eq!(p2.items.len(), 3);
    assert!(!p2.truncated);
}

// ---------- 引用图：反向索引 / DAG 审计 / 未消化证据（D16/R9） ----------

#[test]
fn reverse_refs_find_dependents() {
    let (_t, store, types) = fresh_store();
    let a = add_task(&store, &types, "0198f7a9-0000-7cd2-9e3f-000000000001", "rev-a");
    let b = add_task(&store, &types, "0198f7a9-0000-7cd2-9e3f-000000000001", "rev-b");
    let c = ctx(&store, &types);
    let ta = ops::get(&store, &a).unwrap().token;
    let tb = ops::get(&store, &b).unwrap().token;
    ops::link(&c, &b, &a, "derived_from", &tb, &ta).unwrap();
    let dependents = refgraph::reverse_refs(&store, &a).unwrap();
    assert_eq!(dependents.len(), 1);
    assert_eq!(dependents[0].0, b, "谁依赖我");
    assert_eq!(dependents[0].1, "derived_from");
}

#[test]
fn dag_audit_reports_cycles_only_for_xdag_types() {
    let (_t, store, types) = fresh_store();
    let a = add_task(&store, &types, "0198f7a9-0000-7cd2-9e3f-000000000001", "dag-a");
    let b = add_task(&store, &types, "0198f7a9-0000-7cd2-9e3f-000000000001", "dag-b");
    let c = ctx(&store, &types);
    let ta = ops::get(&store, &a).unwrap().token;
    let tb = ops::get(&store, &b).unwrap().token;
    ops::link(&c, &b, &a, "derived_from", &tb, &ta).unwrap();
    // task 非 x-dag 类型：环（若有）由 agenda 报告，不在写路径强制。
    let cycles = refgraph::audit_dag(&store, &types).unwrap();
    assert!(cycles.is_empty(), "{cycles:?}");
}

#[test]
fn undigested_evidence_query_per_product() {
    let (_t, store, types) = fresh_store();
    let c = ctx(&store, &types);
    // 证据 e1（先落盘），产物 p 引用 e1，cutoff=当时的 seq。
    let e1 = ops::add(&c, AddArgs {
        type_name: "memory".into(),
        payload: serde_json::json!({"title": "证据1"}),
        body: b"".to_vec(),
        principal: None,
    })
    .unwrap();
    let cutoff = EventLog::new(&store).max_seq().unwrap();
    // 产物（lesson 类型 x-dag，derived_from 指回证据）。
    let p = ops::add(&c, AddArgs {
        type_name: "lesson".into(),
        payload: serde_json::json!({"slug": "lesson-1", "title": "原则"}),
        body: b"".to_vec(),
        principal: None,
    })
    .unwrap();
    let te = ops::get(&store, &e1.record.envelope.id).unwrap().token;
    let tp = ops::get(&store, &p.record.envelope.id).unwrap().token;
    ops::link(&c, &p.record.envelope.id, &e1.record.envelope.id, "derived_from", &tp, &te).unwrap();
    // 给产物打证据截止。
    let p_cur = ops::get(&store, &p.record.envelope.id).unwrap();
    let mut env = p_cur.record.envelope.clone();
    env.evidence_cutoff_seq = Some(cutoff);
    let bytes = mem_core::record::serialize_new(&env, &p_cur.record.payload, &p_cur.record.body);
    std::fs::write(store.abs(&p_cur.rel), &bytes).unwrap();
    // 新证据 e2 落盘（seq 超过 cutoff）。
    let _e2 = ops::add(&c, AddArgs {
        type_name: "memory".into(),
        payload: serde_json::json!({"title": "证据2"}),
        body: b"".to_vec(),
        principal: None,
    })
    .unwrap();
    let undigested = refgraph::undigested_evidence(&store, &p.record.envelope.id).unwrap();
    assert_eq!(undigested.len(), 0, "证据1 在 cutoff 之前 → 已消化（cutoff={cutoff}）");
    // 把 cutoff 回拨到 0 → 证据1 变成未消化。
    let p_cur2 = ops::get(&store, &p.record.envelope.id).unwrap();
    let mut env2 = p_cur2.record.envelope.clone();
    env2.evidence_cutoff_seq = Some(0);
    let bytes2 = mem_core::record::serialize_new(&env2, &p_cur2.record.payload, &p_cur2.record.body);
    std::fs::write(store.abs(&p_cur2.rel), &bytes2).unwrap();
    let undigested2 = refgraph::undigested_evidence(&store, &p.record.envelope.id).unwrap();
    assert_eq!(undigested2.len(), 1, "R9.4：增量再提炼——逐项比较，非全库 diff");
    assert_eq!(undigested2[0], e1.record.envelope.id);
}

#[test]
fn envelope_ref_helpers_roundtrip() {
    let r = Ref { rel: "derived_from".into(), target: "0198x".into() };
    let v = serde_json::to_value(&r).unwrap();
    assert_eq!(v["rel"], "derived_from");
    let _e = Envelope::from_json(&serde_json::json!({
        "id": "0198f7a2-b4c1-7cd2-9e3f-1a2b3c4d5e6f", "type": "task",
        "created": "2026-01-01T00:00:00Z", "modified": "2026-01-01T00:00:00Z",
        "version": 1, "principal": null, "refs": [], "aliases": [],
        "evidence_cutoff_seq": null, "commit": null
    }))
    .unwrap();
    let _ = ViewFilter::default();
}

// ---------- r6 B7：trace 深度截断必须如实标注 ----------

#[test]
fn trace_depth_truncation_is_flagged_not_silent() {
    // r6 复现：A→B→C（up 方向 A derived_from B derived_from C），
    // --depth 1 只返回 B——此前 truncated:false / resume:null（静默截断）。
    let (_t, store, types) = fresh_store();
    let c = ctx(&store, &types);
    let mk = |slug: &str| {
        ops::add(&c, ops::AddArgs {
            type_name: "lesson".into(),
            payload: serde_json::json!({"slug": slug, "title": slug}),
            body: b"".to_vec(),
            principal: None,
        })
        .unwrap()
    };
    let a = mk("tr-a");
    let b = mk("tr-b");
    let cc = mk("tr-c");
    let (ta, tb) = (a.token.clone(), b.token.clone());
    // A derived_from B（A 的 refs 指向 B）；B derived_from C。
    let _r1 = ops::link(&c, &a.record.envelope.id, &b.record.envelope.id, "derived_from", &ta, &tb)
        .expect("link A→B 必须成功");
    let b2 = ops::get(&store, &b.record.envelope.id).unwrap();
    let _r2 = ops::link(&c, &b.record.envelope.id, &cc.record.envelope.id, "derived_from", &b2.token, &cc.token)
        .expect("link B→C 必须成功");
    // 方向确认：up = 沿 refs 找目标。A 的 refs 里有 derived_from→B。
    let a2 = ops::get(&store, &a.record.envelope.id).unwrap();
    assert!(a2.record.envelope.refs.iter().any(|e| e.target == b.record.envelope.id), "A→B 边存在");
    // depth=1：只到 B；C 被深度截断。
    let outcome = mem_core::refgraph::trace(&store, &a.record.envelope.id,
        mem_core::refgraph::TraceDirection::Up, 1, 200).unwrap();
    let (nodes, truncated, frontier) = (&outcome.nodes, outcome.truncated, &outcome.frontier);
    assert_eq!(nodes.len(), 1, "深度 1 只含 B：{nodes:?}");
    assert!(truncated, "深度截断必须如实标注（r6 B7）：{nodes:?}");
    // r9：深度截断必须给出可执行续读的 frontier（被挡住的 C——
    // 对每个 frontier 节点 `trace <id> --dir up --depth 1` 可继续深入）。
    assert_eq!(*frontier, vec![cc.record.envelope.id.clone()],
        "frontier = 深度挡住的未访问邻居（r9 可执行续读）：{frontier:?}");
    // depth=4（默认）：全链可见，不截断。
    let outcome2 = mem_core::refgraph::trace(&store, &a.record.envelope.id,
        mem_core::refgraph::TraceDirection::Up, 4, 200).unwrap();
    let (nodes2, truncated2) = (&outcome2.nodes, outcome2.truncated);
    assert_eq!(nodes2.len(), 2, "深度 4 应含 B 和 C：{nodes2:?}");
    assert!(!truncated2);
}
