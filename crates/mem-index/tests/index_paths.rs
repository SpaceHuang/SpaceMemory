//! 店级索引数据库与 snapshot 锁必须遵守 store 路径边界。

fn fresh_store() -> (tempfile::TempDir, mem_core::store::Store) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("store");
    mem_core::store::Store::create(&root).unwrap();
    let store = mem_core::store::Store::open(&root, mem_core::store::OpenMode::Auto).unwrap();
    (tmp, store)
}

fn seed_records(store: &mem_core::store::Store, count: usize, text_len: usize) -> Vec<String> {
    let text = "x".repeat(text_len);
    let mut ids = Vec::with_capacity(count);
    for i in 0..count {
        let id = format!("0198f7a9-0000-7cd2-9e3f-{i:012x}");
        let envelope = mem_core::envelope::Envelope {
            id: id.clone(),
            r#type: "task".into(),
            created: "2026-11-23T00:00:00Z".into(),
            modified: "2026-11-23T00:00:00Z".into(),
            version: 1,
            principal: None,
            refs: vec![],
            aliases: vec![],
            evidence_cutoff_seq: None,
            commit: None,
            extra: Default::default(),
        };
        let bytes = mem_core::record::serialize_new(
            &envelope,
            &serde_json::json!({"slug": format!("task-{i}"), "title": text}),
            b"",
        );
        let path = store.abs(format!("records/task/{id}.md"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
        ids.push(id);
    }
    ids
}

fn hot_journal_path(store: &mem_core::store::Store) -> std::path::PathBuf {
    store.abs("index/index.sqlite-journal")
}

fn assert_file_matches(path: &std::path::Path, expected: &[u8]) {
    let actual = std::fs::read(path).unwrap();
    assert_eq!(
        actual.len(),
        expected.len(),
        "文件长度改变：{}",
        path.display()
    );
    assert_eq!(
        mem_core::token::sha256_hex(&actual),
        mem_core::token::sha256_hex(expected),
        "文件内容改变：{}",
        path.display()
    );
}

#[test]
fn child_leave_hot_index_rollback_journal() {
    let Ok(root) = std::env::var("SM_HOT_INDEX_ROOT") else {
        return;
    };
    let store =
        mem_core::store::Store::open(std::path::Path::new(&root), mem_core::store::OpenMode::Auto)
            .unwrap();
    let db = store.abs("index/index.sqlite");
    let connection = rusqlite::Connection::open(db).unwrap();
    connection
        .execute_batch(
            "BEGIN IMMEDIATE;
             UPDATE records SET payload_json = payload_json || 'uncommitted-child-write';",
        )
        .unwrap();
    assert!(hot_journal_path(&store).is_file());
    std::process::exit(73);
}

#[test]
fn writable_index_close_never_checkpoints_or_changes_database_outside_snapshot_lock() {
    let (_tmp, store) = fresh_store();
    let mut setup = mem_index::Index::open_store(&store).unwrap();
    setup.rebuild(&store).unwrap();
    drop(setup);

    let index = mem_index::Index::open_store(&store).unwrap();
    let db = store.abs("index/index.sqlite");
    let wal = store.abs("index/index.sqlite-wal");
    let snapshot = store.abs("index/index.sqlite.lock");
    assert!(
        !wal.exists(),
        "writable indexes use rollback journaling, so close has no WAL checkpoint"
    );
    let db_before = std::fs::read(&db).unwrap();
    let shared_snapshot = mem_core::platform::LockFile::acquire_shared(&snapshot).unwrap();

    let closer = std::thread::spawn(move || drop(index));
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert!(
        closer.is_finished(),
        "closing must not need a checkpoint lock"
    );
    assert_file_matches(&db, &db_before);
    closer.join().unwrap();
    assert!(!wal.exists());
    drop(shared_snapshot);

    let reopened = mem_index::Index::open_store_readonly(&store).unwrap();
    assert_eq!(reopened.epoch().unwrap(), 1);
}

#[test]
fn failed_rebuild_rolls_back_inside_write_locks_and_can_retry() {
    let (_tmp, store) = fresh_store();
    let ids = seed_records(&store, 770, 8 * 1024);
    let mut initial = mem_index::Index::open_store(&store).unwrap();
    initial.rebuild(&store).unwrap();
    drop(initial);

    let db = store.abs("index/index.sqlite");
    let connection = rusqlite::Connection::open(&db).unwrap();
    connection
        .execute_batch(&format!(
            "CREATE TRIGGER fail_last_index_row BEFORE INSERT ON records
             WHEN NEW.id = '{}' BEGIN SELECT RAISE(ABORT, 'injected rebuild failure'); END;",
            ids.last().unwrap()
        ))
        .unwrap();
    drop(connection);

    let mut index = mem_index::Index::open_store(&store).unwrap();
    let db_before = std::fs::read(&db).unwrap();
    let error = index.rebuild(&store).unwrap_err();
    assert!(error.to_string().contains("injected rebuild failure"));
    assert_file_matches(&db, &db_before);
    assert!(
        !hot_journal_path(&store).exists(),
        "error return must end transaction"
    );

    let shared_snapshot =
        mem_core::platform::LockFile::acquire_shared(&store.abs("index/index.sqlite.lock"))
            .unwrap();
    let before_close = std::fs::read(&db).unwrap();
    drop(index);
    assert_file_matches(&db, &before_close);
    assert!(!hot_journal_path(&store).exists());
    drop(shared_snapshot);

    let connection = rusqlite::Connection::open(&db).unwrap();
    connection
        .execute_batch("DROP TRIGGER fail_last_index_row;")
        .unwrap();
    drop(connection);
    let mut retry = mem_index::Index::open_store(&store).unwrap();
    assert_eq!(retry.rebuild(&store).unwrap().0, 770);
    assert_eq!(retry.epoch().unwrap(), 2);
}

#[test]
fn open_proved_falls_back_without_touching_hot_rollback_journal_and_maintenance_recovers() {
    let (_tmp, store) = fresh_store();
    seed_records(&store, 1, 32);
    let mut index = mem_index::Index::open_store(&store).unwrap();
    index.rebuild(&store).unwrap();
    let epoch = index.epoch().unwrap();
    drop(index);

    let ts = "2026-11-23T00:00:00.000Z";
    let checkpoint = mem_core::freshness::Checkpoint {
        format_version: store.config().format_version,
        ts: ts.into(),
        entries: mem_core::freshness::stat_snapshot(&store),
        index_epoch: epoch,
    };
    let checkpoint_path = mem_core::freshness::checkpoint_path(&store);
    std::fs::write(&checkpoint_path, serde_json::to_vec(&checkpoint).unwrap()).unwrap();
    let hello = mem_core::sidecar::Hello {
        version: 1,
        backend: "hot-journal-test".into(),
        undigested: Some(0),
        max_seq: 0,
        last_rebuild: ts.into(),
    };

    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "child_leave_hot_index_rollback_journal",
            "--nocapture",
        ])
        .env("SM_HOT_INDEX_ROOT", store.root())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let status = child.wait().unwrap();
    let stderr = child
        .stderr
        .take()
        .map(|mut s| {
            use std::io::Read;
            let mut b = String::new();
            let _ = s.read_to_string(&mut b);
            b
        })
        .unwrap_or_default();
    assert_eq!(
        status.code(),
        Some(73),
        "child must exit without destructors: {stderr}"
    );
    let journal = hot_journal_path(&store);
    assert!(
        journal.is_file(),
        "child exit must leave its rollback journal"
    );

    let db_before = std::fs::read(store.abs("index/index.sqlite")).unwrap();
    let journal_before = std::fs::read(&journal).unwrap();
    let checkpoint_before = std::fs::read(&checkpoint_path).unwrap();
    let proved = mem_index::open_proved(&store, Some(&hello));
    assert!(matches!(
        proved.mode,
        mem_core::freshness::ReadMode::Direct { .. }
    ));
    assert!(proved.index.is_none());
    assert_file_matches(&store.abs("index/index.sqlite"), &db_before);
    assert_file_matches(&journal, &journal_before);
    assert_file_matches(&checkpoint_path, &checkpoint_before);
    assert_eq!(mem_core::ops::scan_all(&store).unwrap().records.len(), 1);

    let mut recovered = mem_index::Index::open_store(&store).unwrap();
    recovered.rebuild(&store).unwrap();
    assert!(
        !journal.exists(),
        "writable maintenance rebuild must recover hot journal"
    );
    let recovered_epoch = recovered.epoch().unwrap();
    drop(recovered);
    let recovered_checkpoint = mem_core::freshness::Checkpoint {
        format_version: store.config().format_version,
        ts: "2026-11-23T00:01:00.000Z".into(),
        entries: mem_core::freshness::stat_snapshot(&store),
        index_epoch: recovered_epoch,
    };
    std::fs::write(
        &checkpoint_path,
        serde_json::to_vec(&recovered_checkpoint).unwrap(),
    )
    .unwrap();
    let recovered_hello = mem_core::sidecar::Hello {
        last_rebuild: recovered_checkpoint.ts.clone(),
        ..hello
    };
    let proved = mem_index::open_proved(&store, Some(&recovered_hello));
    assert!(matches!(
        proved.mode,
        mem_core::freshness::ReadMode::Indexed { .. }
    ));
    assert_eq!(
        proved
            .index
            .unwrap()
            .query_by_type("task", 10, 0)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn child_index_rebuild_worker() {
    let Ok(root) = std::env::var("SM_INDEX_LOCK_ROOT") else {
        return;
    };
    let ready = std::path::PathBuf::from(std::env::var("SM_INDEX_LOCK_READY").unwrap());
    let start = std::path::PathBuf::from(std::env::var("SM_INDEX_LOCK_START").unwrap());
    let store =
        mem_core::store::Store::open(root.as_ref(), mem_core::store::OpenMode::Auto).unwrap();
    let mut index = mem_index::Index::open_store(&store).unwrap();
    std::fs::write(ready, b"ready").unwrap();
    wait_for_file(&start);
    index.rebuild(&store).unwrap();
}

#[test]
fn child_index_maintenance_worker() {
    let Ok(root) = std::env::var("SM_INDEX_LOCK_ROOT") else {
        return;
    };
    let ready = std::path::PathBuf::from(std::env::var("SM_INDEX_MAINT_READY").unwrap());
    let store =
        mem_core::store::Store::open(root.as_ref(), mem_core::store::OpenMode::Auto).unwrap();
    let permit = store.acquire_index_maintenance().unwrap();
    std::fs::write(ready, b"ready").unwrap();
    let _index = mem_index::Index::open_store_under_maintenance(&store, &permit).unwrap();
}

#[test]
fn index_rebuild_and_explicit_maintenance_do_not_reverse_lock_order() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("lock-order-store");
    mem_core::store::Store::create(&root).unwrap();
    let writer_ready = tmp.path().join("writer.ready");
    let writer_start = tmp.path().join("writer.start");
    let maintenance_ready = tmp.path().join("maintenance.ready");
    let mut writer = spawn_index_lock_child(
        "child_index_rebuild_worker",
        &root,
        "SM_INDEX_LOCK_READY",
        &writer_ready,
        Some(("SM_INDEX_LOCK_START", &writer_start)),
    );
    wait_for_file(&writer_ready);
    let mut maintenance = spawn_index_lock_child(
        "child_index_maintenance_worker",
        &root,
        "SM_INDEX_MAINT_READY",
        &maintenance_ready,
        None,
    );
    wait_for_file(&maintenance_ready);
    std::fs::write(&writer_start, b"go").unwrap();

    let writer_ok = wait_child_bounded(&mut writer);
    let maintenance_ok = wait_child_bounded(&mut maintenance);
    assert!(
        writer_ok && maintenance_ok,
        "索引写入路径必须在有限时间内按同一锁序完成"
    );
}

fn spawn_index_lock_child(
    test_name: &str,
    root: &std::path::Path,
    ready_env: &str,
    ready: &std::path::Path,
    gate: Option<(&str, &std::path::Path)>,
) -> std::process::Child {
    let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", test_name, "--nocapture"])
        .env("SM_INDEX_LOCK_ROOT", root)
        .env(ready_env, ready)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    if let Some((name, path)) = gate {
        cmd.env(name, path);
    }
    cmd.spawn().unwrap()
}

fn wait_for_file(path: &std::path::Path) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while !path.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "等待同步文件超时: {}",
            path.display()
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn wait_child_bounded(child: &mut std::process::Child) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status.success();
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn writable_store_index_open_refuses_missing_feature_before_creating_database() {
    let (_tmp, store) = fresh_store();
    let config_path = store.abs("mem.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    config["required_features"] = serde_json::json!(["spacememory.future/v1"]);
    std::fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();

    let err = mem_index::Index::open_store(&store)
        .err()
        .expect("不兼容店不得创建派生索引");
    assert!(
        matches!(err, mem_core::error::MemError::MissingFeatures(_)),
        "{err}"
    );
    assert!(!store.abs("index/index.sqlite").exists());
}

#[test]
fn an_index_handle_cannot_rebuild_into_another_stores_database() {
    let (_tmp, first) = fresh_store();
    let second_root = first.root().parent().unwrap().join("second-store");
    mem_core::store::Store::create(&second_root).unwrap();
    let second =
        mem_core::store::Store::open(&second_root, mem_core::store::OpenMode::Auto).unwrap();
    let mut index = mem_index::Index::open_store(&first).unwrap();

    let error = index.rebuild(&second).unwrap_err();

    assert!(
        matches!(error, mem_core::error::MemError::BadArgument(_)),
        "{error}"
    );
    assert!(!second.abs("index/index.sqlite").exists());
    let permit = first.acquire_index_maintenance().unwrap();
    let error = mem_index::Index::open_store_under_maintenance(&second, &permit)
        .err()
        .unwrap();
    assert!(
        matches!(error, mem_core::error::MemError::BadArgument(_)),
        "{error}"
    );
}

#[test]
fn migrated_format_rejects_legacy_index_checkpoint_as_proof() {
    let (_tmp, store) = fresh_store();
    let mut index = mem_index::Index::open_store(&store).unwrap();
    index.rebuild(&store).unwrap();
    let epoch = index.epoch().unwrap();
    drop(index);

    let checkpoint = mem_core::freshness::Checkpoint {
        format_version: "1".into(),
        ts: mem_core::clock::now_rfc3339(),
        entries: mem_core::freshness::stat_snapshot(&store),
        index_epoch: epoch,
    };
    let checkpoint_path = mem_core::freshness::checkpoint_path(&store);
    std::fs::write(&checkpoint_path, serde_json::to_vec(&checkpoint).unwrap()).unwrap();
    let index_bytes_before = std::fs::read(store.abs("index/index.sqlite")).unwrap();
    let checkpoint_bytes_before = std::fs::read(&checkpoint_path).unwrap();

    let config_path = store.abs("mem.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    config["format_version"] = serde_json::json!(1);
    config.as_object_mut().unwrap().remove("required_features");
    std::fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
    mem_core::store::Store::migrate_format_v1(store.root(), "legacy-index-fixture").unwrap();

    let migrated =
        mem_core::store::Store::open(store.root(), mem_core::store::OpenMode::Auto).unwrap();
    let hello = mem_core::sidecar::Hello {
        version: 1,
        backend: "legacy-index-fixture".into(),
        undigested: Some(0),
        max_seq: 0,
        last_rebuild: checkpoint.ts,
    };
    let proved = mem_index::open_proved(&migrated, Some(&hello));
    assert!(
        matches!(proved.mode, mem_core::freshness::ReadMode::Direct { ref reason }
        if reason == "检查点基础格式与数据目录不匹配"),
        "旧格式证明必须失效并直读: {:?}",
        proved.mode
    );
    assert!(proved.index.is_none(), "迁移前索引不得进入查询快路径");
    assert_eq!(
        std::fs::read(migrated.abs("index/index.sqlite")).unwrap(),
        index_bytes_before,
        "迁移不会悄悄重写旧派生索引"
    );
    assert_eq!(
        std::fs::read(mem_core::freshness::checkpoint_path(&migrated)).unwrap(),
        checkpoint_bytes_before,
        "旧检查点保留供核验，但不能再作为新格式证明"
    );
}

#[test]
fn open_proved_rechecks_disk_features_on_an_existing_store_handle() {
    let (_tmp, store) = fresh_store();
    let mut index = mem_index::Index::open_store(&store).unwrap();
    index.rebuild(&store).unwrap();
    let epoch = index.epoch().unwrap();
    drop(index);

    let checkpoint = mem_core::freshness::Checkpoint {
        format_version: store.config().format_version,
        ts: mem_core::clock::now_rfc3339(),
        entries: mem_core::freshness::stat_snapshot(&store),
        index_epoch: epoch,
    };
    let checkpoint_path = mem_core::freshness::checkpoint_path(&store);
    std::fs::write(&checkpoint_path, serde_json::to_vec(&checkpoint).unwrap()).unwrap();
    let hello = mem_core::sidecar::Hello {
        version: 1,
        backend: "feature-recheck-test".into(),
        undigested: Some(0),
        max_seq: 0,
        last_rebuild: checkpoint.ts,
    };

    let config_path = store.abs("mem.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    config["required_features"] = serde_json::json!(["spacememory.future/v1"]);
    std::fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();

    let proved = mem_index::open_proved(&store, Some(&hello));
    assert!(
        matches!(proved.mode, mem_core::freshness::ReadMode::Direct { .. }),
        "旧句柄也必须重查磁盘必需特性，不能让未支持的索引语义进入快路径: {:?}",
        proved.mode
    );
    assert!(proved.index.is_none());
}

#[test]
fn missing_required_features_field_blocks_old_store_and_index_handles_without_mutation() {
    let (_tmp, store) = fresh_store();
    let mut index = mem_index::Index::open_store(&store).unwrap();
    index.rebuild(&store).unwrap();
    let events_before = std::fs::read(store.abs("events.jsonl")).unwrap();
    let index_before = std::fs::read(store.abs("index/index.sqlite")).unwrap();
    let path = store.abs("mem.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    config.as_object_mut().unwrap().remove("required_features");
    std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    let config_after_corruption = std::fs::read(&path).unwrap();

    assert!(matches!(
        store.read_compatible_missing_features(),
        Err(mem_core::error::MemError::Corrupt { .. })
    ));
    assert!(matches!(
        index.rebuild(&store),
        Err(mem_core::error::MemError::Corrupt { .. })
    ));
    assert!(matches!(
        mem_index::Index::open_store(&store),
        Err(mem_core::error::MemError::Corrupt { .. })
    ));
    assert_eq!(std::fs::read(&path).unwrap(), config_after_corruption);
    assert_eq!(
        std::fs::read(store.abs("events.jsonl")).unwrap(),
        events_before
    );
    assert_eq!(
        std::fs::read(store.abs("index/index.sqlite")).unwrap(),
        index_before
    );
}

#[cfg(unix)]
#[test]
fn store_index_open_rejects_snapshot_lock_symlink_before_creating_target() {
    let (_tmp, store) = fresh_store();
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("new-lock-target");
    std::os::unix::fs::symlink(&target, store.abs("index/index.sqlite.lock")).unwrap();

    let err = mem_index::Index::open_store(&store)
        .err()
        .expect("非法店级索引必须拒绝");
    assert!(
        matches!(err, mem_core::error::MemError::BadArgument(_)),
        "{err}"
    );
    assert!(!target.exists(), "外链锁目标不得被创建");
    assert!(
        !store.abs("index/index.sqlite").exists(),
        "不得先创建数据库"
    );
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn store_index_open_rejects_symlinked_index_directory_without_outside_files() {
    let (_tmp, store) = fresh_store();
    let outside = tempfile::tempdir().unwrap();
    std::fs::remove_dir_all(store.abs("index")).unwrap();
    std::os::unix::fs::symlink(outside.path(), store.abs("index")).unwrap();

    let err = mem_index::Index::open_store(&store)
        .err()
        .expect("非法店级索引必须拒绝");
    assert!(
        matches!(err, mem_core::error::MemError::BadArgument(_)),
        "{err}"
    );
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn store_index_open_rejects_database_symlink_without_mutating_external_database() {
    let (_tmp, store) = fresh_store();
    let outside = tempfile::tempdir().unwrap();
    let external_db = outside.path().join("user.sqlite");
    {
        let conn = rusqlite::Connection::open(&external_db).unwrap();
        conn.execute_batch(
            "CREATE TABLE user_data(value TEXT); INSERT INTO user_data VALUES ('valuable');",
        )
        .unwrap();
    }
    let before = std::fs::read(&external_db).unwrap();
    std::os::unix::fs::symlink(&external_db, store.abs("index/index.sqlite")).unwrap();

    let err = mem_index::Index::open_store(&store)
        .err()
        .expect("非法店级索引必须拒绝");
    assert!(
        matches!(err, mem_core::error::MemError::BadArgument(_)),
        "{err}"
    );
    assert_eq!(
        std::fs::read(&external_db).unwrap(),
        before,
        "店外 SQLite 字节必须不变"
    );
    let conn = rusqlite::Connection::open(&external_db).unwrap();
    let value: String = conn
        .query_row("SELECT value FROM user_data", [], |r| r.get(0))
        .unwrap();
    assert_eq!(value, "valuable");
    let tables: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN ('records','refs','meta','bad_records')",
        [], |r| r.get(0),
    ).unwrap();
    assert_eq!(tables, 0, "不得把店外数据库初始化成索引");
}

#[cfg(unix)]
#[test]
fn readonly_store_index_rejects_external_lock_and_open_proved_falls_back_to_direct() {
    let (_tmp, store) = fresh_store();
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("unrelated.txt");
    let sentinel = b"do not lock or modify";
    std::fs::write(&target, sentinel).unwrap();
    std::os::unix::fs::symlink(&target, store.abs("index/index.sqlite.lock")).unwrap();

    let err = mem_index::Index::open_store_readonly(&store)
        .err()
        .expect("非法只读店级索引必须拒绝");
    assert!(
        matches!(err, mem_core::error::MemError::BadArgument(_)),
        "{err}"
    );

    let cp = mem_core::freshness::Checkpoint {
        format_version: store.config().format_version.clone(),
        ts: mem_core::clock::now_rfc3339(),
        entries: Default::default(),
        index_epoch: 1,
    };
    let permit = store.acquire_index_maintenance().unwrap();
    mem_core::freshness::save_checkpoint(&store, &cp, &permit).unwrap();
    let hello = mem_core::sidecar::Hello {
        version: 1,
        backend: "rdc".into(),
        undigested: Some(0),
        max_seq: 0,
        last_rebuild: cp.ts.clone(),
    };
    let proved = mem_index::open_proved(&store, Some(&hello));
    assert!(matches!(
        proved.mode,
        mem_core::freshness::ReadMode::Direct { .. }
    ));
    assert!(proved.index.is_none(), "非法店索引不可进入查询路径");
    assert_eq!(std::fs::read(&target).unwrap(), sentinel);
}
