//! 写路径归属绑定验收（方案 principal-binding-design.md §4.2 十组；决策①–⑦）。
//!
//! 覆盖：台账继承、锁外拒绝（全店逐字节一致 + "有 .git 缺 .gitattributes"反例）、
//! 显式胜出 + 中性告警、principal 类型豁免、悬空归属串行反例、悬空归属并发反例
//! （双分支：删除先完成 / 创建先完成）、删除侧归属依赖回归、verify 归属断链审计
//! 三组回归、外部空归属语义、死锁守护。验收形态以方案 §4.2 为真源，本文件不重定义。

use mem_core::error::MemError;
use mem_core::event::PauseHook;
use mem_core::ops::{self, AddArgs, WriteContext};
use mem_core::session::{self, HandoffSpec, StartArgs};
use mem_core::store::{OpenMode, Store};
use mem_schema::TypeSet;
use std::collections::BTreeMap;
use std::sync::mpsc;

// ---------- 夹具 ----------

fn fresh_store() -> (tempfile::TempDir, Store, TypeSet) {
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
            let r = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
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
        let abs = root.join(&p); // 资产根自带 types/ 层级（同 acceptance fresh_store）
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(abs, bytes).unwrap();
    }
    let store = Store::open(&root, OpenMode::Auto).unwrap();
    let types = TypeSet::load_dir(&types_dir).unwrap();
    (tmp, store, types)
}

/// 直写播种身份记录（外部写入者形态，不产生事件；同 seed_project 先例）。
/// slug="test-owner"，固定 id（调用方可直接比对）。
fn seed_owner(store: &Store) -> String {
    let id = "0198f7a9-0000-7000-8000-0000000000a1".to_string();
    write_principal(store, &id, "test-owner");
    id
}

fn write_principal(store: &Store, id: &str, slug: &str) {
    let env = mem_core::envelope::Envelope {
        id: id.to_string(),
        r#type: "principal".into(),
        created: "2026-11-19T00:00:00Z".into(),
        modified: "2026-11-19T00:00:00Z".into(),
        version: 1,
        principal: None,
        refs: vec![],
        aliases: vec![slug.to_string()],
        evidence_cutoff_seq: None,
        commit: None,
        extra: Default::default(),
    };
    let payload = serde_json::json!({"slug": slug, "label": slug});
    let bytes = mem_core::record::serialize_new(&env, &payload, b"");
    let rel = format!("records/principal/{slug}.md");
    let abs = store.abs(&rel);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(abs, bytes).unwrap();
}

fn ctx<'a>(store: &'a Store, types: &'a TypeSet, session_id: Option<&'a str>) -> WriteContext<'a> {
    WriteContext {
        store,
        types,
        by: "t".into(),
        committer: None,
        session_id,
        pause_hook: None,
    }
}

fn add_memory(
    store: &Store,
    types: &TypeSet,
    session_id: Option<&str>,
    principal: Option<String>,
) -> mem_core::error::MemResult<ops::Added> {
    let c = ctx(store, types, session_id);
    ops::add(
        &c,
        AddArgs {
            type_name: "memory".into(),
            payload: serde_json::json!({"title": "m"}),
            body: b"".to_vec(),
            principal,
        },
    )
}

/// 真相层快照：全店文件树（排除运行时锁文件、索引、.git——决策⑥ 锁文件豁免）。
fn truth_snapshot(root: &std::path::Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(dir: &std::path::Path, rel: &str, out: &mut BTreeMap<String, Vec<u8>>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let name = e.file_name().to_string_lossy().to_string();
            let p = e.path();
            let r = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            if p.is_dir() {
                if r == ".git" || r == "index" || r.starts_with("index/") {
                    continue;
                }
                walk(&p, &r, out);
            } else {
                if r.ends_with(".lock") {
                    continue; // 运行时锁文件豁免（决策⑥）
                }
                out.insert(r, std::fs::read(&p).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, "", &mut out);
    out
}

// ---------- 1. 台账继承 ----------

#[test]
fn ledger_principal_is_inherited_on_add() {
    let (_t, store, types) = fresh_store();
    let owner = seed_owner(&store);
    let o = session::start(
        &store,
        StartArgs {
            principal: owner.clone(),
            view: None,
            task_ref: None,
            handoff: HandoffSpec::None,
            idle_secs: 900,
        },
    )
    .unwrap();
    let out = add_memory(&store, &types, Some(&o.ledger.id), None).unwrap();
    assert_eq!(out.record.envelope.principal.as_deref(), Some(owner.as_str()));
}

// ---------- 2. 锁外拒绝：全店逐字节一致（含 .gitattributes 反例） ----------

#[test]
fn reject_leaves_store_tree_byte_identical_including_gitattrs_counterexample() {
    let (_t, store, types) = fresh_store();
    let root = store.root().to_path_buf();
    // 反例店：有 .git、缺 .gitattributes（git init 不产生该文件）。
    let status = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&root)
        .status()
        .expect("git 可用");
    assert!(status.success());
    assert!(!root.join(".gitattributes").exists());
    let before = truth_snapshot(&root);
    let err = add_memory(&store, &types, None, None).unwrap_err();
    assert!(
        matches!(err, MemError::BadArgument(ref m) if m.contains("归属缺失")),
        "{err}"
    );
    // 全店文件树前后逐字节一致；.gitattributes 未被创建。
    let after = truth_snapshot(&root);
    assert_eq!(before, after, "锁外拒绝：全店逐字节一致");
    assert!(!root.join(".gitattributes").exists(), "写前核验不得先于预检");
}

// ---------- 3. 显式胜出 + 中性告警（决策③） ----------

#[test]
fn explicit_wins_over_ledger_with_neutral_warning() {
    let (_t, store, types) = fresh_store();
    let owner = seed_owner(&store);
    write_principal(&store, "0198f7a9-0000-7000-8000-0000000000b2", "other");
    let o = session::start(
        &store,
        StartArgs {
            principal: owner.clone(),
            view: None,
            task_ref: None,
            handoff: HandoffSpec::None,
            idle_secs: 900,
        },
    )
    .unwrap();
    let out = add_memory(
        &store,
        &types,
        Some(&o.ledger.id),
        Some("0198f7a9-0000-7000-8000-0000000000b2".into()),
    )
    .unwrap();
    assert_eq!(
        out.record.envelope.principal.as_deref(),
        Some("0198f7a9-0000-7000-8000-0000000000b2"),
        "显式胜出"
    );
    assert!(
        out.warnings
            .iter()
            .any(|w| w.contains("显式 principal 与台账 principal 不同")),
        "中性事实告警：{:?}",
        out.warnings
    );
}

// ---------- 4. principal 类型豁免（决策④；自举回归） ----------

#[test]
fn principal_type_creation_is_exempt_from_ownership() {
    let (_t, store, types) = fresh_store();
    let c = ctx(&store, &types, None);
    let out = ops::add(
        &c,
        AddArgs {
            type_name: "principal".into(),
            payload: serde_json::json!({"slug": "codex", "label": "codex"}),
            body: b"".to_vec(),
            principal: None,
        },
    )
    .unwrap();
    assert!(out.created);
    assert_eq!(out.record.envelope.principal, None, "豁免：无归属合法");
    // 非 principal 类型仍走解析链（对照）。
    assert!(add_memory(&store, &types, None, None).is_err());
}

// ---------- 5. 悬空归属串行反例（r1 B3） ----------

#[test]
fn serial_dangling_ownership_rejected_then_restored_succeeds() {
    let (_t, store, types) = fresh_store();
    let owner = seed_owner(&store);
    let o = session::start(
        &store,
        StartArgs {
            principal: owner.clone(),
            view: None,
            task_ref: None,
            handoff: HandoffSpec::None,
            idle_secs: 900,
        },
    )
    .unwrap();
    let tok = ops::get(&store, &owner).unwrap().token;
    // 删除身份（活跃台账不阻止删除，决策⑦）。
    ops::delete_record(&ctx(&store, &types, None), &owner, &tok, false).unwrap();
    // 继承创建：响亮拒绝。
    let err = add_memory(&store, &types, Some(&o.ledger.id), None).unwrap_err();
    assert!(matches!(err, MemError::RefTargetMissing { .. }), "{err}");
    // 恢复身份 → 创建成功。
    ops::restore_record(&ctx(&store, &types, None), &owner, None).unwrap();
    let out = add_memory(&store, &types, Some(&o.ledger.id), None).unwrap();
    assert_eq!(out.record.envelope.principal.as_deref(), Some(owner.as_str()));
}

// ---------- 6/8. 悬空归属并发反例·删除先完成（r2 B4）+ 真相层净零 ----------

#[test]
fn concurrent_delete_wins_branch_add_rejected_with_truth_net_zero() {
    let (_t, store, _types) = fresh_store();
    let owner = seed_owner(&store);
    let o = session::start(
        &store,
        StartArgs {
            principal: owner.clone(),
            view: None,
            task_ref: None,
            handoff: HandoffSpec::None,
            idle_secs: 900,
        },
    )
    .unwrap();
    let tok = ops::get(&store, &owner).unwrap().token;
    let root = store.root().to_path_buf();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let release_rx = std::sync::Arc::new(std::sync::Mutex::new(release_rx));
    let types_dir = root.join("types");
    let session_id = o.ledger.id.clone();

    // add 线程：预检后暂停（pause_hook），等待主线程放行。
    let add_root = root.clone();
    let add_types = types_dir.clone();
    let add_session = session_id.clone();
    let rx = release_rx.clone();
    let handle = std::thread::spawn(move || {
        let s2 = Store::open(&add_root, OpenMode::Auto).unwrap();
        let t2 = TypeSet::load_dir(&add_types).unwrap();
        let (tx, rx) = (started_tx, rx);
        let hook: PauseHook = Box::new(move |_| {
            tx.send(()).unwrap();
            rx.lock().unwrap().recv().unwrap();
        });
        let c = WriteContext {
            store: &s2,
            types: &t2,
            by: "add".into(),
            committer: None,
            session_id: Some(&add_session),
            pause_hook: Some(hook),
        };
        ops::add(
            &c,
            AddArgs {
                type_name: "memory".into(),
                payload: serde_json::json!({"title": "m"}),
                body: b"".to_vec(),
                principal: None,
            },
        )
    });

    started_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("add 已到暂停点");
    // 暂停期间：删除身份完成（受管路径；活跃台账不阻止，决策⑦）。
    let del_root = root.clone();
    let del_owner = owner.clone();
    let del_tok = tok;
    let del_types = types_dir.clone();
    std::thread::spawn(move || {
        let s2 = Store::open(&del_root, OpenMode::Auto).unwrap();
        let t2 = TypeSet::load_dir(&del_types).unwrap();
        let c = WriteContext {
            store: &s2,
            types: &t2,
            by: "del".into(),
            committer: None,
            session_id: None,
            pause_hook: None,
        };
        ops::delete_record(&c, &del_owner, &del_tok, false).unwrap();
    })
    .join()
    .unwrap();
    // 净零断言的基线 = 删除完成后的真相层（add 的拒绝路径自身必须零变化）。
    // 反例前置：有 .git、缺 .gitattributes——删除线程的合法写可能已重建它，
    // 此刻移除；断言 add 的锁内拒绝路径不重建（决策⑥：写前核验位于复核之后）。
    let gitattrs = root.join(".gitattributes");
    {
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&root)
            .status()
            .expect("git 可用");
        assert!(status.success());
    }
    if gitattrs.exists() {
        std::fs::remove_file(&gitattrs).unwrap();
    }
    let before = truth_snapshot(&root);
    release_tx.send(()).unwrap();

    let err = handle
        .join()
        .unwrap()
        .expect_err("锁内复核拒绝：身份已被删除");
    assert!(
        matches!(&err, MemError::RefTargetMissing { target, .. } if target.contains("已被删除")),
        "{err}"
    );
    // 真相层净零：前后快照一致（锁文件豁免，决策⑥）；.gitattributes 未被创建。
    let after = truth_snapshot(&root);
    assert_eq!(before, after, "锁内复核拒绝：真相层净零（决策⑥）");
    assert!(!gitattrs.exists(), "写前核验位于锁内复核之后，拒绝路径不得创建");
}

// ---------- 7. 悬空归属并发反例·创建先完成（r3 B5）+ 删除侧回归 ----------

#[test]
fn concurrent_create_wins_branch_delete_blocked_then_force_warns_and_verify_reports() {
    let (_t, store, types) = fresh_store();
    let owner = seed_owner(&store);
    let out = add_memory(&store, &types, None, Some(owner.clone())).unwrap();
    // 默认阻止：错误含归属依赖。
    let tok = ops::get(&store, &owner).unwrap().token;
    let err = ops::delete_record(&ctx(&store, &types, None), &owner, &tok, false).unwrap_err();
    assert!(err.to_string().contains("归属"), "{err}");
    // force 越过：显式告警。
    let deleted =
        ops::delete_record(&ctx(&store, &types, None), &owner, &tok, true).unwrap();
    assert!(
        deleted
            .warnings
            .iter()
            .any(|w| w.contains("归属悬空")),
        "{:?}",
        deleted.warnings
    );
    // verify 归属断链审计：报告三要素，修复入口指向恢复（隔离区 tombstone）。
    let report = ops::principal_binding_audit(&store).unwrap();
    assert_eq!(report.len(), 1, "{report:?}");
    assert_eq!(report[0]["from"], out.record.envelope.id.as_str());
    assert_eq!(report[0]["target"], owner.as_str());
    assert!(
        report[0]["repair"]
            .as_str()
            .unwrap()
            .contains("恢复身份记录"),
        "{report:?}"
    );
    // 恢复身份 → 告警消失。
    ops::restore_record(&ctx(&store, &types, None), &owner, None).unwrap();
    assert!(ops::principal_binding_audit(&store).unwrap().is_empty());
}

// ---------- 8. verify 三组回归的基线面（空归属/正常归属不误报） ----------

#[test]
fn verify_audit_baseline_no_false_positive_for_empty_and_valid_ownership() {
    let (_t, store, types) = fresh_store();
    let owner = seed_owner(&store);
    // 正常归属：归属现存身份 → 审计干净。
    add_memory(&store, &types, None, Some(owner.clone())).unwrap();
    // 外部空归属：手工放入 principal=null 的 memory（R2.4 合法路径）→ 可读、不误报。
    let env = mem_core::envelope::Envelope {
        id: "0198f7a9-0000-7000-8000-0000000000c3".into(),
        r#type: "memory".into(),
        created: "2026-11-19T00:00:00Z".into(),
        modified: "2026-11-19T00:00:00Z".into(),
        version: 1,
        principal: None,
        refs: vec![],
        aliases: vec!["external-null".into()],
        evidence_cutoff_seq: None,
        commit: None,
        extra: Default::default(),
    };
    let bytes = mem_core::record::serialize_new(&env, &serde_json::json!({"title": "外部"}), b"");
    let rel = "records/memory/external-null.md";
    let abs = store.abs(rel);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(abs, bytes).unwrap();
    assert!(ops::principal_binding_audit(&store).unwrap().is_empty());
    let got = ops::get(&store, "0198f7a9-0000-7000-8000-0000000000c3").unwrap();
    assert_eq!(got.record.envelope.principal, None, "外部空归属合法可读");
}

// ---------- 9. 死锁守护：合法创建与并发删除多轮交错 ----------

#[test]
fn create_and_delete_interleave_without_deadlock() {
    let (_t, store, _types) = fresh_store();
    let owner = seed_owner(&store);
    let root = store.root().to_path_buf();
    let types_dir = root.join("types");
    for round in 0..5 {
        let r2 = root.clone();
        let d2 = types_dir.clone();
        let o2 = owner.clone();
        let add_h = std::thread::spawn(move || {
            let s = Store::open(&r2, OpenMode::Auto).unwrap();
            let t = TypeSet::load_dir(&d2).unwrap();
            // 两分支都合法：创建成功，或删除先完成导致锁内复核拒绝——只验不卡死。
            let _ = add_memory(&s, &t, None, Some(o2));
        });
        let r3 = root.clone();
        let d3 = types_dir.clone();
        let o3 = owner.clone();
        let del_h = std::thread::spawn(move || {
            let s = Store::open(&r3, OpenMode::Auto).unwrap();
            let t = TypeSet::load_dir(&d3).unwrap();
            // 创建先完成 → 默认阻止；删除先完成 → 成功。两分支都合法，只验不卡死。
            let tok = ops::get(&s, &o3).unwrap().token;
            let _ = ops::delete_record(&ctx(&s, &t, None), &o3, &tok, false);
        });
        // 两分支都合法：创建先完成（Ok）或删除先完成（锁内复核拒绝）——只验不卡死。
        // 死锁守护：轮询完成状态 + 30 秒 deadline（join 无超时 API）。
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !add_h.is_finished() || !del_h.is_finished() {
            assert!(
                std::time::Instant::now() < deadline,
                "第 {round} 轮交错死锁（30 秒未完成）"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        add_h.join().expect("add 线程无 panic");
        del_h.join().expect("delete 线程无 panic");
        let _ = round;
    }
}

// ---------- 同路径自锁防御：重复创建同一 principal 且显式指定自身为归属 ----------

#[test]
fn duplicate_principal_self_ownership_returns_loser_protocol() {
    let (_t, store, types) = fresh_store();
    let owner = seed_owner(&store);
    // 重复创建同一 slug 的 principal，显式指定它自己为归属——
    // 归属锁与新记录锁同路径，修复前会因锁不可重入等待 30 秒超时；
    // 修复后立即按唯一键输者协议返回 created: false。
    let c = ctx(&store, &types, None);
    let out = ops::add(
        &c,
        AddArgs {
            type_name: "principal".into(),
            payload: serde_json::json!({"slug": "test-owner", "label": "test-owner"}),
            body: b"".to_vec(),
            principal: Some(owner.clone()),
        },
    )
    .unwrap();
    assert!(!out.created, "输者协议：created=false");
    assert_eq!(
        out.record.envelope.id, owner,
        "输者返回既有记录"
    );
    // 正常（非同路径）对照：创建第二个身份并归属第一个 → 走豁免+显式校验路径，
    // 归属锁（owner 的记录）与新记录锁（other）不同路径，正常获取并创建成功。
    let out2 = ops::add(
        &c,
        AddArgs {
            type_name: "principal".into(),
            payload: serde_json::json!({"slug": "other", "label": "other"}),
            body: b"".to_vec(),
            principal: Some(owner.clone()),
        },
    )
    .unwrap();
    assert!(out2.created, "不同路径：正常创建");
    assert_eq!(out2.record.envelope.principal.as_deref(), Some(owner.as_str()));
}
