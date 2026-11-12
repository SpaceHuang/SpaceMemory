//! §8-6a mem-index 测试：物化 / 查询 / 边表 / FTS / 对账诊断（D5 / 红线8）。

use mem_core::ops::{self, WriteContext};
use mem_core::store::{OpenMode, Store};
use mem_schema::TypeSet;

const PROJECT: &str = "0198f7a9-0000-7cd2-9e3f-000000000001";

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
    let store = Store::open(&root, OpenMode::Auto).unwrap();
    let types = TypeSet::load_dir(&types_dir).unwrap();
    // seed project。
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
        b"",
    );
    let abs = store.abs("records/project/proj.md");
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(abs, bytes).unwrap();
    (tmp, store, types)
}

#[test]
fn rebuild_and_query_by_type() {
    let (_t, store, types) = fresh_store();
    let c = WriteContext { store: &store, types: &types, by: "t".into(), committer: None, session_id: None };
    for slug in ["a", "b"] {
        ops::add(&c, ops::AddArgs {
            type_name: "task".into(),
            payload: serde_json::json!({"project": PROJECT, "slug": slug, "title": slug, "status": "todo"}),
            body: b"".to_vec(),
            principal: None,
        })
        .unwrap();
    }
    let mut idx = mem_index::Index::open_store(&store).unwrap();
    let (n, refs) = idx.rebuild(&store).unwrap();
    assert_eq!(n, 3, "project + 2 task");
    assert!(refs >= 2, "task→project 镜像引用边");
    let tasks = idx.query_by_type("task", 10, 0).unwrap();
    assert_eq!(tasks.len(), 2);
    // 分页。
    let page1 = idx.query_by_type("task", 1, 0).unwrap();
    assert_eq!(page1.len(), 1);
    let page2 = idx.query_by_type("task", 1, 1).unwrap();
    assert_eq!(page2.len(), 1);
    assert_ne!(page1[0].id, page2[0].id);
}

#[test]
fn edge_neighbors_support_trace() {
    let (_t, store, types) = fresh_store();
    let c = WriteContext { store: &store, types: &types, by: "t".into(), committer: None, session_id: None };
    let t = ops::add(&c, ops::AddArgs {
        type_name: "memory".into(),
        payload: serde_json::json!({"title": "证据"}),
        body: b"".to_vec(),
        principal: None,
    })
    .unwrap();
    let mut idx = mem_index::Index::open_store(&store).unwrap();
    idx.rebuild(&store).unwrap();
    // 直读边表查 up/down。
    let up = idx.neighbors(&t.record.envelope.id, "up").unwrap();
    assert!(up.is_empty());
    // 用一次 link 后再物化。
    let _ = ops::get(&store, &t.record.envelope.id).unwrap();
    let proj = ops::get(&store, PROJECT).unwrap();
    let _ = ops::link(&c, &t.record.envelope.id, &proj.record.envelope.id, "belongs_to",
        &t.token, &proj.token).unwrap();
    idx.rebuild(&store).unwrap();
    let up = idx.neighbors(&t.record.envelope.id, "up").unwrap();
    assert_eq!(up.len(), 1);
    let down = idx.neighbors(&proj.record.envelope.id, "down").unwrap();
    assert_eq!(down.len(), 1);
}

#[test]
fn diagnose_reports_drift_never_rejects() {
    let (_t, store, types) = fresh_store();
    let c = WriteContext { store: &store, types: &types, by: "t".into(), committer: None, session_id: None };
    ops::add(&c, ops::AddArgs {
        type_name: "memory".into(),
        payload: serde_json::json!({"title": "m"}),
        body: b"".to_vec(),
        principal: None,
    })
    .unwrap();
    let mut idx = mem_index::Index::open_store(&store).unwrap();
    idx.rebuild(&store).unwrap();
    // 真相层再前进（索引落后）→ 诊断报告差异，不拒收（红线8）。
    ops::add(&c, ops::AddArgs {
        type_name: "memory".into(),
        payload: serde_json::json!({"title": "m2"}),
        body: b"".to_vec(),
        principal: None,
    })
    .unwrap();
    let scan = ops::scan_all(&store).unwrap();
    let diff = idx.diagnose_against(&scan.records).unwrap();
    assert!(!diff.is_empty(), "{diff:?}");
    assert!(diff.iter().any(|d| d.contains("索引缺记录")));
}

// （R10 实施评审 L2）Index::search（单串 LIKE）已删除——粗筛被 FindPrefilter::candidates
// 取代（keyword-search-design.md §2.1：既有 Index::search 被 candidates 取代）。
// 其中段/后段子串语义回归迁移至 tests/find_prefilter.rs 的
// candidates_mid_substring_semantics（payload 中段 + 正文后段仍经折叠列 LIKE 命中）。

#[test]
fn readonly_open_never_creates_and_epoch_advances() {
    // r4 B2：读路径 open_readonly——文件缺失 → Err，且**不创建文件**；
    // rebuild 递增 epoch（检查点同代核对依据）。
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("index/index.sqlite");
    let _err = mem_index::Index::open_readonly(&path).err().expect("缺失必须 Err");
    assert!(!path.exists(), "open_readonly 不得创建索引文件");

    let store = fake_store(tmp.path());
    let mut idx = mem_index::Index::open(&path).unwrap();
    idx.rebuild(&store).unwrap();
    drop(idx);
    let before: std::collections::BTreeMap<_, _> = std::fs::read_dir(path.parent().unwrap()).unwrap()
        .map(|e| { let p = e.unwrap().path(); (p.file_name().unwrap().to_os_string(), std::fs::read(p).unwrap()) })
        .collect();
    let ro = mem_index::Index::open_readonly(&path).expect("建库后只读打开成功");
    let e1 = ro.epoch().unwrap();
    assert!(!path.with_extension("sqlite-wal").exists());
    assert!(!path.with_extension("sqlite-shm").exists());
    let (tx, rx) = std::sync::mpsc::channel();
    let writer_path = path.clone();
    let writer = std::thread::spawn(move || {
        let _idx = mem_index::Index::open(&writer_path).unwrap();
        tx.send(()).unwrap();
    });
    assert!(rx.recv_timeout(std::time::Duration::from_millis(80)).is_err(), "只读快照存活时维护写入须等待");
    drop(ro);
    rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
    writer.join().unwrap();
    let after: std::collections::BTreeMap<_, _> = std::fs::read_dir(path.parent().unwrap()).unwrap()
        .map(|e| { let p = e.unwrap().path(); (p.file_name().unwrap().to_os_string(), std::fs::read(p).unwrap()) })
        .collect();
    assert_eq!(before, after, "只读打开期间索引目录文件与内容不变");
    let mut idx = mem_index::Index::open(&path).unwrap();
    idx.rebuild(&store).unwrap();
    drop(idx);
    let ro2 = mem_index::Index::open_readonly(&path).unwrap();
    let e2 = ro2.epoch().unwrap();
    assert_eq!(e2, e1 + 1, "epoch 随 rebuild 严格递增（{e1} → {e2}）");
}

/// 最小空店（rebuild 的 scan 目标）。
fn fake_store(tmp: &std::path::Path) -> mem_core::store::Store { // &TempDir 可隐式 deref
    let root = tmp.join("store");
    let _ = std::fs::create_dir_all(root.join("records"));
    let _ = std::fs::write(
        root.join("mem.json"),
        r#"{"format_version":1,"created":"2026-11-05T00:00:00Z"}"#,
    );
    mem_core::store::Store::open(&root, mem_core::store::OpenMode::Auto).unwrap()
}





