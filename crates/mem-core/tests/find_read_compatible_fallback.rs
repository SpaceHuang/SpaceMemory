//! 评审 B2 前向兼容路径：只读兼容缺项出现在返回边界时，find 必须放弃 indexed
//! 应答、整趟降级真相层直读（当前发布能力表为空——以 debug 专用受控能力表
//! override 构造该场景，不伪造发布声明）。
//!
//! 本文件独立成测试二进制：能力表 override 是进程级状态，与其他 find 测试
//! 隔离，避免并行测试交叉污染。

use mem_core::error::MemError;
use mem_core::find::{execute, FindParams};
use mem_core::freshness::ReadMode;
use mem_schema::TypeSet;
use mem_core::store::{OpenMode, Store};

#[test]
fn find_downgrades_to_direct_when_read_compatible_feature_appears_at_boundary() {
    mem_core::store::set_read_compatible_features_for_test(&["scan.future/v1"]);

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("store");
    Store::create(&root).unwrap();
    let store = Store::open(&root, OpenMode::Auto).unwrap();
    let types = TypeSet::load_dir(&root.join("types")).unwrap();

    // 种一条可检索记录（直接文件写入，与既有索引测试同法）。
    let envelope = mem_core::envelope::Envelope {
        id: "0198f7a9-0000-7cd2-9e3f-000000000001".into(),
        r#type: "task".into(),
        created: "2026-11-24T00:00:00Z".into(),
        modified: "2026-11-24T00:00:00Z".into(),
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
        &serde_json::json!({"slug": "geometry", "title": "Geometry Notes"}),
        b"",
    );
    let path = store.abs("records/task/0198f7a9-0000-7cd2-9e3f-000000000001.md");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, bytes).unwrap();

    // 物化索引 + 检查点（证明装配，同 index_paths 测试配方）。
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
    std::fs::write(
        mem_core::freshness::checkpoint_path(&store),
        serde_json::to_vec(&checkpoint).unwrap(),
    )
    .unwrap();
    let hello = mem_core::sidecar::Hello {
        version: 1,
        backend: "read-compatible-test".into(),
        undigested: Some(0),
        max_seq: 0,
        last_rebuild: checkpoint.ts,
    };


    // 基线：能力表含 F、磁盘尚未登记——indexed 正常应答。
    let proved = mem_index::open_proved(&store, Some(&hello));
    let idx = proved.index.as_ref().expect("登记前索引可证明");
    let baseline = execute(FindParams {
        prefilter: Some(idx as &dyn mem_core::find::FindPrefilter),
        mode: ReadMode::Indexed { via_subscription: "read-compatible-test".into() },
        store: &store, types: &types, query: Some("geometry"), filters: &[],
        type_narrow: None, view: None, limit: 50, offset: 0,
    }).unwrap();
    assert!(
        matches!(baseline.mode, ReadMode::Indexed { .. }),
        "基线应走 indexed：{:?}",
        baseline.mode
    );
    assert_eq!(baseline.hits.len(), 1);

    // 另一进程登记只读兼容特性（受控能力表中声明的 scan.future/v1）。
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(store.abs("mem.json")).unwrap()).unwrap();
    config["required_features"] = serde_json::json!(["scan.future/v1"]);
    std::fs::write(
        store.abs("mem.json"),
        serde_json::to_vec_pretty(&config).unwrap(),
    )
    .unwrap();

    // 长活句柄在登记后查询：返回边界复核发现只读兼容缺项 → 不得返回 indexed
    // 结果，整趟降级真相层直读，命中与直读等价。
    let outcome = execute(FindParams {
        prefilter: Some(idx as &dyn mem_core::find::FindPrefilter),
        mode: ReadMode::Indexed { via_subscription: "read-compatible-test".into() },
        store: &store, types: &types, query: Some("geometry"), filters: &[],
        type_narrow: None, view: None, limit: 50, offset: 0,
    }).unwrap();
    match &outcome.mode {
        ReadMode::Direct { reason } => {
            assert!(
                reason.contains("scan.future/v1"),
                "降级原因须含只读兼容缺项标识：{reason}"
            );
        }
        other => panic!("只读兼容缺项出现时不得返回 indexed 结果：{other:?}"),
    }
    assert_eq!(outcome.hits.len(), 1, "降级直读仍须命中种子记录");
    let direct = execute(FindParams {
        prefilter: None,
        mode: ReadMode::Direct { reason: "测试直读".into() },
        store: &store, types: &types, query: Some("geometry"), filters: &[],
        type_narrow: None, view: None, limit: 50, offset: 0,
    }).unwrap();
    let direct_ids: Vec<&str> = direct.hits.iter().map(|h| h.id.as_str()).collect();
    let downgraded_ids: Vec<&str> = outcome.hits.iter().map(|h| h.id.as_str()).collect();
    assert_eq!(downgraded_ids, direct_ids, "降级结果与纯直读逐 ID 等价");

    // 不兼容缺项的既有语义不受影响：再登记一个未声明只读兼容的特性 → 结构化拒绝。
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(store.abs("mem.json")).unwrap()).unwrap();
    config["required_features"] =
        serde_json::json!(["scan.future/v1", "record.future/v1"]);
    std::fs::write(
        store.abs("mem.json"),
        serde_json::to_vec_pretty(&config).unwrap(),
    )
    .unwrap();
    let error = execute(FindParams {
        prefilter: Some(idx as &dyn mem_core::find::FindPrefilter),
        mode: ReadMode::Indexed { via_subscription: "read-compatible-test".into() },
        store: &store, types: &types, query: Some("geometry"), filters: &[],
        type_narrow: None, view: None, limit: 50, offset: 0,
    }).unwrap_err();
    assert!(
        matches!(error, MemError::MissingFeatures(ref f) if f.contains(&"record.future/v1".to_string())),
        "未声明只读兼容的缺项仍须结构化拒绝：{error:?}"
    );
}
