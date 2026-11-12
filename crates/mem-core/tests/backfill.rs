//! §8-4c 事件日志 / 状态覆盖回填 / 漂移扫描测试（D12 / 红线24 / 红线26）。
//!
//! 覆盖：v1 创建后崩溃、中间空洞、保版本手改、撕裂尾行、外部删除、外部重命名、
//! 删除后重建、只读存储外部删除；absent 稳定态（同一次删除只回填一次）；
//! 回填并发（比较后暂停 → update 完成 → 恢复回填不追加过时事件；双并发回填不重复）。

use std::time::Duration;

use mem_core::envelope::{Envelope, Ref};
use mem_core::error::MemError;
use mem_core::event::{self, Location, Op, StateTriple};
use mem_core::store::{OpenMode, Store};
use mem_core::token;

const UUID_P: &str = "0198f7b2-0000-7cd2-9e3f-1a2b3c4d5e6f";

fn fresh_store() -> (tempfile::TempDir, Store) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("store");
    Store::create(&root).unwrap();
    let store = Store::open(&root, OpenMode::Auto).unwrap();
    (tmp, store)
}

fn env(id: &str, type_name: &str, version: u64) -> Envelope {
    Envelope {
        id: id.into(),
        r#type: type_name.into(),
        created: "2026-11-04T09:12:00Z".into(),
        modified: "2026-11-04T11:40:00Z".into(),
        version,
        principal: Some(UUID_P.into()),
        refs: Vec::<Ref>::new(),
        aliases: Vec::new(),
        evidence_cutoff_seq: None,
        commit: None,
        extra: Default::default(),
    }
}

/// 手写记录文件落盘（模拟人/崩溃产物；绕过机制写路径）。
fn hand_write(store: &Store, rel: &str, id: &str, type_name: &str, version: u64, body: &str) -> String {
    let e = env(id, type_name, version);
    let payload = serde_json::json!({"slug": "x"});
    let bytes = mem_core::record::serialize_new(&e, &payload, body.as_bytes());
    let digest = token::sha256_hex(&bytes);
    let abs = store.abs(rel);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, &bytes).unwrap();
    digest
}

fn digest_of(store: &Store, rel: &str) -> Option<String> {
    std::fs::read(store.abs(rel)).ok().map(|b| token::sha256_hex(&b))
}

fn event_count(store: &Store) -> usize {
    event::EventLog::new(store).read_log().unwrap().events.len()
}

fn last_event_for(store: &Store, id: &str) -> Option<mem_core::event::Event> {
    event::EventLog::new(store).last_event_for(id).unwrap()
}

// ---------- 事件日志基础 ----------

#[test]
fn append_assigns_sequential_seq_and_roundtrips() {
    let (_t, store) = fresh_store();
    let log = event::EventLog::new(&store);
    let e1 = log
        .append_simple("0198a", "records/task/p/a.md", Op::Create, None, Some(1), None, Some("d1"), "tester")
        .unwrap();
    let e2 = log
        .append_simple("0198a", "records/task/p/a.md", Op::Update, Some(1), Some(2), Some("d1"), Some("d2"), "tester")
        .unwrap();
    assert_eq!(e1.seq, 1);
    assert_eq!(e2.seq, 2, "序号 = 行序，全库串行分配");
    let read = log.read_log().unwrap();
    assert_eq!(read.events.len(), 2);
    assert_eq!(read.events[1].op, Op::Update);
    assert_eq!(read.events[1].new_digest.as_deref(), Some("d2"));
    assert_eq!(log.max_seq().unwrap(), 2);
}

#[test]
fn torn_tail_is_truncated_and_reported() {
    let (_t, store) = fresh_store();
    let log = event::EventLog::new(&store);
    log.append_simple("0198a", "records/x.md", Op::Create, None, Some(1), None, Some("d"), "t").unwrap();
    // 模拟崩溃撕裂：追加半行 JSON（无换行）。
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(store.abs("events.jsonl")).unwrap();
        f.write_all(b"{\"seq\":2,\"ts\":\"20").unwrap();
    }
    let read = log.read_log().unwrap();
    assert_eq!(read.events.len(), 1, "坏行忽略");
    assert!(read.torn_tail, "撕裂尾行必须显式报告");
    // 追加例程先补行界再写：新事件 seq=2，坏行被截断。
    let e = log.append_simple("0198b", "records/y.md", Op::Create, None, Some(1), None, Some("d"), "t").unwrap();
    assert_eq!(e.seq, 2);
    let read2 = log.read_log().unwrap();
    assert_eq!(read2.events.len(), 2);
    assert!(!read2.torn_tail, "追加后行界修复");
}

// ---------- 状态三元组 ----------

#[test]
fn state_triple_locations() {
    let (_t, store) = fresh_store();
    let rel = "records/task/p/a.md";
    // absent（一等状态）
    assert_eq!(
        event::current_state(&store, rel).unwrap(),
        StateTriple { location: Location::Absent, version: None, digest: None }
    );
    let d = hand_write(&store, rel, "0198a", "task", 3, "body");
    assert_eq!(
        event::current_state(&store, rel).unwrap(),
        StateTriple { location: Location::Records, version: Some(3), digest: Some(d) }
    );
    // 移入 attic（受管 tombstone）
    std::fs::create_dir_all(store.abs("attic/task/p")).unwrap();
    std::fs::rename(store.abs(rel), store.abs("attic/task/p/a.md")).unwrap();
    let d2 = digest_of(&store, "attic/task/p/a.md").unwrap();
    assert_eq!(
        event::current_state(&store, rel).unwrap(),
        StateTriple { location: Location::Attic, version: Some(3), digest: Some(d2) }
    );
}

// ---------- 回填：注入面 ----------

#[test]
fn backfill_v1_create_after_crash() {
    let (_t, store) = fresh_store();
    let rel = "records/task/p/a.md";
    let d = hand_write(&store, rel, "0198a", "task", 1, "第一版");
    assert_eq!(event_count(&store), 0);
    let report = event::reconcile_backfill(&store, "tester", None).unwrap();
    assert_eq!(report.appended.len(), 1);
    let ev = &report.appended[0];
    assert_eq!(ev.op, Op::Backfill);
    assert_eq!(ev.kind, event::Kind::Backfill);
    assert_eq!(ev.id, "0198a");
    assert_eq!(ev.new_v, Some(1));
    assert_eq!(ev.new_digest.as_deref(), Some(d.as_str()));
    assert_eq!(event_count(&store), 1);
    // 回填后漂移消失（转正）。
    let drift = mem_core::drift::scan_current_drift(&store, 100).unwrap();
    assert!(drift.items.is_empty(), "{:?}", drift.items);
}

#[test]
fn backfill_middle_version_hole_reported_not_faked() {
    let (_t, store) = fresh_store();
    let log = event::EventLog::new(&store);
    // v1 create，然后伪造 v3 update（v2 缺失——版本链断裂）。
    log.append_simple("0198a", "records/x.md", Op::Create, None, Some(1), None, Some("d1"), "t").unwrap();
    log.append_simple("0198a", "records/x.md", Op::Update, Some(3), Some(3), Some("d3"), Some("d3"), "t").unwrap();
    // 回填无动作（last==cur 稳定——文件与 v3 一致即可；此处文件不存在 → absent≠records → 会 backfill。
    // 为聚焦链审计，手写文件与 v3 状态一致）。
    hand_write(&store, "records/x.md", "0198a", "task", 3, "v3 body");
    let d3 = digest_of(&store, "records/x.md").unwrap();
    // 用手写 digest 修正：直接改最后一行代价高——改为重放：先删文件回 absent 也行。
    // 简化：接受一次 backfill（absent→records），链空洞仍可审计。
    let report = event::reconcile_backfill(&store, "t", None).unwrap();
    let _ = report;
    let holes = event::chain_audit(&log.read_log().unwrap().events);
    assert_eq!(holes.len(), 1, "{holes:?}");
    assert_eq!(holes[0].id, "0198a");
    assert!(holes[0].reason.contains("版本链"), "{holes:?}");
    let _ = d3;
}

#[test]
fn backfill_version_preserved_hand_edit() {
    let (_t, store) = fresh_store();
    let rel = "records/task/p/a.md";
    let id = "0198a";
    hand_write(&store, rel, id, "task", 1, "原版");
    event::reconcile_backfill(&store, "t", None).unwrap();
    // 保版本手改：version 仍 1，正文变了 → 摘要变了。
    hand_write(&store, rel, id, "task", 1, "保版本手改的内容");
    let report = event::reconcile_backfill(&store, "t", None).unwrap();
    assert_eq!(report.appended.len(), 1, "保版本手改必被状态比较捕获（B2）");
    assert_eq!(report.appended[0].op, Op::Backfill);
    assert_eq!(report.appended[0].old_v, Some(1));
    assert_eq!(report.appended[0].new_v, Some(1), "版本未变，仅摘要变");
}

#[test]
fn backfill_external_delete_and_absent_stable_state() {
    let (_t, store) = fresh_store();
    let rel = "records/task/p/a.md";
    hand_write(&store, rel, "0198a", "task", 2, "x");
    event::reconcile_backfill(&store, "t", None).unwrap();
    // 外部删除（人手删文件，无事件）。
    std::fs::remove_file(store.abs(rel)).unwrap();
    let report = event::reconcile_backfill(&store, "t", None).unwrap();
    assert_eq!(report.appended.len(), 1);
    assert_eq!(report.appended[0].op, Op::ExternalDelete, "外部删除产生 external_delete（B1）");
    assert_eq!(report.appended[0].new_digest, None);
    assert_eq!(report.appended[0].new_v, None);
    assert_eq!(report.appended[0].path, rel, "path = 最后已知路径（ID↔路径映射）");
    // absent == absent → 同一次删除只回填一次（稳定态，本轮 B1）。
    let again = event::reconcile_backfill(&store, "t", None).unwrap();
    assert!(again.appended.is_empty(), "不得重复回填：{:?}", again.appended);
    // 漂移也不再见（absent 已登记）。
    let drift = mem_core::drift::scan_current_drift(&store, 100).unwrap();
    assert!(drift.items.is_empty(), "{:?}", drift.items);
}

#[test]
fn two_reconciles_two_diffs_second_clean() {
    let (_t, store) = fresh_store();
    let rel = "records/task/p/a.md";
    hand_write(&store, rel, "0198a", "task", 1, "x");
    event::reconcile_backfill(&store, "t", None).unwrap();
    std::fs::remove_file(store.abs(rel)).unwrap();
    // 第一轮：reconcile + diff
    event::reconcile_backfill(&store, "t", None).unwrap();
    let d1 = mem_core::drift::diff_since(&store, 0, 200, 100).unwrap();
    assert!(d1.current_drift.items.is_empty());
    // 第二轮：reconcile + diff
    let r2 = event::reconcile_backfill(&store, "t", None).unwrap();
    assert!(r2.appended.is_empty(), "第二次 reconcile 不新增事件");
    let d2 = mem_core::drift::diff_since(&store, 0, 200, 100).unwrap();
    assert!(d2.current_drift.items.is_empty(), "不再出现于 current_drift——absent 稳定态");
}

#[test]
fn backfill_external_rename_moves_path_mapping() {
    let (_t, store) = fresh_store();
    let rel = "records/task/p/a.md";
    let new_rel = "records/task/p/b-renamed.md";
    hand_write(&store, rel, "0198a", "task", 1, "x");
    event::reconcile_backfill(&store, "t", None).unwrap();
    // 外部重命名（id 不变，路径变）。
    std::fs::create_dir_all(store.abs("records/task/p")).unwrap();
    std::fs::rename(store.abs(rel), store.abs(new_rel)).unwrap();
    let report = event::reconcile_backfill(&store, "t", None).unwrap();
    assert_eq!(report.appended.len(), 1, "重命名必须被检出（路径参与状态比较）");
    assert_eq!(report.appended[0].path, new_rel, "事件 path 更新为新路径");
    let again = event::reconcile_backfill(&store, "t", None).unwrap();
    assert!(again.appended.is_empty(), "稳定态");
}

#[test]
fn delete_then_recreate_yields_backfill_not_external_delete() {
    let (_t, store) = fresh_store();
    let rel = "records/task/p/a.md";
    let attic_rel = "attic/task/p/a.md";
    hand_write(&store, rel, "0198a", "task", 1, "x");
    event::reconcile_backfill(&store, "t", None).unwrap();
    // 受管删除：移动到 attic + delete 事件（事件后状态 = attic；摘要真实）。
    let d1 = digest_of(&store, rel).unwrap();
    std::fs::create_dir_all(store.abs("attic/task/p")).unwrap();
    std::fs::rename(store.abs(rel), store.abs(attic_rel)).unwrap();
    event::EventLog::new(&store)
        .append_simple("0198a", attic_rel, Op::Delete, Some(1), Some(1), Some(&d1), Some(&d1), "t")
        .unwrap();
    // 重新创建（人手把文件拷回 records/ 同路径）。
    hand_write(&store, rel, "0198a", "task", 1, "recreated");
    let report = event::reconcile_backfill(&store, "t", None).unwrap();
    assert_eq!(report.appended.len(), 1);
    assert_eq!(report.appended[0].op, Op::Backfill, "重建是 backfill，不是 external_delete");
    assert_eq!(report.appended[0].path, rel);
}

#[test]
fn readonly_store_backfill_rejected_drift_still_visible() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("store");
    Store::create(&root).unwrap();
    let ro = Store::open(&root, OpenMode::ForceReadOnly).unwrap();
    // 无事件的手写漂移在只读店上可见。
    let rel = "records/task/p/a.md";
    hand_write(&ro, rel, "0198a", "task", 1, "x");
    let drift = mem_core::drift::scan_current_drift(&ro, 100).unwrap();
    assert_eq!(drift.items.len(), 1);
    assert!(drift.items[0].readonly_note.is_some(), "B 项标注只读无法登记");
    // 回填是写例程 → 结构化拒绝。
    assert!(matches!(
        event::reconcile_backfill(&ro, "t", None),
        Err(MemError::ReadOnly)
    ));
}

// ---------- 回填并发纪律 ----------

#[test]
fn paused_backfill_does_not_append_stale_event_after_update() {
    // 验收：比较后可控暂停 → 普通 update 完成 → 恢复回填不得追加过时事件。
    let (_t, store) = fresh_store();
    let rel = "records/task/p/a.md";
    hand_write(&store, rel, "0198a", "task", 1, "v1");
    event::reconcile_backfill(&store, "t", None).unwrap();
    // 手改成 v2（漂移），然后启动带暂停点的回填。
    hand_write(&store, rel, "0198a", "task", 2, "v2-by-hand");
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let (tx2, rx2) = std::sync::mpsc::channel::<()>();
    let store2 = {
        let root = store.root().to_path_buf();
        Store::open(&root, OpenMode::Auto).unwrap()
    };
    let h = std::thread::spawn(move || {
        let rx = std::sync::Mutex::new(rx);
        let hook = move |_id: &str| {
            tx2.send(()).unwrap(); // 已判定、即将追加
            rx.lock().unwrap().recv().unwrap(); // 等主线程完成 update
        };
        event::reconcile_backfill(&store2, "backfiller", Some(Box::new(hook))).unwrap()
    });
    rx2.recv().unwrap();
    // 回填判定后暂停期间：普通 update 完成（文件 v3 + update 事件，摘要真实）。
    let d2 = digest_of(&store, rel).unwrap();
    hand_write(&store, rel, "0198a", "task", 3, "v3-by-update");
    let d3 = digest_of(&store, rel).unwrap();
    event::EventLog::new(&store)
        .append_simple("0198a", rel, Op::Update, Some(2), Some(3), Some(&d2), Some(&d3), "updater")
        .unwrap();
    tx.send(()).unwrap();
    let report = h.join().unwrap();
    assert!(
        report.appended.is_empty(),
        "恢复后的回填在序列锁内复核：last(update v3) == cur(v3) → 无动作；不得追加过时事件 {:?}",
        report.appended
    );
    let last = last_event_for(&store, "0198a").unwrap();
    assert_eq!(last.op, Op::Update, "最后事件是 update，而非过时 backfill");
    assert_eq!(last.new_v, Some(3));
}

#[test]
fn double_concurrent_backfill_no_duplicates() {
    let (_t, store) = fresh_store();
    let rel = "records/task/p/a.md";
    hand_write(&store, rel, "0198a", "task", 1, "x");
    let mut handles = Vec::new();
    for i in 0..8 {
        let root = store.root().to_path_buf();
        handles.push(std::thread::spawn(move || {
            let s = Store::open(&root, OpenMode::Auto).unwrap();
            std::thread::sleep(Duration::from_millis(i % 3 * 5));
            event::reconcile_backfill(&s, "w", None).unwrap()
        }));
    }
    let mut total = 0;
    for h in handles {
        total += h.join().unwrap().appended.len();
    }
    assert_eq!(event_count(&store), 1, "8 路并发回填，事件日志恰好 1 条 backfill");
    assert_eq!(total, 1, "并发回填被记录锁串行化，后者锁内重算 last==cur → 无动作");
}

#[test]
fn backfill_restore_interleave_no_misordered_events() {
    // 三方交错：回填A判定暂停 → restore 完成（attic→records + 事件）→ 回填B → 恢复A。
    let (_t, store) = fresh_store();
    let rel = "records/task/p/a.md";
    let attic_rel = "attic/task/p/a.md";
    hand_write(&store, rel, "0198a", "task", 1, "x");
    event::reconcile_backfill(&store, "t", None).unwrap();
    // 受管删除进 attic（事件摘要真实）。
    let d1 = digest_of(&store, rel).unwrap();
    std::fs::create_dir_all(store.abs("attic/task/p")).unwrap();
    std::fs::rename(store.abs(rel), store.abs(attic_rel)).unwrap();
    event::EventLog::new(&store)
        .append_simple("0198a", attic_rel, Op::Delete, Some(1), Some(1), Some(&d1), Some(&d1), "t")
        .unwrap();
    // 回填A 启动（会看到漂移：attic 有文件但最后事件是 delete？——delete 事件后状态=attic，
    // cur=attic → last==cur 稳定！要制造漂移：把 attic 文件改一下（保版本手改 tombstone）。
    hand_write(&store, attic_rel, "0198a", "task", 1, "tombstone-hand-edit");
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let (tx2, rx2) = std::sync::mpsc::channel::<()>();
    let store_a = {
        let root = store.root().to_path_buf();
        Store::open(&root, OpenMode::Auto).unwrap()
    };
    let ha = std::thread::spawn(move || {
        let rx = std::sync::Mutex::new(rx);
        let hook = move |_id: &str| {
            tx2.send(()).unwrap();
            rx.lock().unwrap().recv().unwrap();
        };
        event::reconcile_backfill(&store_a, "A", Some(Box::new(hook))).unwrap()
    });
    rx2.recv().unwrap(); // A 已判定（认为 cur=attic 手改版）
    // 暂停期间：restore 完成——attic 文件移回 records + restore 事件（摘要真实）。
    std::fs::rename(store.abs(attic_rel), store.abs(rel)).unwrap();
    event::EventLog::new(&store)
        .append_simple("0198a", rel, Op::Restore, Some(1), Some(1), Some(&d1), Some(&d1), "restorer")
        .unwrap();
    tx.send(()).unwrap();
    let ra = ha.join().unwrap();
    // A 恢复后在序列锁内复核：restore 把"手改过的 tombstone"移回了 records——
    // 该未登记手改随 restore 进入 records 侧，A 必须补登一条 records 侧 backfill
    //（不得追加过时的 attic 事件）。
    assert_eq!(ra.appended.len(), 1, "{:?}", ra.appended);
    assert_eq!(ra.appended[0].op, Op::Backfill);
    assert_eq!(ra.appended[0].path, rel, "补登的是 records 侧新状态，不是过时的 attic 状态");
    // 再一轮 reconcile：稳定态，无新事件；漂移清空。
    let again = event::reconcile_backfill(&store, "t", None).unwrap();
    assert!(again.appended.is_empty());
    let cur = event::current_state(&store, rel).unwrap();
    assert_eq!(cur.location, Location::Records);
    let drift = mem_core::drift::scan_current_drift(&store, 100).unwrap();
    assert!(drift.items.is_empty(), "{:?}", drift.items);
}

// ---------- diff 契约（A/B 分开呈现，红线26） ----------

#[test]
fn torn_tail_with_newline_repaired_on_append_path() {
    // B1 盲区：坏行自带换行（手改/行界恰好写满）——修复必须移除坏行本身，
    // 而非截断到最后换行（那样坏行保留、追加后成中部坏行）。
    let (_t, store) = fresh_store();
    let log = event::EventLog::new(&store);
    log.append_simple("0198a", "records/x.md", Op::Create, None, Some(1), None, Some("d"), "t").unwrap();
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(store.abs("events.jsonl")).unwrap();
        f.write_all(b"{\"seq\":999,\"ts\":\"20
").unwrap(); // 坏行（带换行）
    }
    let read = log.read_log().unwrap();
    assert!(read.torn_tail);
    // 追加后：坏行被移除，日志完全可用。
    let e = log.append_simple("0198b", "records/y.md", Op::Create, None, Some(1), None, Some("d"), "t").unwrap();
    assert_eq!(e.seq, 2);
    let read2 = log.read_log().unwrap();
    assert!(!read2.torn_tail);
    assert_eq!(read2.events.len(), 2);
    // 后续 diff（读路径）不再硬错误。
    let d = mem_core::drift::diff_since(&store, 0, 200, 100).unwrap();
    assert_eq!(d.changes_since.len(), 2);
}

#[test]
fn torn_tail_repaired_on_reconcile_path() {
    // B1 盲区：reconcile 路径（with_sequence_recheck）此前只补 \n 不截断——
    // 无论坏行是否带换行，追加即毒化为中部坏行。
    let bad_lines: [&str; 2] = ["{\"seq\":999,\"ts\":\"20\n", "{ broken json\n"];
    for bad_line in bad_lines {
        let (_t, store) = fresh_store();
        let rel = "records/task/p/a.md";
        hand_write(&store, rel, "0198a", "task", 1, "x");
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new().append(true).open(store.abs("events.jsonl")).unwrap();
            f.write_all(bad_line.as_bytes()).unwrap();
        }
        // reconcile（会追加 backfill 事件）——坏行必须被截断而非保留。
        let report = event::reconcile_backfill(&store, "t", None).unwrap();
        assert_eq!(report.appended.len(), 1, "bad_line={bad_line:?}");
        // 追加后全店可读可写（diff 不硬错误；再次 reconcile 稳定）。
        let read = event::EventLog::new(&store).read_log().unwrap();
        assert!(!read.torn_tail, "bad_line={bad_line:?}");
        let d = mem_core::drift::diff_since(&store, 0, 200, 100).unwrap();
        assert!(d.current_drift.items.is_empty(), "bad_line={bad_line:?}");
        let again = event::reconcile_backfill(&store, "t", None).unwrap();
        assert!(again.appended.is_empty());
    }
}

#[test]
fn mid_file_bad_line_stays_loud_not_silently_repaired() {
    // 中部坏行 = 结构损坏（响亮错误），不被自动修复吞掉——修复只针对**尾行**。
    let (_t, store) = fresh_store();
    let log = event::EventLog::new(&store);
    log.append_simple("0198a", "records/x.md", Op::Create, None, Some(1), None, Some("d"), "t").unwrap();
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(store.abs("events.jsonl")).unwrap();
        f.write_all(b"{ broken mid-file line\n").unwrap();
        f.write_all(b"{\"by\":\"t\",\"id\":\"0198b\",\"kind\":\"op\",\"new_digest\":\"d\",\"new_v\":1,\"old_digest\":null,\"old_v\":null,\"op\":\"create\",\"path\":\"records/y.md\",\"seq\":3,\"ts\":\"2026-01-01T00:00:00Z\"}\n").unwrap();
    }
    let err = log.read_log().unwrap_err();
    assert!(err.to_string().contains("中部损坏"), "{err}");
}

#[test]
fn diff_since_separates_log_filter_and_drift() {
    let (_t, store) = fresh_store();
    let rel = "records/task/p/a.md";
    hand_write(&store, rel, "0198a", "task", 1, "v1");
    event::reconcile_backfill(&store, "t", None).unwrap();
    hand_write(&store, rel, "0198a", "task", 2, "v2");
    event::reconcile_backfill(&store, "t", None).unwrap();
    // 手改 v3（未登记 → drift）。
    hand_write(&store, rel, "0198a", "task", 2, "v3-hand-edit");
    let d = mem_core::drift::diff_since(&store, 0, 200, 100).unwrap();
    assert_eq!(d.changes_since.len(), 1, "A：按记录去重（一条记录一组变更史）");
    assert_eq!(d.changes_since[0].history.len(), 2, "变更史完整");
    assert_eq!(d.current_drift.items.len(), 1, "B：未登记变更独立呈现");
    assert!(d.current_drift.items[0].note.contains("backfill 待执行"));
    assert!(d.current_drift.items[0].token.is_some(), "漂移项附当前令牌");
    // 回填后转入 A、退出 B。
    event::reconcile_backfill(&store, "t", None).unwrap();
    let d2 = mem_core::drift::diff_since(&store, 0, 200, 100).unwrap();
    assert_eq!(d2.changes_since[0].history.len(), 3, "回填后转入 A");
    assert!(d2.current_drift.items.is_empty());
    // 游标过滤只作用于 A；B 不受游标影响。
    let d3 = mem_core::drift::diff_since(&store, 2, 200, 100).unwrap();
    assert_eq!(d3.changes_since[0].history.len(), 1, "游标过滤只作用于 A");
    assert!(d3.current_drift.items.is_empty());
}

#[test]
fn r16_reused_path_keeps_drift_identity_with_original_tombstone() {
    let (_t, store) = fresh_store();
    let rel = "records/task/p/reused.md";
    hand_write(&store, rel, "id-a", "task", 1, "A");
    event::reconcile_backfill(&store, "t", None).unwrap();
    let attic = mem_core::paths::attic_rel(rel).unwrap();
    std::fs::create_dir_all(store.abs(&attic).parent().unwrap()).unwrap();
    std::fs::rename(store.abs(rel), store.abs(&attic)).unwrap();
    event::reconcile_backfill(&store, "t", None).unwrap();
    hand_write(&store, rel, "id-b", "task", 1, "B");
    event::reconcile_backfill(&store, "t", None).unwrap();

    let before = mem_core::drift::diff_since(&store, 999_999, 200, 100).unwrap();
    assert!(before.current_drift.items.is_empty(), "A 已在 attic，B 已登记，均应稳定");
    let report = event::reconcile_backfill(&store, "t", None).unwrap();
    assert!(report.appended.is_empty());
    let after = mem_core::drift::diff_since(&store, 999_999, 200, 100).unwrap();
    assert!(after.current_drift.items.is_empty());

    let rewrite = |path: &str, body: &[u8]| {
        let old = std::fs::read(store.abs(path)).unwrap();
        let rec = mem_core::record::Record::from_bytes(&old).unwrap();
        let bytes = mem_core::record::serialize_new(&rec.envelope, &rec.payload, body);
        std::fs::write(store.abs(path), &bytes).unwrap();
        bytes
    };
    let a_bytes = rewrite(&attic, b"A changed");
    let drift_a = mem_core::drift::diff_since(&store, 999_999, 200, 100).unwrap();
    assert_eq!(drift_a.current_drift.items.len(), 1);
    assert_eq!(drift_a.current_drift.items[0].id, "id-a");
    assert_eq!(drift_a.current_drift.items[0].cur_path.as_deref(), Some(attic.as_str()));
    assert_eq!(drift_a.current_drift.items[0].token.as_ref().unwrap().digest,
        mem_core::token::sha256_hex(&a_bytes));
    event::reconcile_backfill(&store, "t", None).unwrap();

    let b_bytes = rewrite(rel, b"B changed");
    let drift_b = mem_core::drift::diff_since(&store, 999_999, 200, 100).unwrap();
    assert_eq!(drift_b.current_drift.items.len(), 1);
    assert_eq!(drift_b.current_drift.items[0].id, "id-b");
    assert_eq!(drift_b.current_drift.items[0].cur_path.as_deref(), Some(rel));
    assert_eq!(drift_b.current_drift.items[0].token.as_ref().unwrap().digest,
        mem_core::token::sha256_hex(&b_bytes));
}

#[test]
fn diff_works_without_git() {
    // 与 git 完全无关（红线10/26）：目录里没有 .git。
    let (_t, store) = fresh_store();
    assert!(!store.root().join(".git").exists());
    let rel = "records/task/p/a.md";
    hand_write(&store, rel, "0198a", "task", 1, "x");
    event::reconcile_backfill(&store, "t", None).unwrap();
    std::fs::remove_file(store.abs(rel)).unwrap();
    // 回填登记外部删除后，diff 全部经 A 呈现。
    event::reconcile_backfill(&store, "t", None).unwrap();
    let d = mem_core::drift::diff_since(&store, 0, 200, 100).unwrap();
    assert!(d.current_drift.items.is_empty(), "外部删除已由回填登记");
    assert_eq!(d.changes_since.len(), 1, "按记录去重：一组变更史（backfill+external_delete）");
    assert_eq!(d.changes_since[0].history.len(), 2);
}

#[test]
fn drift_budget_is_independent_and_truncates() {
    let (_t, store) = fresh_store();
    for i in 0..5 {
        let rel = format!("records/task/p/item-{i}.md");
        hand_write(&store, &rel, &format!("0198{i:04}"), "task", 1, "x");
    }
    let d = mem_core::drift::diff_since(&store, 0, 200, 3).unwrap();
    assert_eq!(d.current_drift.items.len(), 3, "current_drift 独立分页预算");
    assert!(d.current_drift.truncated, "截断必须显式标注（红线9）");
    // A 预算独立。
    let d2 = mem_core::drift::diff_since(&store, 0, 2, 100).unwrap();
    assert_eq!(d2.changes_since.len(), 0);
}
