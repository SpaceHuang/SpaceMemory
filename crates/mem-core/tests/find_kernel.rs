//! R10 检索内核验收（TDD-1；keyword-search-design.md §8-1）。
//!
//! 覆盖（验收锚点 §4.1 行 1/5/6/7/8 的 direct 侧）：
//! - parse_query 文法（决策①）：Unicode 空白切分、`-` 否定标记、空查询/纯否定
//!   显式拒收（R10.9）、折叠单源（R10.4）；
//! - 组合谓词：多词项与 + 与位排除；单词项与既有单串检索逐记录等价（R10.8）；
//! - 正文域 UTF-8 参与前提收口（§2.3/R11.4 检索侧先行）；
//! - 过滤执行器与声明校验（决策③④）：4 枚机判 kind（§2.7）、折叠/数值/布尔/
//!   数组等值、跨类型同名字段异形合并解释（A∪B）、候选类型集收窄子集、
//!   未声明类型静默排除；
//! - direct 执行器全量面：命中序分页（红线9）、坏记录告警三要素 + rel 升序截断
//!  （决策⑦）、indexed 调用失败回退 direct（红线8：不转空命中/空告警）。

use mem_core::error::MemError;
use mem_core::find::{self, FilterCond, FindParams, FindPrefilter, PrefilterRow};
use mem_core::freshness::ReadMode;
use mem_core::store::{OpenMode, Store};
use mem_schema::TypeSet;
use serde_json::{json, Value};

// ---------- 夹具 ----------

/// x-filter 声明类型 doc：覆盖 v1 全部支持形态 + 两类不支持形态（object /
/// 纯 enum——形态不从 enum 推导，评审 L2）。
const DOC_SCHEMA: &str = r#"{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "title": "doc",
  "type": "object",
  "x-grade": "lenient",
  "properties": {
    "title": {"type": "string"},
    "status": {"type": "string", "x-filter": true},
    "score": {"type": "number", "x-filter": true},
    "flag": {"type": "boolean", "x-filter": true},
    "tags": {"type": "array", "items": {"type": "string"}, "x-filter": true},
    "mixed": {"type": "array", "x-filter": true},
    "obj": {"type": "object", "x-filter": true},
    "form": {"enum": ["a", "b"], "x-filter": true}
  }
}"#;

/// 跨类型同名字段异形反例（评审 B2/B4）：alt.status 为 number，doc.status 为 string。
const ALT_SCHEMA: &str = r#"{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "title": "alt",
  "type": "object",
  "x-grade": "lenient",
  "properties": {
    "label": {"type": "string"},
    "status": {"type": "number", "x-filter": true}
  }
}"#;

struct Fixture {
    _tmp: tempfile::TempDir,
    store: Store,
    types: TypeSet,
}

fn copy_builtin_types(root: &std::path::Path) {
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
}

fn fresh_store() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("store");
    Store::create(&root).unwrap();
    copy_builtin_types(&root);
    std::fs::create_dir_all(root.join("types/doc")).unwrap();
    std::fs::write(root.join("types/doc/schema.json"), DOC_SCHEMA).unwrap();
    std::fs::create_dir_all(root.join("types/alt")).unwrap();
    std::fs::write(root.join("types/alt/schema.json"), ALT_SCHEMA).unwrap();
    let store = Store::open(&root, OpenMode::Auto).unwrap();
    let types = TypeSet::load_dir(&root.join("types")).unwrap();
    Fixture { _tmp: tmp, store, types }
}

/// mem.json budgets 写入（须在 Store::open 之前——Store 缓存配置）。
fn write_budget(root: &std::path::Path, key: &str, value: Value) {
    let mem_json = root.join("mem.json");
    let mut cfg: Value = serde_json::from_slice(&std::fs::read(&mem_json).unwrap()).unwrap();
    cfg["budgets"][key] = value;
    std::fs::write(&mem_json, serde_json::to_vec_pretty(&cfg).unwrap()).unwrap();
}

/// 手写落盘一条记录（固定 id/modified，排序确定；绕过写路径——检索内核测试
/// 不依赖写协议）。
fn seed(
    store: &Store,
    type_name: &str,
    id_tail: &str,
    modified: &str,
    payload: Value,
    body: &[u8],
    aliases: &[&str],
) -> String {
    let id = format!("01f1d000-0000-7000-8000-0000000000{id_tail}");
    let env = mem_core::envelope::Envelope {
        id: id.clone(),
        r#type: type_name.into(),
        created: "2026-11-01T00:00:00Z".into(),
        modified: modified.into(),
        version: 1,
        principal: None,
        refs: vec![],
        aliases: aliases.iter().map(|s| s.to_string()).collect(),
        evidence_cutoff_seq: None,
        commit: None,
        extra: Default::default(),
    };
    let bytes = mem_core::record::serialize_new(&env, &payload, body);
    let rel = format!("records/{type_name}/{id}.md");
    let abs = store.abs(&rel);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(abs, bytes).unwrap();
    id
}

/// 标准记录库。命中序（modified DESC）：
/// memory1(11-05) > alt1(11-04) > doc3(11-03T12) > doc1(11-03) > doc2(11-02) > alt2(11-01)。
fn seed_library(store: &Store) {
    seed(store, "memory", "m001", "2026-11-05T00:00:00Z",
        json!({"title":"other","status":"Geometry"}), b"", &[]);
    seed(store, "alt", "a001", "2026-11-04T00:00:00Z",
        json!({"label":"x","status":1}), b"", &[]);
    seed(store, "doc", "d003", "2026-11-03T12:00:00Z",
        json!({"title":"one","status":"1"}), b"", &[]);
    seed(store, "doc", "d001", "2026-11-03T00:00:00Z",
        json!({"title":"Geometry","status":"Geometry","score":1,"flag":true,"tags":["alpha","beta"]}),
        b"the quadratic formula", &[]);
    seed(store, "doc", "d002", "2026-11-02T00:00:00Z",
        json!({"title":"plain","status":"Active","score":1.0,"flag":false,"tags":["gamma"],"mixed":[1,"two"]}),
        b"linha reta", &[]);
    seed(store, "alt", "a002", "2026-11-01T00:00:00Z",
        json!({"label":"y","status":2}), b"", &[]);
}

fn conds(pairs: &[(&str, &str)]) -> Vec<FilterCond> {
    pairs
        .iter()
        .map(|(k, v)| FilterCond { field: k.to_string(), text: v.to_string() })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn run(
    f: &Fixture,
    query: Option<&str>,
    filters: &[FilterCond],
    type_narrow: Option<&str>,
    limit: usize,
    offset: usize,
) -> mem_core::find::FindOutcome {
    find::execute(FindParams {
        store: &f.store,
        types: &f.types,
        query,
        filters: &filters,
        type_narrow,
        view: None,
        limit,
        offset,
        mode: ReadMode::Direct { reason: "测试直读".into() },
        prefilter: None,
    })
    .unwrap()
}

fn ids(out: &mem_core::find::FindOutcome) -> Vec<String> {
    out.hits.iter().map(|h| h.id.clone()).collect()
}

fn kind_of(e: &MemError) -> String {
    e.param_kind().expect("必须是带 kind 的显式拒收").to_string()
}

fn run_err(
    f: &Fixture,
    query: Option<&str>,
    filters: &[FilterCond],
    type_narrow: Option<&str>,
) -> MemError {
    find::execute(FindParams {
        store: &f.store,
        types: &f.types,
        query,
        filters: &filters,
        type_narrow,
        view: None,
        limit: 50,
        offset: 0,
        mode: ReadMode::Direct { reason: "测试直读".into() },
        prefilter: None,
    })
    .unwrap_err()
}

// ---------- 决策①：parse_query 文法 ----------

#[test]
fn parse_query_grammar_folding_and_rejection() {
    // Unicode 空白切分（含 U+3000 全角空格）+ 否定标记 + 折叠一次。
    let pq = find::parse_query(" alpha\tBeta\u{3000}-ÄRGER ").unwrap();
    let pos_raw: Vec<&str> = pq.positive.iter().map(|t| t.raw.as_str()).collect();
    let pos_fold: Vec<&str> = pq.positive.iter().map(|t| t.folded.as_str()).collect();
    let neg_fold: Vec<&str> = pq.negative.iter().map(|t| t.folded.as_str()).collect();
    assert_eq!(pos_raw, vec!["alpha", "Beta"], "原始词项保留（响应回显用）");
    assert_eq!(pos_fold, vec!["alpha", "beta"], "词项折叠一次（匹配用）");
    assert_eq!(neg_fold, vec!["ärger"], "否定匹配串 = 去标记后折叠（NFC+小写）");

    // "-" 恰为字面肯定词项；"--x" = 否定词项，匹配串 "-x"。
    let pq = find::parse_query("- --x").unwrap();
    assert_eq!(pq.positive[0].raw, "-");
    assert_eq!(pq.negative[0].folded, "-x");

    // 空查询 / 纯否定 → query_invalid（R10.9：绝不表现为空命中清单）。
    for q in ["", "   \t\u{3000} ", "-a -b"] {
        let e = find::parse_query(q).unwrap_err();
        assert_eq!(kind_of(&e), "query_invalid", "查询 {q:?} 必须 query_invalid");
    }
}

// ---------- §2.3：组合谓词 + 正文 UTF-8 收口 ----------

#[test]
fn combined_predicate_terms_and_negation() {
    let f = fresh_store();
    seed_library(&f.store);
    let d001 = "01f1d000-0000-7000-8000-0000000000d001";
    let m001 = "01f1d000-0000-7000-8000-0000000000m001";

    // 单词项正文域命中。
    assert_eq!(ids(&run(&f, Some("quadratic"), &[], None, 50, 0)), vec![d001]);
    // 多词项与（验收 6 的谓词层）：同记录含两词 → 命中。
    assert_eq!(ids(&run(&f, Some("quadratic formula"), &[], None, 50, 0)), vec![d001]);
    // 与位排除（验收 7 的谓词层）：含 A 不含 B——doc1 有 formula 被排除，
    // memory1 含 geometry 无 formula → 命中。
    assert_eq!(ids(&run(&f, Some("geometry -formula"), &[], None, 50, 0)), vec![m001]);

    // R10.8：仅一个肯定词项且无否定 → 与既有单串检索逐记录等价。
    let q_fold = mem_core::paths::fold("geometry");
    let expected = ids(&run(&f, Some("geometry"), &[], None, 50, 0));
    for rec in mem_core::ops::scan_all(&f.store).unwrap().records {
        let env = &rec.record.envelope;
        let single = mem_core::search::record_matches(
            &env.id, &env.aliases, &rec.record.payload, &rec.record.body, &q_fold,
        );
        assert_eq!(single, expected.contains(&env.id), "组合谓词与单串检索逐记录等价（R10.8）：{}", env.id);
    }
}

#[test]
fn body_domain_strict_utf8() {
    let f = fresh_store();
    // 合法 UTF-8 正文记录（对照）。
    let good = seed(&f.store, "doc", "dggd", "2026-11-06T00:00:00Z",
        json!({"title":"nohit"}), b"say hello", &[]);
    // 无效 UTF-8 正文：lossy 解码形式含 "hello"（旧实现伪命中形态），严格判定必须退出正文域。
    let bad = seed(&f.store, "doc", "dbad", "2026-11-06T00:00:00Z",
        json!({"title":"nohit"}), &[0xff, b'h', b'e', b'l', b'l', b'o'], &[]);

    let out = run(&f, Some("hello"), &[], None, 50, 0);
    let got = ids(&out);
    assert!(got.contains(&good), "合法 UTF-8 正文照常命中：{got:?}");
    assert!(!got.contains(&bad), "非合法 UTF-8 正文退出匹配（§2.3 收口）：{got:?}");

    // 验收措辞：命中集合与"移除该记录的等价库"一致——坏正文记录对命中集零贡献。
    let f2 = fresh_store();
    let good2 = seed(&f2.store, "doc", "dggd", "2026-11-06T00:00:00Z",
        json!({"title":"nohit"}), b"say hello", &[]);
    let a = ids(&run(&f, Some("hello"), &[], None, 50, 0));
    let b = ids(&run(&f2, Some("hello"), &[], None, 50, 0));
    assert_eq!(a, b, "坏正文记录的存在不得改变命中集合（R11 验收 5(i) 检索侧）");
    assert_eq!(a, vec![good2]);
}

// ---------- 决策③④：过滤 kind 与等值语义 ----------

#[test]
fn filter_rejection_kinds() {
    let f = fresh_store();
    seed_library(&f.store);

    // 候选集中无任何类型声明 → unknown_filter_field（防手滑静默空结果）。
    let e = run_err(&f, None, &conds(&[("nosuch", "1")]), None);
    assert_eq!(kind_of(&e), "unknown_filter_field");
    // 同一字段重复 → duplicate_filter_field。
    let e = run_err(&f, None, &conds(&[("status", "a"), ("status", "b")]), None);
    assert_eq!(kind_of(&e), "duplicate_filter_field");
    // 空文本 → invalid_filter_value。
    let e = run_err(&f, None, &conds(&[("status", "")]), None);
    assert_eq!(kind_of(&e), "invalid_filter_value");
    // 兼容类型集为空（score 仅 number 声明，文本非数值）→ invalid_filter_value。
    let e = run_err(&f, None, &conds(&[("score", "abc")]), None);
    assert_eq!(kind_of(&e), "invalid_filter_value");
    // 形态不支持（object 声明 / 纯 enum 声明）→ invalid_filter_value。
    let e = run_err(&f, None, &conds(&[("obj", "x")]), None);
    assert_eq!(kind_of(&e), "invalid_filter_value");
    let e = run_err(&f, None, &conds(&[("form", "a")]), None);
    assert_eq!(kind_of(&e), "invalid_filter_value");
}

#[test]
fn filter_equality_semantics() {
    let f = fresh_store();
    seed_library(&f.store);
    let d001 = "01f1d000-0000-7000-8000-0000000000d001";
    let d002 = "01f1d000-0000-7000-8000-0000000000d002";

    // string：折叠等值（非子串）——"Geometry" 与 "geometry" 互可检出；
    // memory1 的 status 字段存在但类型未声明 x-filter → 静默排除（同一条语义）。
    assert_eq!(ids(&run(&f, None, &conds(&[("status", "geometry")]), None, 50, 0)), vec![d001]);
    assert_eq!(ids(&run(&f, None, &conds(&[("status", "GEOMETRY")]), None, 50, 0)), vec![d001]);
    // 等值不是子串："eometr" 不等于任何声明值。
    assert!(ids(&run(&f, None, &conds(&[("status", "eometr")]), None, 50, 0)).is_empty());

    // number：JSON 数值文法解析后数值等值——"1" 与 "1.0" 同值。
    assert_eq!(ids(&run(&f, None, &conds(&[("score", "1")]), None, 50, 0)), vec![d001, d002]);
    assert_eq!(ids(&run(&f, None, &conds(&[("score", "1.0")]), None, 50, 0)), vec![d001, d002]);
    assert!(ids(&run(&f, None, &conds(&[("score", "1.5")]), None, 50, 0)).is_empty());
    // 严格 JSON 数值文法："01" 非法 → 兼容集为空 → 显式错误。
    let e = run_err(&f, None, &conds(&[("score", "01")]), None);
    assert_eq!(kind_of(&e), "invalid_filter_value");

    // boolean：仅认字面 true/false（大小写敏感）；不可解释文本使兼容类型集为空
    // → invalid_filter_value（§2.4：兼容集为空显式错误，不是静默空命中）。
    assert_eq!(ids(&run(&f, None, &conds(&[("flag", "true")]), None, 50, 0)), vec![d001]);
    let e = run_err(&f, None, &conds(&[("flag", "True")]), None);
    assert_eq!(kind_of(&e), "invalid_filter_value");
    let e = run_err(&f, None, &conds(&[("flag", "1")]), None);
    assert_eq!(kind_of(&e), "invalid_filter_value");

    // 数组：任一元素按其实际形态等值即命中。
    assert_eq!(ids(&run(&f, None, &conds(&[("tags", "beta")]), None, 50, 0)), vec![d001]);
    assert_eq!(ids(&run(&f, None, &conds(&[("tags", "gamma")]), None, 50, 0)), vec![d002]);
    // mixed = [1, "two"]：数字元素按数值等值、字符串元素按折叠等值。
    assert_eq!(ids(&run(&f, None, &conds(&[("mixed", "1")]), None, 50, 0)), vec![d002]);
    assert_eq!(ids(&run(&f, None, &conds(&[("mixed", "two")]), None, 50, 0)), vec![d002]);
    assert!(ids(&run(&f, None, &conds(&[("mixed", "3")]), None, 50, 0)).is_empty());
}

#[test]
fn filter_cross_type_same_name_union_and_narrowing() {
    let f = fresh_store();
    seed_library(&f.store);
    let a001 = "01f1d000-0000-7000-8000-0000000000a001";
    let d003 = "01f1d000-0000-7000-8000-0000000000d003";

    // A/B 异形同名（doc.status string、alt.status number）：一次过滤 = 合并解释
    // （A∪B）——文本 "1" 经 string 折叠等值命中 doc3，经数值等值命中 alt1。
    assert_eq!(
        ids(&run(&f, None, &conds(&[("status", "1")]), None, 50, 0)),
        vec![a001, d003],
        "异形同名合并命中（A∪B），modified DESC 序"
    );
    // 子集表达两端同径入口：--type 收窄候选类型集 → 单形态子集。
    assert_eq!(ids(&run(&f, None, &conds(&[("status", "1")]), Some("alt"), 50, 0)), vec![a001]);
    assert_eq!(ids(&run(&f, None, &conds(&[("status", "1")]), Some("doc"), 50, 0)), vec![d003]);
    // 候选类型集内无声明 → unknown_filter_field（memory 类型未声明 status 可过滤）。
    let e = run_err(&f, None, &conds(&[("status", "1")]), Some("memory"));
    assert_eq!(kind_of(&e), "unknown_filter_field");
}

#[test]
fn filter_combined_with_terms() {
    let f = fresh_store();
    seed_library(&f.store);
    let d001 = "01f1d000-0000-7000-8000-0000000000d001";

    // 词项 ∧ 过滤同时生效（验收 8 组合语义）。
    assert_eq!(
        ids(&run(&f, Some("quadratic"), &conds(&[("status", "geometry")]), None, 50, 0)),
        vec![d001]
    );
    // 词项命中但过滤不中 → 零命中。
    assert!(ids(&run(&f, Some("linha"), &conds(&[("status", "geometry")]), None, 50, 0)).is_empty());
    // find 不收空查询（R10.9）：execute 层同样拒收。
    let e = run_err(&f, Some("   "), &conds(&[]), None);
    assert_eq!(kind_of(&e), "query_invalid");
}

// ---------- direct 执行器：分页 / 告警 / 回退 ----------

#[test]
fn direct_paging_by_hit_sequence_exhaustive() {
    let f = fresh_store();
    let mut made = Vec::new();
    for i in 0..5 {
        made.push(seed(&f.store, "doc", &format!("pg{i:02}"), &format!("2026-11-0{}T00:00:00Z", i + 1),
            json!({"title": format!("paging {i}")}), b"pager body", &[]));
    }
    // 命中序（modified DESC）= pg04 pg03 pg02 pg01 pg00。
    let expect_seq = [&made[4], &made[3], &made[2], &made[1], &made[0]];

    let o1 = run(&f, Some("pager"), &[], None, 2, 0);
    assert_eq!(ids(&o1), vec![expect_seq[0].clone(), expect_seq[1].clone()]);
    assert!(o1.truncated);
    assert_eq!(o1.resume_offset, Some(2));
    let o2 = run(&f, Some("pager"), &[], None, 2, 2);
    assert_eq!(ids(&o2), vec![expect_seq[2].clone(), expect_seq[3].clone()]);
    let o3 = run(&f, Some("pager"), &[], None, 2, 4);
    assert_eq!(ids(&o3), vec![expect_seq[4].clone()]);
    assert!(!o3.truncated);
    assert_eq!(o3.resume_offset, None);
    // 续读穷尽无重复（验收 2/4 direct 侧）。
    let mut seen = Vec::new();
    let mut off = 0usize;
    for _ in 0..10 {
        let o = run(&f, Some("pager"), &[], None, 2, off);
        seen.extend(ids(&o));
        match o.resume_offset {
            Some(n) => off = n,
            None => break,
        }
    }
    seen.sort();
    let mut all = expect_seq.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    all.sort();
    assert_eq!(seen, all, "续读穷尽且不重复");
}

#[test]
fn bad_record_warnings_template_sorted_capped() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("store");
    Store::create(&root).unwrap();
    copy_builtin_types(&root);
    std::fs::create_dir_all(root.join("types/doc")).unwrap();
    std::fs::write(root.join("types/doc/schema.json"), DOC_SCHEMA).unwrap();
    // 告警上限复用 list_limit_max（=2）：3 个坏文件 → 截断标注。
    write_budget(&root, "list_limit_max", json!(2));
    let store = Store::open(&root, OpenMode::Auto).unwrap();
    let types = TypeSet::load_dir(&root.join("types")).unwrap();
    let f = Fixture { _tmp: tmp, store, types };

    seed(&f.store, "doc", "ok01", "2026-11-02T00:00:00Z", json!({"title":"fine"}), b"good text", &[]);
    // rel 升序：records/doc/bad-1.md < bad-2.md < bad-3.md。
    for name in ["bad-1", "bad-2", "bad-3"] {
        std::fs::write(f.store.abs(format!("records/doc/{name}.md")), b"not a record").unwrap();
    }

    let out = run(&f, Some("good"), &[], None, 50, 0);
    // 坏记录不进命中（R10.3）且不崩。
    assert_eq!(out.hits.len(), 1, "只有正常记录命中：{:?}", ids(&out));
    // 告警三要素：位置（rel）+ 原因 + 修复入口；rel 升序截断至 cap=2。
    assert_eq!(out.warnings.len(), 2, "上限 2（list_limit_max）：{:?}", out.warnings);
    assert!(out.warnings_truncated, "超限必须截断标注");
    let rel1 = "records/doc/bad-1.md";
    let rel2 = "records/doc/bad-2.md";
    assert!(out.warnings[0].starts_with(&format!("跳过坏条目 {rel1}：")), "三要素·位置+原因：{}", out.warnings[0]);
    assert!(out.warnings[0].contains(&format!("修复入口：mem fix {rel1}")), "三要素·修复入口：{}", out.warnings[0]);
    assert!(out.warnings[1].starts_with(&format!("跳过坏条目 {rel2}：")), "rel 升序稳定排序：{}", out.warnings[1]);

    // 修好后恢复参与（R10.3 验收）。
    let good_bytes = mem_core::record::serialize_new(
        &mem_core::envelope::Envelope {
            id: "01f1d000-0000-7000-8000-0000000000fx1".into(),
            r#type: "doc".into(),
            created: "2026-11-02T00:00:00Z".into(),
            modified: "2026-11-02T00:00:00Z".into(),
            version: 1,
            principal: None,
            refs: vec![],
            aliases: vec![],
            evidence_cutoff_seq: None,
            commit: None,
            extra: Default::default(),
        },
        &json!({"title":"recovered"}), b"good text",
    );
    std::fs::write(f.store.abs("records/doc/bad-1.md"), good_bytes).unwrap();
    let out2 = run(&f, Some("good"), &[], None, 50, 0);
    assert_eq!(out2.hits.len(), 2, "fix 后恢复参与检索：{:?}", ids(&out2));
    assert_eq!(out2.warnings.len(), 2, "仍有两个坏文件");
}

/// indexed 调用失败注入：candidates/bad_records 任一 Err → 放弃 indexed 应答、
/// 回退 direct 全量应答（mode=Direct + reason 携带真实错误）——不转空命中/空告警
/// （红线8 / 决策⑦）。
#[test]
fn prefilter_failure_falls_back_to_direct_full_answer() {
    struct FailingCands;
    struct FailingBad;
    impl FindPrefilter for FailingCands {
        fn candidates(&self, _t: &[String], _ty: Option<&[String]>) -> mem_core::error::MemResult<Vec<PrefilterRow>> {
            Err(MemError::Corrupt { path: "index".into(), reason: "注入失败：candidates".into() })
        }
        fn bad_records(&self, _cap: usize) -> mem_core::error::MemResult<(Vec<(String, String)>, bool)> {
            Ok((vec![], false))
        }
    }
    impl FindPrefilter for FailingBad {
        fn candidates(&self, _t: &[String], _ty: Option<&[String]>) -> mem_core::error::MemResult<Vec<PrefilterRow>> {
            Ok(vec![])
        }
        fn bad_records(&self, _cap: usize) -> mem_core::error::MemResult<(Vec<(String, String)>, bool)> {
            Err(MemError::Corrupt { path: "index".into(), reason: "注入失败：bad_records".into() })
        }
    }

    let f = fresh_store();
    seed_library(&f.store);
    let d001 = "01f1d000-0000-7000-8000-0000000000d001";
    let m001 = "01f1d000-0000-7000-8000-0000000000m001";
    for (pf, where_failed) in [
        (&FailingCands as &dyn FindPrefilter, "candidates"),
        (&FailingBad as &dyn FindPrefilter, "bad_records"),
    ] {
        let out = find::execute(FindParams {
            store: &f.store,
            types: &f.types,
            query: Some("geometry"),
            filters: &conds(&[]),
            type_narrow: None,
            view: None,
            limit: 50,
            offset: 0,
            mode: ReadMode::Indexed { via_subscription: "rdc".into() },
            prefilter: Some(pf),
        })
        .unwrap();
        // 回退 direct 全量应答：命中集合与 direct 一致（不是空命中）。
        assert_eq!(ids(&out), vec![m001, d001], "{where_failed} 失败必须回退 direct 全量应答");
        match &out.mode {
            ReadMode::Direct { reason } => assert!(reason.contains(where_failed), "reason 携带真实错误：{reason}"),
            other => panic!("必须标注 Direct：{other:?}"),
        }
    }
}

/// 过滤下钻至 indexed 的收窄语义（mock 粗筛）：收窄类型集 = 各条件兼容类型集交集，
/// 纯词项查询无类型收窄（types=None）。
#[test]
fn prefilter_narrowing_intersection_contract() {
    struct Recording {
        seen: std::sync::Mutex<Vec<Option<Vec<String>>>>,
    }
    impl FindPrefilter for Recording {
        fn candidates(&self, _t: &[String], types: Option<&[String]>) -> mem_core::error::MemResult<Vec<PrefilterRow>> {
            self.seen.lock().unwrap().push(types.map(|t| t.to_vec()));
            Ok(vec![])
        }
        fn bad_records(&self, _c: usize) -> mem_core::error::MemResult<(Vec<(String, String)>, bool)> {
            Ok((vec![], false))
        }
    }
    let f = fresh_store();
    seed_library(&f.store);
    let rec = std::sync::Arc::new(Recording { seen: std::sync::Mutex::new(vec![]) });

    let call = |filters: &[FilterCond]| {
        find::execute(FindParams {
            store: &f.store, types: &f.types, query: Some("x"), filters,
            type_narrow: None, view: None, limit: 50, offset: 0,
            mode: ReadMode::Indexed { via_subscription: "rdc".into() },
            prefilter: Some(rec.as_ref()),
        })
        .unwrap()
    };
    call(&conds(&[]));                                    // 纯词项：无类型收窄
    call(&conds(&[("status", "1")]));                     // doc string ∪ alt number 都可解释 "1"
    call(&conds(&[("score", "1")]));                      // score 仅 doc 声明

    let seen = rec.seen.lock().unwrap().clone();
    assert_eq!(seen[0], None, "纯词项查询无类型收窄");
    let mut second = seen[1].clone().unwrap();
    second.sort();
    assert_eq!(second, vec!["alt".to_string(), "doc".to_string()], "多条件收窄 = 各兼容类型集交集");
    assert_eq!(seen[2].clone().unwrap(), vec!["doc".to_string()], "单条件收窄 = 其兼容类型集");
}

/// 大数精度回归（实施评审 B1）：>2^53 的整数×浮点混合比较必须精确实数等值——
/// 绝不经 f64 化（|v| > 2^53 处相邻整数折叠为同一浮点，不同数值被误判相等）。
/// 标量与数组元素两面 + 正反两方向；同一实数的整数/浮点表示仍互认（决策④）。
#[test]
fn filter_number_precision_beyond_f64_safe_range() {
    let f = fresh_store();
    let big1 = seed(&f.store, "doc", "big1", "2026-11-06T00:00:00Z",
        json!({"title":"big","score":9007199254740993u64}), b"", &[]);          // 整数 2^53+1
    let big2 = seed(&f.store, "doc", "big2", "2026-11-06T00:00:00Z",
        json!({"title":"big","score":9007199254740992.0}), b"", &[]);           // 浮点 2^53
    let big3 = seed(&f.store, "doc", "big3", "2026-11-06T00:00:00Z",
        json!({"title":"big","mixed":[9007199254740993u64]}), b"", &[]);        // 数组元素·整数
    let big4 = seed(&f.store, "doc", "big4", "2026-11-06T00:00:00Z",
        json!({"title":"big","mixed":[9007199254740992.0]}), b"", &[]);         // 数组元素·浮点

    // B1 复现方向：过滤浮点字面 2^53.0 不得命中整数 2^53+1（评审复现为误命中）。
    assert_eq!(ids(&run(&f, None, &conds(&[("score", "9007199254740992.0")]), None, 50, 0)), vec![big2.clone()]);
    // 反方向：过滤整数 2^53+1 不得命中浮点 2^53 记录。
    assert_eq!(ids(&run(&f, None, &conds(&[("score", "9007199254740993")]), None, 50, 0)), vec![big1.clone()]);
    // 同一实数、不同 JSON 表示：整数文本 9007199254740992 ≍ 浮点 2^53.0（数值等值）。
    assert_eq!(ids(&run(&f, None, &conds(&[("score", "9007199254740992")]), None, 50, 0)), vec![big2.clone()]);
    // 数组元素两面同规则。
    assert_eq!(ids(&run(&f, None, &conds(&[("mixed", "9007199254740992.0")]), None, 50, 0)), vec![big4.clone()]);
    assert_eq!(ids(&run(&f, None, &conds(&[("mixed", "9007199254740993")]), None, 50, 0)), vec![big3.clone()]);
    assert_eq!(ids(&run(&f, None, &conds(&[("mixed", "9007199254740992")]), None, 50, 0)), vec![big4.clone()]);
    // 安全区间内行为不回退："1" ≍ "1.0"（既有决策④用例）。
    seed_library(&f.store);
    let d001 = "01f1d000-0000-7000-8000-0000000000d001";
    let d002 = "01f1d000-0000-7000-8000-0000000000d002";
    assert_eq!(ids(&run(&f, None, &conds(&[("score", "1")]), None, 50, 0)), vec![d001, d002]);
}

/// verify 审计（决策③配套）：形态不支持的 x-filter 声明 → filter_shape_unsupported；
/// 支持形态不告警。
#[test]
fn filter_declaration_audit_reports_unsupported_shapes() {
    let f = fresh_store();
    let report = mem_core::find::filter_declaration_audit(&f.types);
    // doc.obj（object）与 doc.form（纯 enum）不支持；status/score/flag/tags/mixed 支持。
    let joined = report.join("\n");
    assert!(joined.contains("doc 字段 obj"), "{report:?}");
    assert!(joined.contains("doc 字段 form"), "{report:?}");
    assert!(joined.contains("filter_shape_unsupported"));
    assert!(!joined.contains("字段 status"), "支持形态不告警：{report:?}");
    assert!(!joined.contains("字段 mixed"), "数组形态支持：{report:?}");
}
