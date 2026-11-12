//! 店级索引数据库与 snapshot 锁必须遵守 store 路径边界。

#[cfg(unix)]
fn fresh_store() -> (tempfile::TempDir, mem_core::store::Store) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("store");
    mem_core::store::Store::create(&root).unwrap();
    let store = mem_core::store::Store::open(&root, mem_core::store::OpenMode::Auto).unwrap();
    (tmp, store)
}

#[cfg(unix)]
#[test]
fn store_index_open_rejects_snapshot_lock_symlink_before_creating_target() {
    let (_tmp, store) = fresh_store();
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("new-lock-target");
    std::os::unix::fs::symlink(&target, store.abs("index/index.sqlite.lock")).unwrap();

    let err = mem_index::Index::open_store(&store).err().expect("非法店级索引必须拒绝");
    assert!(matches!(err, mem_core::error::MemError::BadArgument(_)), "{err}");
    assert!(!target.exists(), "外链锁目标不得被创建");
    assert!(!store.abs("index/index.sqlite").exists(), "不得先创建数据库");
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn store_index_open_rejects_symlinked_index_directory_without_outside_files() {
    let (_tmp, store) = fresh_store();
    let outside = tempfile::tempdir().unwrap();
    std::fs::remove_dir_all(store.abs("index")).unwrap();
    std::os::unix::fs::symlink(outside.path(), store.abs("index")).unwrap();

    let err = mem_index::Index::open_store(&store).err().expect("非法店级索引必须拒绝");
    assert!(matches!(err, mem_core::error::MemError::BadArgument(_)), "{err}");
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
        conn.execute_batch("CREATE TABLE user_data(value TEXT); INSERT INTO user_data VALUES ('valuable');").unwrap();
    }
    let before = std::fs::read(&external_db).unwrap();
    std::os::unix::fs::symlink(&external_db, store.abs("index/index.sqlite")).unwrap();

    let err = mem_index::Index::open_store(&store).err().expect("非法店级索引必须拒绝");
    assert!(matches!(err, mem_core::error::MemError::BadArgument(_)), "{err}");
    assert_eq!(std::fs::read(&external_db).unwrap(), before, "店外 SQLite 字节必须不变");
    let conn = rusqlite::Connection::open(&external_db).unwrap();
    let value: String = conn.query_row("SELECT value FROM user_data", [], |r| r.get(0)).unwrap();
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

    let err = mem_index::Index::open_store_readonly(&store).err().expect("非法只读店级索引必须拒绝");
    assert!(matches!(err, mem_core::error::MemError::BadArgument(_)), "{err}");

    let cp = mem_core::freshness::Checkpoint {
        ts: mem_core::clock::now_rfc3339(),
        entries: Default::default(),
        index_epoch: 1,
    };
    mem_core::freshness::save_checkpoint(&store, &cp).unwrap();
    let hello = mem_core::sidecar::Hello {
        version: 1,
        backend: "rdc".into(),
        undigested: Some(0),
        max_seq: 0,
        last_rebuild: cp.ts.clone(),
    };
    let proved = mem_index::open_proved(&store, Some(&hello));
    assert!(matches!(proved.mode, mem_core::freshness::ReadMode::Direct { .. }));
    assert!(proved.index.is_none(), "非法店索引不可进入查询路径");
    assert_eq!(std::fs::read(&target).unwrap(), sentinel);
}
