//! 店级索引数据库与 snapshot 锁必须遵守 store 路径边界。

#[cfg(unix)]
fn fresh_store() -> (tempfile::TempDir, mem_core::store::Store) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("store");
    mem_core::store::Store::create(&root).unwrap();
    let store = mem_core::store::Store::open(&root, mem_core::store::OpenMode::Auto).unwrap();
    (tmp, store)
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
