//! §8-4d 写路径测试：add（唯一键原子独占）/ update（全摘要令牌 CAS）/ link / delete /
//! restore / merge / git 同批提交（commit-pending）。锚点：D3/D11/D17、红线19/22/24、R3.3。

use std::sync::Arc;

use mem_core::error::MemError;
use mem_core::event::{EventLog, Op};
use mem_core::ops::{self, AddArgs, CommitSink, WriteContext};
use mem_core::store::{OpenMode, Store};
use mem_core::token::{self};
use mem_schema::TypeSet;

fn fresh_store() -> (tempfile::TempDir, Store, TypeSet) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("store");
    Store::create(&root).unwrap();
    // 播种类型（用仓库资产的真实两件套）。
    let types_dir = root.join("types");
    for (p, bytes) in mem_assets_bytes() {
        let abs = root.join(&p);
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(abs, bytes).unwrap();
    }
    // 追加一个 x-dag 测试类型（lesson：派生类强制无环）。
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
    let types = TypeSet::load_dir(&types_dir).unwrap();
    (tmp, store, types)
}

/// 直接读仓库资产（测试夹具；机制路径仍走数据目录副本）。
fn mem_assets_bytes() -> Vec<(String, Vec<u8>)> {
    let root =
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
    let mut out = Vec::new();
    walk(&root, "", &mut out);
    out.sort();
    out
}

#[derive(Default, Clone)]
struct RecordingSink {
    batches: Arc<std::sync::Mutex<Vec<Vec<String>>>>,
    fail: bool,
}

impl CommitSink for RecordingSink {
    fn commit(&self, paths: &[String]) -> mem_core::error::MemResult<Option<String>> {
        if self.fail {
            return Err(MemError::Git("模拟 index.lock 冲突耗尽".into()));
        }
        self.batches.lock().unwrap().push(paths.to_vec());
        Ok(Some("deadbeef".into()))
    }
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
    WriteContext {
        store,
        types,
        by: "0198f7b2(zcode_main)".into(),
        committer: None,
        session_id: None,
    }
}

fn task_payload(project: &str, slug: &str, status: &str) -> serde_json::Value {
    serde_json::json!({"project": project, "slug": slug, "title": "实现登录接口", "status": status})
}

// ---------- add：创建 / 唯一键 / 校验 ----------

#[test]
fn add_creates_v1_at_derived_path_with_create_event() {
    let (_t, store, types) = fresh_store();
    let c = ctx(&store, &types);
    let out = ops::add(&c, AddArgs {
        type_name: "task".into(),
        payload: task_payload("0198f7a9-0000-7cd2-9e3f-000000000001", "login-api", "todo"),
        body: "# 实现登录接口\n".as_bytes().to_vec(),
        principal: None,
    })
    .unwrap();
    assert!(out.created);
    assert_eq!(out.record.envelope.version, 1);
    assert_eq!(out.token.version, 1);
    assert_eq!(out.token.digest.len(), 64);
    assert!(out.rel.starts_with("records/task/0198f7a9-0000-7cd2-9e3f-000000000001/login-api.md"), "{}", out.rel);
    // 文件在磁盘上，内容往返一致。
    let bytes = std::fs::read(store.abs(&out.rel)).unwrap();
    assert_eq!(token::sha256_hex(&bytes), out.token.digest);
    // 事件：create。
    let log = EventLog::new(&store).read_log().unwrap();
    assert_eq!(log.events.len(), 1);
    assert_eq!(log.events[0].op, Op::Create);
    assert_eq!(log.events[0].path, out.rel);
    assert_eq!(log.events[0].new_v, Some(1));
}

#[test]
fn add_loser_gets_existing_record_and_token() {
    let (_t, store, types) = fresh_store();
    let c = ctx(&store, &types);
    let args = || AddArgs {
        type_name: "task".into(),
        payload: task_payload("0198f7a9-0000-7cd2-9e3f-000000000001", "login-api", "todo"),
        body: b"x".to_vec(),
        principal: None,
    };
    let w1 = ops::add(&c, args()).unwrap();
    let w2 = ops::add(&c, args()).unwrap();
    assert!(w1.created);
    assert!(!w2.created, "输者协议：返回既有记录及令牌（D11）");
    assert_eq!(w1.record.envelope.id, w2.record.envelope.id);
    assert_eq!(w1.token, w2.token);
    assert_eq!(EventLog::new(&store).read_log().unwrap().events.len(), 1, "只产生一次 create");
}

#[test]
fn concurrent_double_add_single_winner() {
    let (_t, store, _types) = fresh_store();
    let root = store.root().to_path_buf();
    let types_dir = root.join("types");
    let mut handles = Vec::new();
    for i in 0..8 {
        let root = root.clone();
        let types_dir = types_dir.clone();
        handles.push(std::thread::spawn(move || {
            let s = Store::open(&root, OpenMode::Auto).unwrap();
            let ts = TypeSet::load_dir(&types_dir).unwrap();
            let c = WriteContext {
                store: &s,
                types: &ts,
                by: format!("w{i}"),
                committer: None,
                session_id: None,
            };
            ops::add(&c, AddArgs {
                type_name: "task".into(),
                payload: task_payload("0198f7a9-0000-7cd2-9e3f-000000000001", "race", "todo"),
                body: b"x".to_vec(),
                principal: None,
            })
            .unwrap()
        }));
    }
    let outs: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let created = outs.iter().filter(|o| o.created).count();
    assert_eq!(created, 1, "R3.3：并发创建唯一性由基础设施强制");
    let ids: std::collections::HashSet<_> = outs.iter().map(|o| o.record.envelope.id.clone()).collect();
    assert_eq!(ids.len(), 1, "输者拿到的都是同一条既有记录");
    assert_eq!(EventLog::new(&store).read_log().unwrap().events.len(), 1);
}

#[test]
fn add_strict_rejects_bad_payload_with_field_paths() {
    let (_t, store, types) = fresh_store();
    let c = ctx(&store, &types);
    let err = ops::add(&c, AddArgs {
        type_name: "task".into(),
        payload: serde_json::json!({"slug": "x", "status": "不存在的状态", "title": "t"}),
        body: b"".to_vec(),
        principal: None,
    })
    .unwrap_err();
    match err {
        MemError::SchemaReject { type_name, violations } => {
            assert_eq!(type_name, "task");
            assert!(violations.iter().any(|v| v.path.contains("status")), "{violations:?}");
        }
        e => panic!("{e}"),
    }
}

#[test]
fn add_lenient_tolerates_unknown_fields_with_warning() {
    let (_t, store, types) = fresh_store();
    let c = ctx(&store, &types);
    let out = ops::add(&c, AddArgs {
        type_name: "memory".into(),
        payload: serde_json::json!({"title": "记忆", "surprise_field": 42}),
        body: "内容".as_bytes().to_vec(),
        principal: None,
    })
    .unwrap();
    assert!(out.created);
    assert!(!out.warnings.is_empty(), "lenient 未知字段必须降级告警（R8.6）");
}

#[test]
fn add_enforces_ref_existence_and_principal_existence() {
    let (_t, store, types) = fresh_store();
    let c = ctx(&store, &types);
    // x-ref 目标不存在 → 拒收（R8.5）。
    let err = ops::add(&c, AddArgs {
        type_name: "task".into(),
        payload: task_payload("0198f7a9-0000-7cd2-9e3f-000000000099", "t1", "todo"),
        body: b"".to_vec(),
        principal: None,
    })
    .unwrap_err();
    assert!(matches!(err, MemError::RefTargetMissing { .. }), "{err}");
    // principal 存在性强制（红线12）：先建 principal。
    let p = ops::add(&c, AddArgs {
        type_name: "principal".into(),
        payload: serde_json::json!({"slug": "codex_main", "label": "codex_main"}),
        body: b"".to_vec(),
        principal: None,
    })
    .unwrap();
    // 引用 principal 记录本身做归属。
    let task = ops::add(&c, AddArgs {
        type_name: "memory".into(),
        payload: serde_json::json!({"title": "m"}),
        body: b"".to_vec(),
        principal: Some(p.record.envelope.id.clone()),
    })
    .unwrap();
    assert_eq!(task.record.envelope.principal.as_deref(), Some(p.record.envelope.id.as_str()));
    // 不存在的 principal → 拒收。
    let err = ops::add(&c, AddArgs {
        type_name: "memory".into(),
        payload: serde_json::json!({"title": "m2"}),
        body: b"".to_vec(),
        principal: Some("0198ghost-0000-7cd2-9e3f-000000000001".into()),
    })
    .unwrap_err();
    assert!(matches!(err, MemError::RefTargetMissing { .. }), "{err}");
}

// ---------- update：全摘要令牌 CAS（红线19） ----------

fn add_task(store: &Store, types: &TypeSet, slug: &str) -> ops::Added {
    let c = ctx(store, types);
    ops::add(&c, AddArgs {
        type_name: "task".into(),
        payload: task_payload("0198f7a9-0000-7cd2-9e3f-000000000001", slug, "todo"),
        body: b"v1 body".to_vec(),
        principal: None,
    })
    .unwrap()
}

#[test]
fn update_with_valid_token_bumps_version_and_logs_event() {
    let (_t, store, types) = fresh_store();
    let added = add_task(&store, &types, "t-update");
    let mut payload = task_payload("0198f7a9-0000-7cd2-9e3f-000000000001", "t-update", "in_progress");
    payload["title"] = serde_json::json!("改标题");
    let c = ctx(&store, &types);
    let out = ops::update(&c, &added.record.envelope.id, &added.token, payload, Some(b"v2 body".to_vec())).unwrap();
    assert_eq!(out.record.envelope.version, 2);
    assert_eq!(out.record.payload["status"], "in_progress");
    assert_eq!(out.record.body, b"v2 body");
    // 事件：update，新旧摘要真实。
    let last = EventLog::new(&store).last_event_for(&added.record.envelope.id).unwrap().unwrap();
    assert_eq!(last.op, Op::Update);
    assert_eq!(last.old_v, Some(1));
    assert_eq!(last.new_v, Some(2));
    assert_eq!(last.new_digest.as_deref(), Some(out.token.digest.as_str()));
    // 正文按字节保持：v2 body 原样落盘。
    let bytes = std::fs::read(store.abs(&out.rel)).unwrap();
    assert!(window_contains(&bytes, b"v2 body"));
}

fn window_contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn update_conflict_is_structured_with_current_token() {
    let (_t, store, types) = fresh_store();
    let added = add_task(&store, &types, "t-conflict");
    // 另一个写入者先完成一次 update。
    let c = ctx(&store, &types);
    let payload = task_payload("0198f7a9-0000-7cd2-9e3f-000000000001", "t-conflict", "in_progress");
    let _ = ops::update(&c, &added.record.envelope.id, &added.token, payload.clone(), None).unwrap();
    // 陈旧令牌的 update → 结构化冲突，含当前令牌（D3/R3.2）。
    let err = ops::update(&c, &added.record.envelope.id, &added.token, payload, None).unwrap_err();
    match err {
        MemError::Conflict { expected, current } => {
            assert_eq!(expected, added.token);
            assert_eq!(current.version, 2, "冲突结果携带当前令牌供重读重放");
        }
        e => panic!("{e}"),
    }
    // 文件未被陈旧写入破坏。
    let cur = ops::get(&store, &added.record.envelope.id).unwrap();
    assert_eq!(cur.record.envelope.version, 2);
}

#[test]
fn update_detects_version_preserved_hand_edit() {
    let (_t, store, types) = fresh_store();
    let added = add_task(&store, &types, "t-hand");
    // 保版本手改：直接重写文件，version 不变。
    let bytes = std::fs::read(store.abs(&added.rel)).unwrap();
    let rec = mem_core::record::Record::from_bytes(&bytes).unwrap();
    let tampered = mem_core::record::serialize_new(&rec.envelope, &rec.payload, "手改正文".as_bytes());
    std::fs::write(store.abs(&added.rel), tampered).unwrap();
    let c = ctx(&store, &types);
    let err = ops::update(&c, &added.record.envelope.id, &added.token, task_payload("0198f7a9-0000-7cd2-9e3f-000000000001", "t-hand", "done"), None).unwrap_err();
    assert!(matches!(err, MemError::Conflict { .. }), "保版本手改必被全摘要令牌检出：{err}");
    // 手改保留（不被静默覆盖）。
    let cur = std::fs::read(store.abs(&added.rel)).unwrap();
    assert!(window_contains(&cur, "手改正文".as_bytes()));
}

#[test]
fn update_rejects_unique_key_change() {
    let (_t, store, types) = fresh_store();
    let added = add_task(&store, &types, "t-move");
    let c = ctx(&store, &types);
    let err = ops::update(&c, &added.record.envelope.id, &added.token, task_payload("0198f7a9-0000-7cd2-9e3f-000000000001", "moved-away", "todo"), None).unwrap_err();
    assert!(matches!(err, MemError::UniqueKeyImmut { .. }), "移动语义 v1 拒收：{err}");
}

// ---------- link / DAG ----------

#[test]
fn link_requires_both_tokens_and_logs_events() {
    let (_t, store, types) = fresh_store();
    let a = add_task(&store, &types, "t-a");
    let b = add_task(&store, &types, "t-b");
    let c = ctx(&store, &types);
    let (ta, tb) = ops::link(&c, &a.record.envelope.id, &b.record.envelope.id, "derived_from", &a.token, &b.token).unwrap();
    assert_eq!(ta.version, 2, "源记录写边 → 版本 +1");
    assert_eq!(tb, b.token, "目标仅 CAS 校验，不写不改版本");
    let rec = ops::get(&store, &a.record.envelope.id).unwrap();
    assert!(rec.record.envelope.refs.iter().any(|r| r.rel == "derived_from" && r.target == b.record.envelope.id));
    let rec_b = ops::get(&store, &b.record.envelope.id).unwrap();
    assert!(!rec_b.record.envelope.refs.iter().any(|r| r.rel == "derived_from"), "目标不加边（双向落边会凭空成环）");
    // 陈旧令牌 → 冲突（两次单记录 CAS）。
    let err = ops::link(&c, &a.record.envelope.id, &b.record.envelope.id, "relates", &a.token, &b.token).unwrap_err();
    assert!(matches!(err, MemError::Conflict { .. }));
}

#[test]
fn dag_cycle_rejected_for_xdag_type() {
    let (_t, store, types) = fresh_store();
    let c = ctx(&store, &types);
    let mk = |slug: &str| ops::add(&c, AddArgs {
        type_name: "lesson".into(),
        payload: serde_json::json!({"slug": slug, "title": slug}),
        body: b"".to_vec(),
        principal: None,
    })
    .unwrap();
    let a = mk("l-a");
    let b = mk("l-b");
    let cc = mk("l-c");
    let (ta, tb) = (a.token.clone(), b.token.clone());
    // link 返回 (第一个参数的新令牌, 第二个参数的新令牌)。
    let (tb, _ta2) = ops::link(&c, &b.record.envelope.id, &a.record.envelope.id, "derived_from", &tb, &ta).unwrap();
    let (tc, _tb2) = ops::link(&c, &cc.record.envelope.id, &b.record.envelope.id, "derived_from", &cc.token, &tb).unwrap();
    // a → c 构成环 → 拒收（D16/红线17）。
    let a2 = ops::get(&store, &a.record.envelope.id).unwrap();
    let err = ops::link(&c, &a.record.envelope.id, &cc.record.envelope.id, "derived_from", &a2.token, &tc).unwrap_err();
    assert!(matches!(err, MemError::CycleDetected(_)), "环必须被拒收：{err}");
}

// ---------- delete / restore ----------

#[test]
fn delete_blocked_by_reverse_refs_then_force_to_attic_and_restore() {
    let (_t, store, types) = fresh_store();
    let a = add_task(&store, &types, "t-del-a");
    let b = add_task(&store, &types, "t-del-b");
    let c = ctx(&store, &types);
    let _ = ops::link(&c, &b.record.envelope.id, &a.record.envelope.id, "derived_from", &b.token, &a.token).unwrap();
    // b 引用 a → 删除 a 默认阻止（R3.4/D17）。
    let a2 = ops::get(&store, &a.record.envelope.id).unwrap();
    let err = ops::delete_record(&c, &a.record.envelope.id, &a2.token, false).unwrap_err();
    assert!(matches!(err, MemError::RefTargetMissing { .. }) || err.to_string().contains("引用"), "{err}");
    // force：进入 attic（受管 tombstone）+ delete 事件。
    let out = ops::delete_record(&c, &a.record.envelope.id, &a2.token, true).unwrap();
    assert!(out.rel.starts_with("attic/"), "{}", out.rel);
    assert!(!store.abs("records/task/0198f7a9-0000-7cd2-9e3f-000000000001/t-del-a.md").exists());
    let last = EventLog::new(&store).last_event_for(&a.record.envelope.id).unwrap().unwrap();
    assert_eq!(last.op, Op::Delete);
    // restore：回 records 同路径 + restore 事件。
    let out2 = ops::restore_record(&c, &a.record.envelope.id, None).unwrap();
    assert!(out2.rel.starts_with("records/"));
    let last2 = EventLog::new(&store).last_event_for(&a.record.envelope.id).unwrap().unwrap();
    assert_eq!(last2.op, Op::Restore);
}

#[test]
fn r15_reused_record_key_never_overwrites_older_tombstone() {
    let (_t, store, types) = fresh_store();
    let c = ctx(&store, &types);
    let first = add_task(&store, &types, "r15-reuse");
    let first_id = first.record.envelope.id.clone();
    ops::delete_record(&c, &first_id, &first.token, false).unwrap();
    let attic = "attic/task/0198f7a9-0000-7cd2-9e3f-000000000001/r15-reuse.md";
    let preserved = std::fs::read(store.abs(attic)).unwrap();
    let second = add_task(&store, &types, "r15-reuse");
    assert_ne!(first_id, second.record.envelope.id);
    let err = ops::delete_record(&c, &second.record.envelope.id, &second.token, false).unwrap_err();
    assert!(err.to_string().contains("File exists") || err.to_string().contains("already exists") || err.to_string().contains("已存在"), "{err}");
    assert_eq!(std::fs::read(store.abs(attic)).unwrap(), preserved);
    assert_eq!(mem_core::record::Record::from_bytes(&preserved).unwrap().envelope.id, first_id);
}

#[test]
fn r15_merge_never_overwrites_an_existing_different_tombstone() {
    let (_t, store, types) = fresh_store();
    let c = ctx(&store, &types);
    let old = add_task(&store, &types, "r15-merge-reuse");
    let attic = "attic/task/0198f7a9-0000-7cd2-9e3f-000000000001/r15-merge-reuse.md";
    ops::delete_record(&c, &old.record.envelope.id, &old.token, false).unwrap();
    let preserved = std::fs::read(store.abs(attic)).unwrap();
    let dup = add_task(&store, &types, "r15-merge-reuse");
    let canonical = add_task(&store, &types, "r15-merge-canonical");
    let dependent = ops::add(&c, AddArgs {
        type_name: "memory".into(),
        payload: serde_json::json!({"title": "points to dup"}),
        body: b"dependent".to_vec(),
        principal: None,
    }).unwrap();
    // 构造已存在的引用边，专门验证 merge 失败时第三方文件不被提前重写。
    let mut dep_rec = ops::get(&store, &dependent.record.envelope.id).unwrap().record;
    dep_rec.envelope.refs.push(mem_core::envelope::Ref {
        rel: "test_ref".into(), target: dup.record.envelope.id.clone(),
    });
    let dep_bytes = mem_core::record::serialize_new(&dep_rec.envelope, &dep_rec.payload, &dep_rec.body);
    std::fs::write(store.abs(&dependent.rel), &dep_bytes).unwrap();
    let before_canonical = std::fs::read(store.abs(&canonical.rel)).unwrap();
    let before_dependent = std::fs::read(store.abs(&dependent.rel)).unwrap();
    let before_events = std::fs::read(store.abs("events.jsonl")).unwrap();
    let err = ops::merge(&c, &dup.record.envelope.id, &canonical.record.envelope.id, &dup.token, &canonical.token).unwrap_err();
    assert!(err.to_string().contains("File exists") || err.to_string().contains("already exists") || err.to_string().contains("已存在"), "{err}");
    assert_eq!(std::fs::read(store.abs(attic)).unwrap(), preserved);
    assert!(store.abs(&dup.rel).is_file(), "冲突时 dup 原记录必须仍在 records");
    assert_eq!(std::fs::read(store.abs(&canonical.rel)).unwrap(), before_canonical);
    assert_eq!(std::fs::read(store.abs(&dependent.rel)).unwrap(), before_dependent);
    assert_eq!(std::fs::read(store.abs("events.jsonl")).unwrap(), before_events);
}

#[test]
fn r16_add_revalidates_reference_after_waiting_for_shared_barrier() {
    let (_t, store, types) = fresh_store();
    let c = ctx(&store, &types);
    let target_id = "0198f7a9-0000-7cd2-9e3f-000000000001".to_string();
    let target = ops::get(&store, &target_id).unwrap();
    let gate = mem_core::lock::barrier_exclusive(&store).unwrap();
    let root = store.root().to_path_buf();
    let id = target_id.clone();
    let token = target.token.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let creator = std::thread::spawn(move || {
        let store2 = Store::open(&root, OpenMode::Auto).unwrap();
        let types2 = TypeSet::load_dir(&root.join("types")).unwrap();
        let c2 = ctx(&store2, &types2);
        tx.send(()).unwrap();
        ops::add(&c2, AddArgs {
            type_name: "task".into(),
            payload: task_payload(&id, "r16-reference-race", "todo"),
            body: b"race".to_vec(),
            principal: None,
        })
    });
    rx.recv().unwrap();
    // 在创建方等待共享门闩期间完成删除；之后创建方必须在门闩内重新校验引用。
    ops::delete_locked(&c, &target_id, &token, false).unwrap();
    drop(gate);
    let err = creator.join().unwrap().unwrap_err();
    assert!(matches!(err, MemError::RefTargetMissing { .. }), "创建必须拒绝已删除目标：{err}");
    assert!(!ops::scan_all(&store).unwrap().records.iter()
        .any(|r| r.record.envelope.aliases.iter().any(|a| a == "r16-reference-race")));
}

#[test]
fn r16_fix_rejects_lexical_escape_without_touching_outside_record() {
    let (_t, store, types) = fresh_store();
    let outside = store.root().parent().unwrap().join("outside-fix-r16.md");
    let env = mem_core::envelope::Envelope {
        id: "outside-r16".into(), r#type: "memory".into(),
        created: "2026-11-12T00:00:00Z".into(), modified: "2026-11-12T00:00:00Z".into(),
        version: 1, principal: None, refs: vec![], aliases: vec![], evidence_cutoff_seq: None,
        commit: None, extra: Default::default(),
    };
    let bytes = mem_core::record::serialize_new(&env, &serde_json::json!({"title":"outside"}), b"body");
    std::fs::write(&outside, &bytes).unwrap();
    let err = ops::fix_record(&store, &types, "records/../../outside-fix-r16.md", None,
        None, None, None).unwrap_err();
    assert!(matches!(err, MemError::BadArgument(_)), "路径拒绝必须原样返回：{err}");
    assert_eq!(std::fs::read(&outside).unwrap(), bytes);
    assert!(!outside.with_file_name("outside-fix-r16.md.lock").exists());
    std::fs::remove_file(outside).unwrap();
}

#[cfg(unix)]
#[test]
fn r16_record_path_rejects_symlink_escape() {
    let (_t, store, _types) = fresh_store();
    let outside_dir = tempfile::tempdir().unwrap();
    let outside = outside_dir.path().join("outside.md");
    let env = mem_core::envelope::Envelope {
        id: "0198f7a9-0000-7cd2-9e3f-000000000099".into(), r#type: "memory".into(),
        created: "2026-11-12T00:00:00Z".into(), modified: "2026-11-12T00:00:00Z".into(),
        version: 1, principal: None, refs: vec![], aliases: vec!["outside-alias".into()],
        evidence_cutoff_seq: None, commit: None, extra: Default::default(),
    };
    let bytes = mem_core::record::serialize_new(&env, &serde_json::json!({"title":"outside"}), b"sentinel");
    std::fs::write(&outside, &bytes).unwrap();
    std::os::unix::fs::symlink(outside_dir.path(), store.abs("records/escape")).unwrap();
    let err = ops::get(&store, "records/escape/outside.md").unwrap_err();
    assert!(matches!(err, MemError::BadArgument(_)), "符号链接越界应拒绝：{err}");
    assert!(matches!(ops::get(&store, &env.id), Err(MemError::MissingRecord { .. })));
    assert!(matches!(ops::get(&store, "outside-alias"), Err(MemError::MissingRecord { .. })));
    assert_eq!(std::fs::read(outside).unwrap(), bytes);
}

#[cfg(unix)]
fn add_memory(store: &Store, types: &TypeSet, title: &str) -> ops::Added {
    ops::add(&ctx(store, types), AddArgs {
        type_name: "memory".into(),
        payload: serde_json::json!({"title": title}),
        body: b"memory body".to_vec(),
        principal: None,
    }).unwrap()
}

#[cfg(unix)]
#[test]
fn r17_add_rejects_symlinked_record_destination_before_side_effects() {
    let (_t, store, types) = fresh_store();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), store.abs("records/memory")).unwrap();
    let before_events = std::fs::read(store.abs("events.jsonl")).unwrap();
    let err = ops::add(&ctx(&store, &types), AddArgs {
        type_name: "memory".into(),
        payload: serde_json::json!({"title": "outside-created"}),
        body: b"outside".to_vec(), principal: None,
    }).unwrap_err();
    assert!(matches!(err, MemError::BadArgument(_)), "外链目标必须拒绝：{err}");
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    assert_eq!(std::fs::read(store.abs("events.jsonl")).unwrap(), before_events);
}

#[cfg(unix)]
#[test]
fn r17_delete_rejects_symlinked_attic_destination_without_moving_record() {
    let (_t, store, types) = fresh_store();
    let added = add_memory(&store, &types, "delete-link");
    let record_before = std::fs::read(store.abs(&added.rel)).unwrap();
    let events_before = std::fs::read(store.abs("events.jsonl")).unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), store.abs("attic/memory")).unwrap();
    let err = ops::delete_record(&ctx(&store, &types), &added.record.envelope.id, &added.token, false).unwrap_err();
    assert!(matches!(err, MemError::BadArgument(_)), "外链 tombstone 目标必须拒绝：{err}");
    assert_eq!(std::fs::read(store.abs(&added.rel)).unwrap(), record_before);
    assert_eq!(std::fs::read(store.abs("events.jsonl")).unwrap(), events_before);
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn r17_restore_rejects_symlinked_records_destination_without_moving_tombstone() {
    let (_t, store, types) = fresh_store();
    let c = ctx(&store, &types);
    let added = add_memory(&store, &types, "restore-link");
    let deleted = ops::delete_record(&c, &added.record.envelope.id, &added.token, false).unwrap();
    let tomb_before = std::fs::read(store.abs(&deleted.rel)).unwrap();
    let events_before = std::fs::read(store.abs("events.jsonl")).unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::remove_dir_all(store.abs("records/memory")).unwrap();
    std::os::unix::fs::symlink(outside.path(), store.abs("records/memory")).unwrap();
    let err = ops::restore_record(&c, &added.record.envelope.id, None).unwrap_err();
    assert!(matches!(err, MemError::BadArgument(_)), "外链 records 目标必须拒绝：{err}");
    assert_eq!(std::fs::read(store.abs(&deleted.rel)).unwrap(), tomb_before);
    assert_eq!(std::fs::read(store.abs("events.jsonl")).unwrap(), events_before);
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn r17_update_rejects_symlinked_lock_without_overwriting_external_file() {
    let (_t, store, types) = fresh_store();
    let added = add_memory(&store, &types, "lock-link");
    let lock_path = store.abs(&format!("{}.lock", added.rel));
    std::fs::remove_file(&lock_path).unwrap();
    let outside = tempfile::tempdir().unwrap();
    let external = outside.path().join("unrelated.txt");
    let original = b"valuable unrelated bytes";
    std::fs::write(&external, original).unwrap();
    std::os::unix::fs::symlink(&external, &lock_path).unwrap();
    let record_before = std::fs::read(store.abs(&added.rel)).unwrap();
    let events_before = std::fs::read(store.abs("events.jsonl")).unwrap();
    let err = ops::update(&ctx(&store, &types), &added.record.envelope.id, &added.token,
        serde_json::json!({"title": "changed"}), None).unwrap_err();
    assert!(matches!(err, MemError::BadArgument(_)), "外链锁目标必须拒绝：{err}");
    assert_eq!(std::fs::read(external).unwrap(), original);
    assert_eq!(std::fs::read(store.abs(&added.rel)).unwrap(), record_before);
    assert_eq!(std::fs::read(store.abs("events.jsonl")).unwrap(), events_before);
}

#[test]
fn r15_nonforce_delete_serializes_with_new_reference_creation() {
    let (_t, store, types) = fresh_store();
    let target_id = "0198f7a9-0000-7cd2-9e3f-000000000001".to_string();
    let target = ops::get(&store, &target_id).unwrap();
    let gate = mem_core::lock::barrier_shared(&store).unwrap();
    // 新引用与待运行的删除交错：删除申请独占门闩后，引用已完成在共享门闩内的发布。
    let _dependent = add_task(&store, &types, "r15-racing-dependent");
    let root = store.root().to_path_buf();
    let token = target.token.clone();
    let id = target_id.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let deleter = std::thread::spawn(move || {
        let store2 = Store::open(&root, OpenMode::Auto).unwrap();
        let types2 = TypeSet::load_dir(&root.join("types")).unwrap();
        let c = ctx(&store2, &types2);
        tx.send(()).unwrap();
        ops::delete_record(&c, &id, &token, false)
    });
    rx.recv().unwrap();
    drop(gate);
    let err = deleter.join().unwrap().unwrap_err();
    assert!(err.to_string().contains("引用"), "删除必须被新引用阻止：{err}");
    assert!(store.abs("records/project/0198f7a9-0000-7cd2-9e3f-000000000001.md").exists());
}

#[test]
fn r15_get_rejects_paths_that_escape_store_root() {
    let (tmp, store, _) = fresh_store();
    let outside = tmp.path().join("outside.md");
    std::fs::write(&outside, b"outside sentinel").unwrap();
    let err = ops::get(&store, "records/../../outside.md").unwrap_err();
    assert!(matches!(err, MemError::BadArgument(_)), "应在文件访问前拒绝路径：{err}");
    assert_eq!(std::fs::read(outside).unwrap(), b"outside sentinel");
}

// ---------- git 同批提交 / commit-pending ----------

#[test]
fn commits_include_record_and_events_same_batch() {
    let (_t, store, types) = fresh_store();
    let sink = RecordingSink::default();
    let c = WriteContext {
        store: &store,
        types: &types,
        by: "w".into(),
        committer: Some(&sink),
        session_id: None,
    };
    let out = ops::add(&c, AddArgs {
        type_name: "task".into(),
        payload: task_payload("0198f7a9-0000-7cd2-9e3f-000000000001", "t-git", "todo"),
        body: b"".to_vec(),
        principal: None,
    })
    .unwrap();
    let batches = sink.batches.lock().unwrap();
    assert_eq!(batches.len(), 1, "高优1：记录路径 + events.jsonl 同批提交");
    let batch = &batches[0];
    assert!(batch.contains(&out.rel), "{batch:?}");
    assert!(batch.iter().any(|p| p == "events.jsonl"), "{batch:?}");
}

#[test]
fn commit_failure_becomes_pending_not_rollback() {
    let (_t, store, types) = fresh_store();
    let sink = RecordingSink { batches: Default::default(), fail: true };
    let c = WriteContext {
        store: &store,
        types: &types,
        by: "w".into(),
        committer: Some(&sink),
        session_id: None,
    };
    let out = ops::add(&c, AddArgs {
        type_name: "task".into(),
        payload: task_payload("0198f7a9-0000-7cd2-9e3f-000000000001", "t-pending", "todo"),
        body: b"".to_vec(),
        principal: None,
    })
    .unwrap();
    // 真相层已生效（文件 + 事件），提交滞后合法（D17）。
    assert!(store.abs(&out.rel).is_file());
    assert_eq!(EventLog::new(&store).read_log().unwrap().events.len(), 1);
    // commit-pending 登记，不分配序号重排——事件 seq 已定。
    let pending_dir = store.abs(".pending-commits");
    assert!(pending_dir.is_dir());
    let n = std::fs::read_dir(&pending_dir).unwrap().count();
    assert_eq!(n, 1, "pending 对账文件已登记");
}

// ---------- 读取与扫描（R6.2 隔离） ----------

#[test]
fn scan_skips_corrupt_records_with_warning() {
    let (_t, store, types) = fresh_store();
    let a = add_task(&store, &types, "t-ok");
    // 注入一条坏记录（R6：坏写入是常态输入）。
    let bad_rel = "records/task/0198proj/bad.md";
    std::fs::create_dir_all(store.abs("records/task/0198proj")).unwrap();
    std::fs::write(store.abs(bad_rel), b"---\n{broken json\n---\n").unwrap();
    let scanned = ops::scan_all(&store).unwrap();
    assert!(scanned.records.iter().any(|r| r.record.envelope.id == a.record.envelope.id));
    assert_eq!(scanned.warnings.len(), 1, "坏条目跳过 + 告警，不崩溃（R6.2）");
    // 读路径 get 对坏记录：响亮报告（R6.1），修理由 fix 承担。
    let err = ops::get(&store, bad_rel).unwrap_err();
    assert!(err.to_string().contains("frontmatter") || err.to_string().contains("损坏"), "{err}");
}
