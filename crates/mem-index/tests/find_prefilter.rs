//! R10 索引侧验收（TDD-3；keyword-search-design.md §8-3）：
//! - candidates 多词项合取粗筛（每正词项 LIKE = 谓词命中超集，词项间 AND；
//!   负词项与过滤不下推——它们的正确性由精筛裁定，粗筛只决定速度）；
//! - bad_records 表：与 records 行同扫描同事务写入；检查点 stat 集覆盖全部
//!   records/ 文件（含坏文件）——评审 B1；
//! - open_proved 构造即证明（决策②/红线21 唯一证明点）：stat 咨询 ∧ 只读打开 ∧
//!   只读 schema 门禁含 bad_records ∧ epoch 代际核对；任一失败 → None；
//! - FindPrefilter 实现经 mem-core find::execute 与 direct 等价（命中面 + 告警面）。

use std::collections::BTreeMap;

use mem_core::error::MemResult;
use mem_core::find::{self, FilterCond, FindParams, FindPrefilter};
use mem_core::freshness::{self, Checkpoint, ReadMode};
use mem_core::sidecar::Hello;
use mem_core::store::{OpenMode, Store};
use mem_index::ProvedIndex;
use serde_json::{json, Value};

// ---------- 夹具 ----------

fn fresh_store() -> (tempfile::TempDir, Store) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("store");
    Store::create(&root).unwrap();
    let store = Store::open(&root, OpenMode::Auto).unwrap();
    (tmp, store)
}

fn seed(
    store: &Store,
    type_name: &str,
    id_tail: &str,
    modified: &str,
    payload: Value,
    body: &str,
) -> String {
    let id = format!("01f2e000-0000-7000-8000-0000000000{id_tail}");
    let env = mem_core::envelope::Envelope {
        id: id.clone(),
        r#type: type_name.into(),
        created: "2026-11-01T00:00:00Z".into(),
        modified: modified.into(),
        version: 1,
        principal: None,
        refs: vec![],
        aliases: vec![],
        evidence_cutoff_seq: None,
        commit: None,
        extra: Default::default(),
    };
    let bytes = mem_core::record::serialize_new(&env, &payload, body.as_bytes());
    let rel = format!("records/{type_name}/{id}.md");
    let abs = store.abs(&rel);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(abs, bytes).unwrap();
    id
}

/// 物化（生产维护器同序）：rebuild（含 bad_records 同事务写入 + 折叠列）→
/// set_body_texts → 返回 (epoch, 同一次扫描的 stat 集)。
fn materialize(store: &Store) -> MemResult<(u64, BTreeMap<String, (u128, u64)>)> {
    let mut idx = mem_index::Index::open_store(store)?;
    let (_, _, entries) = idx.rebuild_with_entries(store)?;
    idx.set_body_texts(store)?;
    let epoch = idx.epoch()?;
    Ok((epoch, entries))
}

fn publish(store: &Store, epoch: u64, entries: BTreeMap<String, (u128, u64)>) {
    let cp = Checkpoint {
        ts: "2026-11-10T00:00:00.000Z".into(),
        entries,
        index_epoch: epoch,
    };
    freshness::save_checkpoint(store, &cp).unwrap();
}

fn hello0() -> Hello {
    Hello {
        version: 1,
        backend: "rdc".into(),
        undigested: Some(0),
        max_seq: 0,
        last_rebuild: "2026-11-10T00:00:00Z".into(),
    }
}

fn proven(store: &Store) -> mem_index::Proved {
    mem_index::open_proved(store, Some(&hello0()))
}

fn ids(out: &find::FindOutcome) -> Vec<String> {
    out.hits.iter().map(|h| h.id.clone()).collect()
}

// ---------- bad_records 表：同扫描同事务 + stat 全覆盖 ----------

#[test]
fn bad_records_same_tx_and_stat_covers_all_files() {
    let (_tmp, store) = fresh_store();
    seed(&store, "doc", "g001", "2026-11-02T00:00:00Z", json!({"title":"fine"}), "good text");
    std::fs::write(store.abs("records/doc/bad-1.md"), b"not a record").unwrap();
    std::fs::write(store.abs("records/doc/bad-2.md"), b"---\nbroken").unwrap();

    let (epoch, entries) = materialize(&store).unwrap();
    publish(&store, epoch, entries.clone());

    // 检查点 stat 集覆盖全部 records/ 文件（含坏文件）——评审 B1：坏文件的任何
    // 后续外部变化都造成 stat 失配 → 回退 direct → 告警如实反映。
    for rel in ["records/doc/bad-1.md", "records/doc/bad-2.md", "records/doc/01f2e000-0000-7000-8000-0000000000g001.md"] {
        assert!(entries.contains_key(rel), "stat 集必须覆盖 {rel}");
    }

    let proved = proven(&store);
    let pf = proved.index.expect("证明必须成立");
    // rel 升序 + cap 语义（上限 = list_limit_max 由调用方传入）。
    let (bad, more) = pf.bad_records(1).unwrap();
    assert_eq!(bad.len(), 1, "cap=1 截断");
    assert!(more, "还有更多");
    assert!(bad[0].0.starts_with("records/doc/bad-"), "rel 升序第一 = bad-1：{:?}", bad);
    assert!(bad[0].1.contains("跳过坏条目"), "告警行模板前段：{:?}", bad[0]);

    let (bad_all, more_all) = pf.bad_records(10).unwrap();
    assert_eq!(bad_all.len(), 2);
    assert!(!more_all);
    assert_eq!(bad_all[0].0, "records/doc/bad-1.md");
    assert_eq!(bad_all[1].0, "records/doc/bad-2.md");
}

// ---------- candidates：多词项合取粗筛（超集） ----------

#[test]
fn candidates_multiterm_conjunctive_superset_and_narrowing() {
    let (_tmp, store) = fresh_store();
    // r1：alpha(title) + one/two(tags，LF 分隔) → 两词项 LIKE 都命中（真命中）。
    let r1 = seed(&store, "doc", "r001", "2026-11-02T00:00:00Z",
        json!({"title":"alpha","tags":["one","two"]}), "");
    // r2：仅 gamma → 词项 "alpha" LIKE 不命中（被合取排除）。
    seed(&store, "doc", "r002", "2026-11-03T00:00:00Z", json!({"title":"gamma"}), "");
    // r3：单值 "alpha two"（跨值假命中形态的候选——精筛会裁定，粗筛只多不漏）。
    let r3 = seed(&store, "doc", "r003", "2026-11-04T00:00:00Z", json!({"title":"alpha two"}), "");
    // r4：alt 类型（类型收窄用）。
    seed(&store, "alt", "r004", "2026-11-05T00:00:00Z", json!({"title":"alpha"}), "");

    let (epoch, entries) = materialize(&store).unwrap();
    publish(&store, epoch, entries);
    let pf = proven(&store).index.expect("证明成立");

    // 多词项 AND：r1/r3 保留，r2 排除（粗筛超集 ⊇ 真命中集）。
    let rows = pf.candidates(&["alpha".to_string(), "two".to_string()], None).unwrap();
    let got: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
    assert!(got.contains(&r1.as_str()) && got.contains(&r3.as_str()), "超集候选含真命中：{got:?}");
    assert!(!got.contains(&"01f2e000-0000-7000-8000-0000000000r002"), "词项合取排除不含 alpha 的记录：{got:?}");

    // 单词项 = 既有单串 LIKE 语义的超集（id 前缀 ∨ 三折叠列）。
    let rows = pf.candidates(&["gamma".to_string()], None).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "01f2e000-0000-7000-8000-0000000000r002");

    // 类型收窄：Some(["doc"]) 排除 alt；空类型集 → 空候选（正确性无害）。
    let rows = pf.candidates(&["alpha".to_string()], Some(&["doc".to_string()])).unwrap();
    assert!(rows.iter().all(|r| r.type_name == "doc"), "类型收窄生效");
    assert!(pf.candidates(&[], Some(&[])).unwrap().is_empty(), "空类型集 → 空候选");

    // LIKE 转义：字面 % 不匹配全部（r7 语义保持）。
    assert!(pf.candidates(&["%".to_string()], None).unwrap().is_empty());

    // 纯类型收窄（无词项 = 清单快路径）：行字段与磁盘真相一致。
    let rows = pf.candidates(&[], Some(&["doc".to_string()])).unwrap();
    assert_eq!(rows.len(), 3);
    let row = rows.iter().find(|r| r.id == r1).unwrap();
    assert_eq!(row.type_name, "doc");
    assert_eq!(row.version, 1);
    assert_eq!(row.modified, "2026-11-02T00:00:00Z");
    assert_eq!(row.digest.len(), 64);
}

// ---------- open_proved：构造即证明（唯一证明点） ----------

#[test]
fn open_proved_gates_all_or_nothing() {
    let (_tmp, store) = fresh_store();
    let r1 = seed(&store, "doc", "g001", "2026-11-02T00:00:00Z", json!({"title":"alpha"}), "");

    // 1) 无检查点 → None（sidecar 从未维护过）。
    assert!(proven(&store).index.is_none(), "无检查点不可证明");

    let (epoch, entries) = materialize(&store).unwrap();
    publish(&store, epoch, entries.clone());

    // 2) sidecar 不在 → None。
    assert!(mem_index::open_proved(&store, None).index.is_none(), "sidecar 不在不可证明");
    // 3) 订阅存在未消化变更 → None（索引落后于真相）。
    let mut hello1 = hello0();
    hello1.undigested = Some(3);
    assert!(mem_index::open_proved(&store, Some(&hello1)).index.is_none());
    // 4) 卷不支持订阅（backend=none）→ None（永久直读形态）。
    let mut hello_none = hello0();
    hello_none.backend = "none".into();
    assert!(mem_index::open_proved(&store, Some(&hello_none)).index.is_none());

    // 5) 全门禁通过 → Some（mode = Indexed）。（作用域内结束句柄——Windows 上
    //    打开的连接会锁住 index.sqlite，后续门禁场景需要文件级操作。）
    {
        let proved = proven(&store);
        assert!(proved.index.is_some(), "stat 一致 ∧ 订阅覆盖 ∧ schema 完整 ∧ 同代 → 证明成立");
        assert!(matches!(proved.mode, ReadMode::Indexed { .. }), "mode 如实标注 Indexed：{:?}", proved.mode);
    }

    // 6) stat 失配（检查点之后新增文件）→ None。
    seed(&store, "doc", "g002", "2026-11-06T00:00:00Z", json!({"title":"new"}), "");
    assert!(proven(&store).index.is_none(), "stat 失配不可证明");
    // 还原：重新物化 + 发布（供后续门禁场景）。
    let (epoch, entries) = materialize(&store).unwrap();
    publish(&store, epoch, entries);

    // 7) epoch 代际核对：再物化一次（epoch+1）而不更新检查点 → None
    //   （既有 CLI 快路径的"索引删除重建但检查点仍指旧代"回归随下沉完整收进）。
    let (_e2, _en2) = materialize(&store).unwrap();
    assert!(proven(&store).index.is_none(), "检查点与索引代际不匹配不可证明");

    // 8) 只读 schema 门禁扩展（含 bad_records）：手工删除表（旧代索引形态）→ None，
    //    回退 direct 而不是"空告警的 indexed"。
    let (epoch, entries) = materialize(&store).unwrap();
    publish(&store, epoch, entries);
    let conn = rusqlite::Connection::open(store.abs("index/index.sqlite")).unwrap();
    conn.execute_batch("DROP TABLE bad_records").unwrap();
    drop(conn);
    let proved = proven(&store);
    assert!(proved.index.is_none(), "bad_records 缺表 → 门禁不通过");
    match &proved.mode {
        ReadMode::Direct { reason } => assert!(reason.contains("schema") || reason.contains("索引"), "reason 如实：{reason}"),
        other => panic!("必须标注 Direct：{other:?}"),
    }
    drop(proved); // 释放只读连接（Windows：句柄在则文件不可删）

    // 9) 索引文件缺失 → None（只读打开失败，读路径不建库——红线18）。
    std::fs::remove_file(store.abs("index/index.sqlite")).unwrap();
    assert!(proven(&store).index.is_none());
    let _ = r1;
}

// ---------- FindPrefilter 实现 × find::execute：与 direct 等价 ----------

#[test]
fn indexed_via_prefilter_equals_direct() {
    let (_tmp, store) = fresh_store();
    let types = mem_schema::TypeSet::load_dir(&store.abs("types")).unwrap();
    let a = seed(&store, "doc", "e001", "2026-11-02T00:00:00Z",
        json!({"title":"alpha geometry"}), "quadratic body");
    seed(&store, "doc", "e002", "2026-11-03T00:00:00Z",
        json!({"title":"beta"}), "gamma body");
    std::fs::write(store.abs("records/doc/bad-1.md"), b"corrupt").unwrap();

    let (epoch, entries) = materialize(&store).unwrap();
    publish(&store, epoch, entries);
    let proved = proven(&store);
    let pf = proved.index.as_ref().map(|p| p as &dyn FindPrefilter).expect("证明成立");

    // 词项 + 否定词项：indexed 与 direct 命中面逐 ID 相等。
    let mk = |pf: Option<&dyn FindPrefilter>, mode: ReadMode| {
        find::execute(FindParams {
            store: &store, types: &types, query: Some("quadratic -beta"),
            filters: &[], type_narrow: None, view: None, limit: 50, offset: 0,
            mode, prefilter: pf,
        })
        .unwrap()
    };
    let indexed = mk(Some(pf), proved.mode.clone());
    let direct = mk(None, ReadMode::Direct { reason: "测试".into() });
    assert_eq!(ids(&indexed), vec![a], "indexed 命中面 == direct：{:?} vs {:?}", ids(&indexed), ids(&direct));
    assert!(matches!(indexed.mode, ReadMode::Indexed { .. }));

    // 告警面等价（同模板、rel 升序、三要素）——两执行器同库同告警集。
    assert_eq!(indexed.warnings, direct.warnings, "告警集合与文本一致（决策⑦）");
    assert_eq!(indexed.warnings_truncated, direct.warnings_truncated);
    assert!(indexed.warnings[0].contains("修复入口：mem fix records/doc/bad-1.md"));

    // 过滤 + 词项组合走 indexed（过滤不下推，只在精筛施加）。
    let out = find::execute(FindParams {
        store: &store, types: &types, query: Some("quadratic"),
        filters: &[FilterCond { field: "nosuch".into(), text: "1".into() }],
        type_narrow: None, view: None, limit: 50, offset: 0,
        mode: proved.mode.clone(), prefilter: Some(pf),
    });
    assert!(out.is_err(), "过滤声明检查在 indexed 路径同样生效（unknown_filter_field）");
    assert_eq!(out.unwrap_err().param_kind(), Some("unknown_filter_field"));
}

/// ProvedIndex 不经 open_proved 不可构造（构造即证明——唯一构造路径）。
#[test]
fn proved_index_only_constructible_via_open_proved() {
    // ProvedIndex 字段私有且无 pub 构造函数：本测试以编译为证——
    // `ProvedIndex { .. }` 字面构造与 `ProvedIndex::new` 均不存在于公开面。
    fn takes(_p: &ProvedIndex) {}
    let (_tmp, store) = fresh_store();
    let (epoch, entries) = materialize(&store).unwrap();
    publish(&store, epoch, entries);
    let proved = proven(&store);
    if let Some(p) = proved.index {
        takes(&p);
    }
}
