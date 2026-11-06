//! §8-4b 锁协议测试：门闩（持有式屏障）→ 记录锁（缺失记录可锁）→ 序列锁（叶子）。
//!
//! 锚点：D3/D17/红线20（屏障必须持有而非探测；锁序全库统一）、红线22（陈旧锁判定：
//! PID 死亡即抢、存活永不抢、超时只许等待方放弃）、红线24（缺失记录使用同规范路径锁，
//! 锁文件独立于记录文件存在）。

use std::time::Duration;

use mem_core::lock;
use mem_core::store::{OpenMode, Store};

fn fresh_store() -> (tempfile::TempDir, Store) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("store");
    Store::create(&root).unwrap();
    let store = Store::open(&root, OpenMode::Auto).unwrap();
    (tmp, store)
}

const REC: &str = "records/task/p1/login-api.md";

// ---------- 门闩：共享共存 / 独占互斥（红线20） ----------

#[test]
fn barrier_shared_allows_concurrent_writers() {
    let (_t, store) = fresh_store();
    let g1 = lock::barrier_shared(&store).unwrap();
    let g2 = lock::barrier_shared(&store).unwrap();
    let g3 = lock::barrier_shared(&store).unwrap();
    drop((g1, g2, g3));
}

#[test]
fn barrier_exclusive_excludes_shared_and_exclusive() {
    let (_t, store) = fresh_store();
    {
        let s = lock::barrier_shared(&store).unwrap();
        assert!(lock::try_barrier_exclusive(&store).is_err(), "共享持有期间独占必须失败");
        drop(s);
    }
    {
        let x = lock::barrier_exclusive(&store).unwrap();
        assert!(lock::try_barrier_exclusive(&store).is_err(), "独占持有期间第二个独占必须失败");
        assert!(lock::try_barrier_shared(&store).is_err(), "独占持有期间共享必须失败");
        drop(x);
    }
    let s = lock::barrier_shared(&store).unwrap();
    drop(s);
    let x = lock::barrier_exclusive(&store).unwrap();
    drop(x);
}

#[test]
fn barrier_is_held_not_probed() {
    // 红线20：屏障在 guard 存活期间持续有效（不是单次探测）——用线程观测：
    // 主线程持独占，子线程取共享必须阻塞到释放为止。
    let (_t, store) = fresh_store();
    let x = lock::barrier_exclusive(&store).unwrap();
    let store2 = {
        let root = store.root().to_path_buf();
        Store::open(&root, OpenMode::Auto).unwrap()
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let h = std::thread::spawn(move || {
        let g = lock::barrier_shared(&store2).unwrap();
        tx.send(()).unwrap();
        drop(g);
    });
    std::thread::sleep(Duration::from_millis(150));
    assert!(rx.try_recv().is_err(), "独占屏障持有期间，共享写不得进入（持有而非探测）");
    drop(x);
    h.join().unwrap();
    // 子线程已成功进入并退出。
}

// ---------- 记录锁：缺失记录可锁 / 互斥 / 超时（红线22/24） ----------

#[test]
fn record_lock_works_when_record_file_missing() {
    let (_t, store) = fresh_store();
    // 记录文件不存在（待创建/absent），锁文件独立存在。
    assert!(!store.abs(REC).exists());
    let g = lock::record_lock(&store, REC, Duration::from_secs(2)).unwrap();
    assert!(store.abs(format!("{REC}.lock")).is_file(), "锁文件独立于记录存在");
    // 锁文件内容 JSON：pid/host/ts。
    let content = std::fs::read_to_string(store.abs(format!("{REC}.lock"))).unwrap();
    let v: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(v["pid"], serde_json::json!(std::process::id()));
    drop(g);
}

#[test]
fn record_lock_is_exclusive_within_process() {
    let (_t, store) = fresh_store();
    let g = lock::record_lock(&store, REC, Duration::from_secs(2)).unwrap();
    assert!(lock::try_record_lock(&store, REC).unwrap().is_none(), "同进程重入必须忙");
    // 不同路径互不影响。
    let other = lock::try_record_lock(&store, "records/task/p2/other.md").unwrap();
    assert!(other.is_some());
    drop(other);
    drop(g);
    let again = lock::try_record_lock(&store, REC).unwrap();
    assert!(again.is_some(), "释放后可重取");
}

#[test]
fn record_lock_timeout_reports_holder() {
    let (_t, store) = fresh_store();
    let g = lock::record_lock(&store, REC, Duration::from_secs(2)).unwrap();
    let err = lock::record_lock(&store, REC, Duration::from_millis(120)).unwrap_err();
    match err {
        mem_core::error::MemError::LockTimeout { lock_path, holder } => {
            assert!(lock_path.ends_with(".lock"), "{lock_path}");
            let pid = holder.expect("持有者信息必须随超时报告");
            assert!(pid.contains(&std::process::id().to_string()), "{pid}");
        }
        e => panic!("应 LockTimeout，实际 {e}"),
    }
    drop(g);
}

#[test]
fn stale_lock_with_dead_pid_is_stealable() {
    let (_t, store) = fresh_store();
    // 模拟崩溃残留：锁文件在、内容 PID 已死、无 OS 锁（进程死亡自动释放）。
    let lock_file = store.abs(format!("{REC}.lock"));
    std::fs::create_dir_all(lock_file.parent().unwrap()).unwrap();
    std::fs::write(
        &lock_file,
        format!("{{\"pid\":{},\"host\":\"ghost\",\"ts\":\"2020-01-01T00:00:00Z\"}}", u32::MAX - 7),
    )
    .unwrap();
    let g = lock::record_lock(&store, REC, Duration::from_millis(500)).unwrap();
    // 新持有者覆盖 owner 信息。
    let content = std::fs::read_to_string(&lock_file).unwrap();
    let v: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(v["pid"], serde_json::json!(std::process::id()));
    drop(g);
}

#[test]
fn live_holder_is_never_stolen() {
    let (_t, store) = fresh_store();
    let g = lock::record_lock(&store, REC, Duration::from_secs(2)).unwrap();
    // 持有者存活（本进程）→ 等待方只能超时放弃，不得抢（红线22）。
    let t0 = std::time::Instant::now();
    let err = lock::record_lock(&store, REC, Duration::from_millis(150)).unwrap_err();
    assert!(matches!(err, mem_core::error::MemError::LockTimeout { .. }));
    assert!(t0.elapsed() >= Duration::from_millis(140), "必须等待到超时而不是立即放弃或抢锁");
    drop(g);
}

// ---------- 序列锁：叶子、全库串行（红线24） ----------

#[test]
fn sequence_lock_serializes_concurrent_users() {
    let (_t, store) = fresh_store();
    let counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let max_concurrent = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut handles = Vec::new();
    for _ in 0..8 {
        let c = counter.clone();
        let m = max_concurrent.clone();
        let store2 = {
            let root = store.root().to_path_buf();
            Store::open(&root, OpenMode::Auto).unwrap()
        };
        handles.push(std::thread::spawn(move || {
            lock::with_sequence_lock(&store2, || {
                let now = c.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                m.fetch_max(now, std::sync::atomic::Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(10));
                c.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            })
            .unwrap();
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(
        max_concurrent.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "序列锁内并发必须为 1（全库串行分配点）"
    );
}

// ---------- 锁序守护：门闩 → 记录锁 → 序列锁 ----------

#[test]
fn lock_order_barrier_then_record_then_sequence() {
    // 合法次序：共享门闩 → 记录锁 → 序列锁，全程嵌套无死锁。
    let (_t, store) = fresh_store();
    let b = lock::barrier_shared(&store).unwrap();
    let r = lock::record_lock(&store, REC, Duration::from_secs(2)).unwrap();
    lock::with_sequence_lock(&store, || {
        // 序列锁（叶子）内不再取上锁——这里只证明嵌套成立。
    })
    .unwrap();
    drop(r);
    drop(b);
}

#[test]
#[cfg(windows)]
fn foreign_host_lock_is_never_stolen() {
    // H6：owner.host ≠ 本机 → 视为存活（只等待/超时），不得用本机 PID 判死抢锁。
    // 前提：OS 锁真实被持有（windows 锁按句柄——本测试进程直接持底层 LockFile
    // 模拟"他机持有者"；若无人持 OS 锁则 acquire 本来就该成功，与本用例无关）。
    let (_t, store) = fresh_store();
    let lock_file = store.abs(format!("{REC}.lock"));
    std::fs::create_dir_all(lock_file.parent().unwrap()).unwrap();
    std::fs::write(
        &lock_file,
        format!(
            "{{\"pid\":{},\"host\":\"some-other-host\",\"ts\":\"2020-01-01T00:00:00Z\"}}",
            std::process::id()
        ),
    )
    .unwrap();
    let _os_lock = mem_core::platform::LockFile::acquire_exclusive(&lock_file).unwrap();
    let t0 = std::time::Instant::now();
    let err = lock::record_lock(&store, REC, Duration::from_millis(200)).unwrap_err();
    assert!(matches!(err, mem_core::error::MemError::LockTimeout { .. }), "{err}");
    assert!(t0.elapsed() >= Duration::from_millis(190), "跨主机持有者只等待不抢");
    // owner 文件未被污染（LockTimeout 报告的持有者 = 他机信息）。
    let content = std::fs::read_to_string(&lock_file).unwrap();
    assert!(content.contains("some-other-host"), "{content}");
}
