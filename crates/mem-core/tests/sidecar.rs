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
#[cfg(debug_assertions)]
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
    let h = std::thread::spawn(move || sidecar::serve(&store2, Duration::from_secs(30), || false));
    // 等 serve 持锁。
    std::thread::sleep(Duration::from_millis(300));
    let err = sidecar::serve(&store3, Duration::from_secs(30), || true).unwrap_err();
    assert!(
        matches!(err, mem_core::error::MemError::LockTimeout { .. }),
        "{err}"
    );
    // 清理：stop 标志让第一个退出。
    std::thread::sleep(Duration::from_millis(1200));
    drop(h);
}

#[test]
fn old_rdc_zero_count_hello_cannot_authorize_indexed_reads() {
    let (_tmp, store) = fresh_store();
    let checkpoint = mem_core::freshness::Checkpoint {
        format_version: store.config().format_version.clone(),
        ts: "2026-11-24T00:00:00.000Z".into(),
        entries: mem_core::freshness::stat_snapshot(&store),
        index_epoch: 1,
    };
    let permit = store.acquire_index_maintenance().unwrap();
    mem_core::freshness::save_checkpoint(&store, &checkpoint, &permit).unwrap();
    let old_sidecar_hello = sidecar::Hello {
        version: sidecar::SIDECAR_VERSION,
        backend: "rdc".into(),
        undigested: Some(0),
        max_seq: 0,
        last_rebuild: String::new(),
    };

    let mode = mem_core::freshness::consult_with_hello(
        &store,
        Some(&checkpoint),
        Some(&old_sidecar_hello),
    );
    assert!(
        mode.is_direct(),
        "旧 sidecar 的 RDC 零计数不能授权索引查询：{mode:?}"
    );
}

#[test]
#[cfg(windows)] // RDC 只触发后台物化；无法证明查询新鲜度，故不发布检查点。
fn rdc_sidecar_does_not_publish_freshness_checkpoint() {
    let (_t, store) = fresh_store();
    let store2 = {
        let root = store.root().to_path_buf();
        Store::open(&root, OpenMode::Auto).unwrap()
    };
    let h = std::thread::spawn(move || {
        sidecar::serve(&store2, Duration::from_secs(4), || false).unwrap()
    });
    // 等 sidecar 启动；RDC 可以负责通知后台维护，但不能签发新鲜度检查点。
    let mut hello = None;
    for _ in 0..40 {
        if let Some(reply) = sidecar::probe(&store) {
            hello = Some(reply);
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(hello.is_some(), "sidecar 应成功启动并响应握手");
    if let Some(reply) = hello {
        if reply.backend == "rdc" {
            assert_eq!(reply.undigested, None, "RDC 不能作为查询新鲜度证明");
        }
    }
    assert!(
        mem_core::freshness::load_checkpoint(&store).is_none(),
        "RDC 不能发布新鲜度检查点"
    );
    // 追加变化后，sidecar 仍可触发后台维护；无论维护是否已完成，都不能签发证明。
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
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        mem_core::freshness::load_checkpoint(&store).is_none(),
        "后台维护不能把 RDC 零计数转换成新鲜度证明"
    );
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
