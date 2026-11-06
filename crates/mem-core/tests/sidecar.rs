//! §8-6b sidecar 测试：单实例握手 / 懒拉起 / 空闲自退 / 维护循环（D2 / 红线5）。
//!
//! 不变量验证：
//! - sidecar 不在 → 嵌入模式照常工作（降速不是故障）；
//! - sidecar 任意时刻可 kill（测试直接 drop 线程/进程）；
//! - 空闲自退（无握手 + 无变更超过 idle）。

use std::time::Duration;

use mem_core::sidecar;
use mem_core::store::{OpenMode, Store};

fn fresh_store() -> (tempfile::TempDir, Store) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("store");
    Store::create(&root).unwrap();
    let store = Store::open(&root, OpenMode::Auto).unwrap();
    (tmp, store)
}

#[test]
fn handshake_probe_and_idle_exit() {
    let (_t, store) = fresh_store();
    let store2 = {
        let root = store.root().to_path_buf();
        Store::open(&root, OpenMode::Auto).unwrap()
    };
    // sidecar 未启动：探测 None（嵌入模式，降速不是故障）。
    assert!(sidecar::probe(&store).is_none());
    // 启动（短空闲：1.5s 无握手无变更即自退）。
    let h = std::thread::spawn(move || {
        sidecar::serve(&store2, Duration::from_millis(1500), || false).unwrap()
    });
    // 等待就绪后握手。
    let mut hello = None;
    for _ in 0..40 {
        if let Some(h) = sidecar::probe(&store) {
            hello = Some(h);
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let hello = hello.expect("sidecar 就绪后握手成功");
    assert_eq!(hello.version, sidecar::SIDECAR_VERSION);
    // H7：后端可为 usn（提权/NTFS）或 rdc（非提权目录变更通知）——两者都是
    // 红线21 语义下的合法订阅源；none = 均不可用（永久直读形态）。
    assert!(
        hello.backend == "usn" || hello.backend == "rdc" || hello.backend == "none",
        "{hello:?}"
    );
    // 空闲自退：idle 后 serve 返回（线程结束）。
    let report = h.join().unwrap();
    assert!(report.idle_exited, "{report:?}");
    // 自退后探测再次失败。
    std::thread::sleep(Duration::from_millis(200));
    assert!(sidecar::probe(&store).is_none(), "空闲自退后管道应不可用");
}

#[test]
fn single_instance_enforced() {
    let (_t, store) = fresh_store();
    let store2 = {
        let root = store.root().to_path_buf();
        Store::open(&root, OpenMode::Auto).unwrap()
    };
    let store3 = {
        let root = store.root().to_path_buf();
        Store::open(&root, OpenMode::Auto).unwrap()
    };
    let h = std::thread::spawn(move || {
        sidecar::serve(&store2, Duration::from_secs(30), || false)
    });
    // 等 serve 持锁。
    std::thread::sleep(Duration::from_millis(300));
    let err = sidecar::serve(&store3, Duration::from_secs(30), || true).unwrap_err();
    assert!(matches!(err, mem_core::error::MemError::LockTimeout { .. }), "{err}");
    // 清理：stop 标志让第一个退出。
    std::thread::sleep(Duration::from_millis(1200));
    drop(h);
}

#[test]
fn maintenance_rebuilds_checkpoint_on_change() {
    let (_t, store) = fresh_store();
    let store2 = {
        let root = store.root().to_path_buf();
        Store::open(&root, OpenMode::Auto).unwrap()
    };
    let h = std::thread::spawn(move || {
        sidecar::serve(&store2, Duration::from_secs(4), || false).unwrap()
    });
    // sidecar 初始物化 → 检查点存在。
    let mut saw_checkpoint = false;
    for _ in 0..40 {
        if mem_core::freshness::load_checkpoint(&store).is_some() {
            saw_checkpoint = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(saw_checkpoint, "sidecar 初始物化刷新检查点（D5 条件 (a) 原料）");
    // 追加变更（写例程之外的直接事件）→ 维护循环检测 → 检查点时间戳前进。
    let cp1 = mem_core::freshness::load_checkpoint(&store).unwrap();
    mem_core::event::EventLog::new(&store)
        .append_simple("x", "records/x.md", mem_core::event::Op::Create, None, Some(1), None, Some("d"), "t")
        .unwrap();
    std::fs::write(
        store.abs("records/x.md"),
        mem_core::record::serialize_new(
            &mem_core::envelope::Envelope {
                id: "x-actual-id-not-uuid".into(),
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
            },
            &serde_json::json!({"title": "x"}),
            b"",
        ),
    )
    .unwrap();
    let mut advanced = false;
    for _ in 0..40 {
        if let Some(cp2) = mem_core::freshness::load_checkpoint(&store) {
            if cp2.ts > cp1.ts {
                advanced = true;
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(advanced, "维护循环检测变更并刷新检查点");
    drop(h);
}

#[test]
fn subscription_capability_matches_volume() {
    let (_t, store) = fresh_store();
    // 能力探测与卷一致（开放问题39 v1：NTFS → usn；其他 → none = 永久直读）。
    let backend = mem_core::freshness::probe_subscription_backend(store.root());
    let fs = mem_core::platform::fs_name_for(store.root());
    match (fs.as_deref(), backend) {
        (Some(name), b) if name.eq_ignore_ascii_case("NTFS") => assert_eq!(b, Some("usn")),
        _ => assert_eq!(backend, None, "非 NTFS 卷必须声明不支持订阅"),
    }
}
