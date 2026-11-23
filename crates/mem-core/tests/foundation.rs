//! §8-4a 核心库地基测试：envelope / token / record / paths / store / platform 原语。
//!
//! 锚点：D18（信封物理格式）、红线27（正文按字节保持）、D3/红线19（全摘要令牌 CAS）、
//! D19/D11（键字符集与路径推导）、§3.1（目录布局）、红线6（崩溃原子落盘）、
//! 红线18（读路径零写盘——含只读形态判定不得借写探测真相）。

use mem_core::envelope::{Envelope, Ref};
use mem_core::paths;
use mem_core::record::Record;
use mem_core::store::{OpenMode, Store};
use mem_core::token::{self, Token};

const UUID_A: &str = "0198f7a2-b4c1-7cd2-9e3f-1a2b3c4d5e6f";
const UUID_B: &str = "0198f7b2-0000-7cd2-9e3f-1a2b3c4d5e6f";
const UUID_C: &str = "0198f7c2-0000-7cd2-9e3f-1a2b3c4d5e6f";

#[test]
fn capability_manifest_matches_runtime_format_and_feature_tables() {
    let manifest = Store::capability_manifest();
    let capability_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/develop/embedded-downstream-contract-capabilities.json");
    let published: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&capability_path).unwrap()).unwrap();
    assert_eq!(manifest["schema_version"], 1);
    assert_eq!(manifest["product_version"], "1.0.0");
    assert_eq!(manifest["formats"]["read_write"][0], Store::CURRENT_FORMAT);
    assert_eq!(Store::supported_features(), &["event.restore_body/v1"]);
    assert_eq!(
        manifest["formats"]["read_only"],
        serde_json::to_value(Store::READ_ONLY_FORMATS).unwrap()
    );
    assert_eq!(
        manifest["features"]["supported"],
        serde_json::to_value(Store::supported_features()).unwrap()
    );
    assert_eq!(
        manifest["features"]["read_compatible"],
        serde_json::to_value(Store::read_compatible_features()).unwrap()
    );
    for section in ["schema_version", "product_version", "formats", "features"] {
        assert_eq!(
            published[section], manifest[section],
            "发布机读能力文件的 {section} 与运行时不一致"
        );
    }
    assert_eq!(
        published["stable_rust_api"],
        "embedded-downstream-contract-api.md"
    );
    assert_eq!(
        published["mcp_tools_source"],
        "../../crates/mem-mcp/src/tools.rs"
    );
    assert_eq!(
        published["change_log"],
        "embedded-downstream-contract-changelog.md"
    );
    assert!(
        capability_path
            .parent()
            .unwrap()
            .join(published["stable_rust_api"].as_str().unwrap())
            .is_file()
    );
    assert!(
        capability_path
            .parent()
            .unwrap()
            .join(published["mcp_tools_source"].as_str().unwrap())
            .is_file()
    );
    let change_log = std::fs::read_to_string(
        capability_path
            .parent()
            .unwrap()
            .join(published["change_log"].as_str().unwrap()),
    )
    .unwrap();
    assert!(change_log.contains("候选，未发布"));
    assert!(change_log.contains("维护者已选定首个稳定契约版本号 `1.0.0`"));
    assert!(
        published["validated_coexistence"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(published["release_evidence"]["status"], "candidate-only");
}

fn sample_envelope() -> Envelope {
    Envelope {
        id: UUID_A.to_string(),
        r#type: "task".into(),
        created: "2026-11-04T09:12:00Z".into(),
        modified: "2026-11-04T11:40:00Z".into(),
        version: 6,
        principal: Some(UUID_B.to_string()),
        refs: vec![
            Ref {
                rel: "belongs_to".into(),
                target: UUID_C.to_string(),
            },
            Ref {
                rel: "derived_from".into(),
                target: UUID_B.to_string(),
            },
        ],
        aliases: vec!["login-api".into()],
        evidence_cutoff_seq: None,
        commit: Some("e3f1".into()),
        extra: Default::default(),
    }
}

fn sample_payload() -> serde_json::Value {
    serde_json::json!({
        "project": UUID_C,
        "slug": "login-api",
        "status": "in_progress",
        "due": "2026-11-10"
    })
}

fn append_raw_config_member(config: &str, key: &str, raw_value: &str) -> String {
    let mut text = config.trim_end().strip_suffix('}').unwrap().to_owned();
    text.push_str(",\n  ");
    text.push_str(&serde_json::to_string(key).unwrap());
    text.push_str(": ");
    text.push_str(raw_value);
    text.push_str("\n}\n");
    text
}

// ---------- envelope：确定性序列化（D18/红线27） ----------

#[test]
fn envelope_roundtrip_preserves_all_fields() {
    let e = sample_envelope();
    let v = e.to_json_value();
    let e2 = Envelope::from_json(&v).unwrap();
    assert_eq!(e2, e);
}

#[test]
fn envelope_serialization_is_deterministic_and_sorted() {
    let e = sample_envelope();
    let a = serde_json::to_string(&e.to_json_value()).unwrap();
    let b = serde_json::to_string(&e.to_json_value()).unwrap();
    assert_eq!(a, b, "同数据必须产出同字节（键排序固定）");
    // 字母序：aliases 在 created 前；envelope 键序由外层 frontmatter 保证。
    assert!(a.find("\"aliases\"").unwrap() < a.find("\"created\"").unwrap());
    assert!(a.find("\"commit\"").unwrap() < a.find("\"created\"").unwrap());
}

#[test]
fn envelope_unknown_keys_are_preserved_not_dropped() {
    let mut e = sample_envelope();
    e.extra
        .insert("custom_note".into(), serde_json::json!("保留我"));
    let v = e.to_json_value();
    let e2 = Envelope::from_json(&v).unwrap();
    assert_eq!(
        e2.extra.get("custom_note"),
        Some(&serde_json::json!("保留我"))
    );
}

#[test]
fn uuid_v7_ids_are_generated_and_valid() {
    let id = mem_core::envelope::new_uuid_v7();
    assert_eq!(id.len(), 36);
    // uuid v7：第 14 个字符 = '7'（版本位）；第 19 个字符 ∈ {8,9,a,b}（变体位）。
    assert_eq!(id.as_bytes()[14] as char, '7', "{id}");
    assert!(
        matches!(id.as_bytes()[19] as char, '8' | '9' | 'a' | 'b'),
        "{id}"
    );
}

// ---------- record：解析响亮失败 + 正文字节保持（D18/红线27） ----------

fn record_bytes(body: &str) -> Vec<u8> {
    let e = sample_envelope();
    let p = sample_payload();
    mem_core::record::serialize_new(&e, &p, body.as_bytes())
}

#[test]
fn record_parse_ok_and_body_byte_exact() {
    let body = "# 实现登录接口\r\n\r\n注意 CRLF 与中文 —— 字节领土。\r\n- [ ] TODO\r\n";
    let bytes = record_bytes(body);
    let rec = Record::from_bytes(&bytes).expect("parse");
    assert_eq!(rec.envelope.id, UUID_A);
    assert_eq!(rec.envelope.version, 6);
    assert_eq!(rec.payload["slug"], "login-api");
    assert_eq!(rec.body, body.as_bytes(), "正文必须按字节保持（含 CRLF）");
}

#[test]
fn record_parse_loud_on_missing_closing_delimiter() {
    let mut bytes = record_bytes("正文");
    // 截掉结尾的 "---\n正文"——闭合分隔符彻底消失（模拟被截断的文件）。
    let n = bytes.len();
    bytes.truncate(n - 4 - "正文".len());
    assert!(
        Record::from_bytes(&bytes).is_err(),
        "缺闭合分隔符必须响亮失败"
    );
}

#[test]
fn record_parse_loud_on_bad_frontmatter_json() {
    let bytes = b"---\n{not json}\n---\nbody\n";
    let err = Record::from_bytes(bytes).unwrap_err();
    assert!(err.to_string().contains("frontmatter"), "{err}");
}

#[test]
fn record_parse_loud_on_bom() {
    let mut bytes = b"\xEF\xBB\xBF---\n{}\n---\n".to_vec();
    bytes.extend_from_slice(b"body");
    assert!(Record::from_bytes(&bytes).is_err(), "BOM 必须响亮拒绝");
}

#[test]
fn record_parse_loud_on_missing_envelope_block() {
    let bytes: &[u8] = b"---\n{\"payload\":{}}\n---\n";
    assert!(Record::from_bytes(bytes).is_err());
}

#[test]
fn record_regen_keeps_body_bytes_and_rewrites_frontmatter() {
    let body = "人手写的正文\r\n---\r\n（正文里出现分隔符样文本也是字节领土）\r\n";
    let original = record_bytes(body);
    let rec = Record::from_bytes(&original).unwrap();
    // 机制再生成：只动 envelope（version+1），payload 原样，body 原样。
    let mut e2 = rec.envelope.clone();
    e2.version = 7;
    e2.modified = "2026-11-05T00:00:00Z".into();
    let out = mem_core::record::serialize_new(&e2, &rec.payload, &rec.body);
    let rec2 = Record::from_bytes(&out).unwrap();
    assert_eq!(rec2.body, body.as_bytes(), "红线27：正文按字节保持");
    assert_eq!(rec2.envelope.version, 7);
    // 打乱 frontmatter 键序的人手文件：再生成后解析等价、正文不变。
    let hand_written = b"---\n{\"payload\":{\"slug\":\"login-api\"},\"envelope\":{\"version\":6,\"id\":\"0198f7a2-b4c1-7cd2-9e3f-1a2b3c4d5e6f\",\"type\":\"task\",\"created\":\"2026-11-04T09:12:00Z\",\"modified\":\"2026-11-04T11:40:00Z\",\"principal\":null,\"refs\":[],\"aliases\":[],\"evidence_cutoff_seq\":null,\"commit\":null}}\n---\nhand body\r\n";
    let r = Record::from_bytes(hand_written).unwrap();
    let regen = mem_core::record::serialize_new(&r.envelope, &r.payload, &r.body);
    let r2 = Record::from_bytes(&regen).unwrap();
    assert_eq!(r2.body, b"hand body\r\n");
    assert!(
        regen.starts_with(b"---\n{\n  \"envelope\": {"),
        "再生成必须确定性键序"
    );
}

// ---------- token：全摘要 CAS（D3/红线19） ----------

#[test]
fn token_format_and_parse() {
    let bytes = record_bytes("x");
    let t = token::for_bytes(6, &bytes);
    assert_eq!(t.version, 6);
    assert_eq!(t.digest.len(), 64, "全 SHA-256，64 hex");
    assert!(
        t.digest
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    );
    let rendered = t.to_string();
    assert!(rendered.starts_with("v6@"));
    let t2: Token = rendered.parse().unwrap();
    assert_eq!(t2, t);
    assert!(
        "v6@abc".parse::<Token>().is_err(),
        "短摘要拒绝（红线19：禁截断摘要）"
    );
}

#[test]
fn cas_detects_version_and_digest_mismatch() {
    let a = record_bytes("v1 内容");
    let b = record_bytes("v2 内容");
    let t_a = token::for_bytes(5, &a);
    let t_b = token::for_bytes(6, &b);
    assert!(token::cas_check(&t_a, &t_a).is_ok());
    // 版本不同
    let e = token::cas_check(&t_a, &t_b).unwrap_err();
    assert!(e.to_string().contains("conflict"));
    // 保版本手改：版本同、摘要不同 → 必被检出（红线19 的"必"）
    let t_hand = Token {
        version: t_a.version,
        digest: token::sha256_hex("手改内容".as_bytes()),
    };
    assert!(token::cas_check(&t_hand, &t_a).is_err());
}

// ---------- paths：NFC/casefold/黑名单/保留名/长度（D19） ----------

#[test]
fn fold_collapses_case_and_normalizes_nfc() {
    assert_eq!(paths::fold("Login-API"), paths::fold("login-api"));
    // NFC：等价分解序列折叠到同一成分。
    let decomposed = "cafe\u{0301}"; // café NFD
    let composed = "caf\u{e9}"; // café NFC
    assert_eq!(paths::fold(decomposed), paths::fold(composed));
}

#[test]
fn slug_charset_rules() {
    assert!(paths::validate_component("login-api_2").is_ok());
    assert!(paths::validate_component("登录接口").is_ok());
    for bad in [
        "a<b",
        "a>b",
        "a:b",
        "a\"b",
        "a/b",
        "a\\b",
        "a|b",
        "a?b",
        "a*b",
        "a\u{7f}b",
        "a\u{1}b",
        "trailing.",
        "trailing ",
        ".dot",
        "..",
        "...",
        "con",
        "CON",
        "aux",
        "nul",
        "com1",
        "lpt2",
    ] {
        assert!(paths::validate_component(bad).is_err(), "应拒收：{bad:?}");
    }
    assert!(
        paths::validate_component("conventions").is_err(),
        "店级保留名"
    );
    assert!(paths::validate_component("types").is_err());
    assert!(paths::validate_component("").is_err());
    // 长度上限 128 字节（UTF-8）。
    let long = "a".repeat(128); // 恰好 128 字节
    assert!(paths::validate_component(&long).is_ok());
    assert!(
        paths::validate_component(&(long + "a")).is_err(),
        "129 字节拒收"
    );
    let cjk = "长".repeat(42); // 126 字节合法
    assert!(paths::validate_component(&cjk).is_ok());
}

#[test]
fn derive_record_rel_from_unique_key() {
    // 有唯一键：records/<type>/<key1折叠>/<key2折叠>.md（键序 = x-unique-key 序）。
    let rel = paths::derive_record_rel(
        "task",
        &[UUID_C.to_string(), "Login-API".to_string()],
        "0198f7a2-b4c1-7cd2-9e3f-1a2b3c4d5e6f",
    )
    .unwrap();
    assert_eq!(rel, format!("records/task/{UUID_C}/login-api.md"));
    let lock = paths::record_lock_rel(&rel);
    assert_eq!(lock, format!("records/task/{UUID_C}/login-api.md.lock"));
    let attic = paths::attic_rel(&rel).unwrap();
    assert_eq!(attic, format!("attic/task/{UUID_C}/login-api.md"));
    // 无唯一键：records/<type>/<id>.md。
    let rel2 =
        paths::derive_record_rel("memory", &[], "0198aaaa-0000-7cd2-9e3f-000000000001").unwrap();
    assert_eq!(
        rel2,
        "records/memory/0198aaaa-0000-7cd2-9e3f-000000000001.md"
    );
}

// ---------- L1 守卫：确定性序列化依赖 serde_json Map = BTreeMap（键排序） ----------

#[test]
fn serde_json_map_is_sorted_by_default() {
    // L1：一旦未来依赖启用 preserve_order（IndexMap），frontmatter 键序与全部
    // 摘要翻转——此测试作为依赖树守卫（红线27 确定性序列化的前提）。
    let mut m = serde_json::Map::new();
    m.insert("z".to_string(), serde_json::json!(1));
    m.insert("a".to_string(), serde_json::json!(2));
    m.insert("m".to_string(), serde_json::json!(3));
    let out = serde_json::to_string(&serde_json::Value::Object(m)).unwrap();
    assert_eq!(
        out, r#"{"a":2,"m":3,"z":1}"#,
        "Map 必须按键排序（preserve_order 未启用）"
    );
}

// ---------- store：布局/打开/只读判定 ----------

#[test]
fn store_create_layout_and_open() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("store");
    Store::create(&root).expect("create");
    for d in [
        "records",
        "attic",
        "sessions",
        "agenda",
        "types",
        "conventions",
        "index",
        ".tmp",
    ] {
        assert!(root.join(d).is_dir(), "缺目录 {d}");
    }
    assert!(root.join("mem.json").is_file());
    assert!(root.join("events.jsonl").is_file());
    assert!(root.join("conventions/CONVENTIONS.md").is_file());

    let store = Store::open(&root, OpenMode::Auto).expect("open");
    assert!(store.writable());
    assert_eq!(store.config().budgets.list_limit, 50, "规范默认预算");
    assert_eq!(store.config().format_version, "spacememory/2");
}

#[test]
fn store_open_fails_loud_without_mem_json() {
    let tmp = tempfile::tempdir().unwrap();
    let err = Store::open(tmp.path(), OpenMode::Auto).unwrap_err();
    assert!(err.to_string().contains("mem.json"));
}

#[test]
fn store_open_rejects_unknown_format_before_writable_probe() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("store");
    Store::create(&root).unwrap();
    let mem_json = root.join("mem.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&mem_json).unwrap()).unwrap();
    config["format_version"] = serde_json::json!("spacememory/999");
    std::fs::write(&mem_json, serde_json::to_vec(&config).unwrap()).unwrap();
    std::fs::remove_dir_all(root.join(".tmp")).unwrap();

    let error = Store::open(&root, OpenMode::Auto).unwrap_err();

    assert!(
        matches!(error, mem_core::error::MemError::UnsupportedFormat { found, .. } if found == "spacememory/999")
    );
    assert!(!root.join(".tmp").exists(), "格式拒绝前不得进行可写探测");
}

#[test]
fn legacy_numeric_format_opens_only_explicitly_readonly() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("legacy");
    Store::create(&root).unwrap();
    let mem_json = root.join("mem.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&mem_json).unwrap()).unwrap();
    config["format_version"] = serde_json::json!(1);
    config.as_object_mut().unwrap().remove("required_features");
    std::fs::write(&mem_json, serde_json::to_vec(&config).unwrap()).unwrap();

    let auto = Store::open(&root, OpenMode::Auto).unwrap_err();
    assert!(auto.to_string().contains("须先迁移"), "{auto:?}");
    let before_readonly_open = snapshot_tree(&root);
    let readonly = Store::open(&root, OpenMode::ForceReadOnly).unwrap();
    assert!(!readonly.writable());
    assert_eq!(readonly.config().format_version, "1");
    assert_eq!(
        snapshot_tree(&root),
        before_readonly_open,
        "旧格式只读打开不得创建、删除或改写任何店内文件"
    );
    assert_eq!(
        std::fs::read(&mem_json).unwrap(),
        serde_json::to_vec(&config).unwrap()
    );
}

#[test]
fn format_migration_is_atomic_idempotent_and_preserves_unknown_config() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("legacy-migration");
    Store::create(&root).unwrap();
    let config_path = root.join("mem.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    let record_path = root.join("records/task/legacy.md");
    std::fs::create_dir_all(record_path.parent().unwrap()).unwrap();
    let legacy_record = mem_core::record::serialize_new(
        &sample_envelope(),
        &sample_payload(),
        b"legacy-user-bytes\r\n\x00\xff",
    );
    std::fs::write(&record_path, legacy_record).unwrap();
    let record_before = std::fs::read(&record_path).unwrap();
    let record_stat_before = std::fs::metadata(&record_path).unwrap();
    let event = mem_core::event::Event {
        seq: 1,
        ts: "2026-11-22T00:00:00Z".into(),
        id: UUID_A.into(),
        path: "records/task/legacy.md".into(),
        op: mem_core::event::Op::Create,
        old_v: None,
        new_v: Some(6),
        old_digest: None,
        new_digest: Some(mem_core::token::sha256_hex(&record_before)),
        by: "migration-test".into(),
        kind: mem_core::event::Kind::Op,
    };
    std::fs::write(root.join("events.jsonl"), event.to_line()).unwrap();
    config["format_version"] = serde_json::json!(1);
    config.as_object_mut().unwrap().remove("required_features");
    config["future_config"] = serde_json::json!({"nested": ["retained", 7]});
    std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    let before_records = std::fs::read(root.join("events.jsonl")).unwrap();
    let backup = tmp.path().join("store-backup");
    copy_directory(&root, &backup);

    let report = Store::migrate_format_v1(&root, "backup-sha256:abc123").unwrap();

    assert_eq!(report.status, mem_core::store::MigrationStatus::Migrated);
    let migrated: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    assert_eq!(migrated["format_version"], "spacememory/2");
    assert_eq!(migrated["required_features"], serde_json::json!([]));
    assert_eq!(migrated["future_config"], config["future_config"]);
    assert_eq!(
        std::fs::read(root.join("events.jsonl")).unwrap(),
        before_records
    );
    let record_stat_after = std::fs::metadata(&record_path).unwrap();
    assert_eq!(
        std::fs::read(&record_path).unwrap(),
        record_before,
        "记录正文和任意用户字节必须原样保留"
    );
    assert_eq!(record_stat_after.len(), record_stat_before.len());

    // 用迁前全量副本演练回退恢复；恢复到独立目录并通过旧格式只读打开。
    let restored = tmp.path().join("restored-store");
    copy_directory(&backup, &restored);
    assert_eq!(
        snapshot_tree(&backup),
        snapshot_tree(&restored),
        "完整迁前备份必须逐文件、逐字节可恢复"
    );
    let restored_store = Store::open(&restored, OpenMode::ForceReadOnly).unwrap();
    assert_eq!(restored_store.config().format_version, "1");
    assert_eq!(
        std::fs::read(restored.join("events.jsonl")).unwrap(),
        before_records
    );
    assert_eq!(
        std::fs::read(restored.join("records/task/legacy.md")).unwrap(),
        record_before
    );

    let migrated_bytes = std::fs::read(&config_path).unwrap();
    let second = Store::migrate_format_v1(&root, "backup-sha256:abc123").unwrap();
    assert_eq!(
        second.status,
        mem_core::store::MigrationStatus::AlreadyMigrated
    );
    assert_eq!(
        std::fs::read(&config_path).unwrap(),
        migrated_bytes,
        "幂等迁移不得改写已迁移配置的字节"
    );
}

fn copy_directory(source: &std::path::Path, target: &std::path::Path) {
    std::fs::create_dir_all(target).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let src = entry.path();
        let dst = target.join(entry.file_name());
        if src.is_dir() {
            copy_directory(&src, &dst);
        } else {
            std::fs::copy(src, dst).unwrap();
        }
    }
}

fn snapshot_tree(
    root: &std::path::Path,
) -> std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> {
    fn visit(
        root: &std::path::Path,
        dir: &std::path::Path,
        out: &mut std::collections::BTreeMap<std::path::PathBuf, Vec<u8>>,
    ) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(root, &path, out);
            } else {
                out.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    std::fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut out = std::collections::BTreeMap::new();
    visit(root, root, &mut out);
    out
}

#[test]
fn format_migration_rejects_record_event_drift_before_switching_format() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("legacy-drift");
    Store::create(&root).unwrap();
    let record_path = root.join("records/task/legacy.md");
    std::fs::create_dir_all(record_path.parent().unwrap()).unwrap();
    std::fs::write(
        &record_path,
        mem_core::record::serialize_new(&sample_envelope(), &sample_payload(), b"bytes"),
    )
    .unwrap();
    let config_path = root.join("mem.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    config["format_version"] = serde_json::json!(1);
    config.as_object_mut().unwrap().remove("required_features");
    std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    let before = std::fs::read(&config_path).unwrap();

    let error = Store::migrate_format_v1(&root, "backup").unwrap_err();

    assert!(matches!(
        error,
        mem_core::error::MemError::MigrationState(_)
    ));
    assert_eq!(std::fs::read(config_path).unwrap(), before);
}

#[test]
fn migration_rejects_missing_history_record_duplicate_seq_and_corrupt_target_config() {
    for case in ["missing", "duplicate-seq", "corrupt-target"] {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("legacy");
        Store::create(&root).unwrap();
        let config_path = root.join("mem.json");
        let mut config: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
        if case != "corrupt-target" {
            let path = root.join("records/task/legacy.md");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let bytes =
                mem_core::record::serialize_new(&sample_envelope(), &sample_payload(), b"body");
            std::fs::write(&path, &bytes).unwrap();
            let ev = mem_core::event::Event {
                seq: 1,
                ts: "2026-11-22T00:00:00Z".into(),
                id: UUID_A.into(),
                path: "records/task/legacy.md".into(),
                op: mem_core::event::Op::Create,
                old_v: None,
                new_v: Some(6),
                old_digest: None,
                new_digest: Some(mem_core::token::sha256_hex(&bytes)),
                by: "test".into(),
                kind: mem_core::event::Kind::Op,
            };
            let mut events = ev.to_line();
            if case == "duplicate-seq" {
                events.extend_from_slice(&ev.to_line());
            }
            std::fs::write(root.join("events.jsonl"), events).unwrap();
            if case == "missing" {
                std::fs::remove_file(path).unwrap();
            }
        } else {
            config["created"] = serde_json::Value::Null;
        }
        if case == "corrupt-target" {
            config["format_version"] = serde_json::json!("spacememory/2");
            config["required_features"] = serde_json::json!([]);
        } else {
            config["format_version"] = serde_json::json!(1);
            config.as_object_mut().unwrap().remove("required_features");
        }
        std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
        let before = std::fs::read(&config_path).unwrap();
        assert!(
            Store::migrate_format_v1(&root, "backup").is_err(),
            "case={case}"
        );
        assert_eq!(std::fs::read(&config_path).unwrap(), before, "case={case}");
    }
}

#[test]
fn migration_accepts_consistent_deleted_tombstone_and_rejects_bad_unmapped_tombstone() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("deleted-legacy");
    Store::create(&root).unwrap();
    let config_path = root.join("mem.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    config["format_version"] = serde_json::json!(1);
    config.as_object_mut().unwrap().remove("required_features");

    let record_rel = "records/task/legacy.md";
    let attic_rel = "attic/task/legacy.md";
    let bytes =
        mem_core::record::serialize_new(&sample_envelope(), &sample_payload(), b"deleted body");
    let attic_path = root.join(attic_rel);
    std::fs::create_dir_all(attic_path.parent().unwrap()).unwrap();
    std::fs::write(&attic_path, &bytes).unwrap();
    let digest = mem_core::token::sha256_hex(&bytes);
    let create = mem_core::event::Event {
        seq: 1,
        ts: "2026-11-22T00:00:00Z".into(),
        id: UUID_A.into(),
        path: record_rel.into(),
        op: mem_core::event::Op::Create,
        old_v: None,
        new_v: Some(6),
        old_digest: None,
        new_digest: Some(digest.clone()),
        by: "test".into(),
        kind: mem_core::event::Kind::Op,
    };
    let delete = mem_core::event::Event {
        seq: 2,
        ts: "2026-11-22T00:00:01Z".into(),
        id: UUID_A.into(),
        path: attic_rel.into(),
        op: mem_core::event::Op::Delete,
        old_v: Some(6),
        new_v: Some(6),
        old_digest: Some(digest.clone()),
        new_digest: Some(digest),
        by: "test".into(),
        kind: mem_core::event::Kind::Op,
    };
    let mut events = create.to_line();
    events.extend_from_slice(&delete.to_line());
    std::fs::write(root.join("events.jsonl"), events.clone()).unwrap();
    std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();

    Store::migrate_format_v1(&root, "backup").unwrap();
    assert_eq!(std::fs::read(root.join("events.jsonl")).unwrap(), events);
    assert_eq!(std::fs::read(&attic_path).unwrap(), bytes);

    let bad_root = tmp.path().join("bad-tombstone-legacy");
    Store::create(&bad_root).unwrap();
    let bad_config_path = bad_root.join("mem.json");
    let mut bad_config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&bad_config_path).unwrap()).unwrap();
    bad_config["format_version"] = serde_json::json!(1);
    bad_config
        .as_object_mut()
        .unwrap()
        .remove("required_features");
    std::fs::write(&bad_config_path, serde_json::to_vec(&bad_config).unwrap()).unwrap();
    let bad = bad_root.join("attic/bad.md");
    std::fs::create_dir_all(bad.parent().unwrap()).unwrap();
    std::fs::write(&bad, b"not a valid record").unwrap();
    let before = std::fs::read(&bad_config_path).unwrap();
    assert!(Store::migrate_format_v1(&bad_root, "backup").is_err());
    assert_eq!(std::fs::read(&bad_config_path).unwrap(), before);
}

#[test]
fn config_mutations_preserve_arbitrary_precision_unknown_numbers() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("precise");
    Store::create(&root).unwrap();
    let config_path = root.join("mem.json");
    let original = std::fs::read_to_string(&config_path).unwrap();
    let original = append_raw_config_member(
        &original,
        "opaque",
        r#"{"id":100000000000000000001,"ratio":0.123456789012345678901,"sentinel":{"$serde_json::private::Number":"123"}}"#,
    );
    std::fs::write(&config_path, original).unwrap();
    let store = Store::open(&root, OpenMode::Auto).unwrap();
    store
        .register_required_feature("event.restore_body/v1")
        .unwrap();
    let text = std::fs::read_to_string(&config_path).unwrap();
    assert!(text.contains("100000000000000000001"), "{text}");
    assert!(text.contains("0.123456789012345678901"), "{text}");
    let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        parsed["opaque"]["sentinel"]["$serde_json::private::Number"],
        "123"
    );
    store
        .set_config_value("another", serde_json::json!(true))
        .unwrap();
    let text = std::fs::read_to_string(&config_path).unwrap();
    assert!(text.contains("100000000000000000001"), "{text}");
    assert!(text.contains("0.123456789012345678901"), "{text}");
    let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        parsed["opaque"]["sentinel"]["$serde_json::private::Number"],
        "123"
    );
}

#[test]
fn migration_preserves_arbitrary_precision_unknown_numbers() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("precise-legacy");
    Store::create(&root).unwrap();
    let path = root.join("mem.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    config["format_version"] = serde_json::json!(1);
    config.as_object_mut().unwrap().remove("required_features");
    let raw = append_raw_config_member(
        &serde_json::to_string(&config).unwrap(),
        "opaque",
        r#"{"id":100000000000000000001,"ratio":0.123456789012345678901,"sentinel":{"$serde_json::private::Number":"123"}}"#,
    );
    std::fs::write(&path, raw).unwrap();
    Store::migrate_format_v1(&root, "backup").unwrap();
    let text = std::fs::read_to_string(path).unwrap();
    assert!(text.contains("100000000000000000001"), "{text}");
    assert!(text.contains("0.123456789012345678901"), "{text}");
    let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        parsed["opaque"]["sentinel"]["$serde_json::private::Number"],
        "123"
    );
}

#[test]
fn format_migration_refuses_pending_commit_debt_without_switching() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("legacy-pending");
    Store::create(&root).unwrap();
    let config_path = root.join("mem.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    config["format_version"] = serde_json::json!(1);
    config.as_object_mut().unwrap().remove("required_features");
    std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    let pending_path = root.join(".pending-commits/recovery.json");
    std::fs::create_dir_all(pending_path.parent().unwrap()).unwrap();
    std::fs::write(
        &pending_path,
        br#"{"paths":["events.jsonl"],"ts":"2026-11-22T00:00:00Z","error":"simulated"}"#,
    )
    .unwrap();
    let before = std::fs::read(&config_path).unwrap();

    let error = Store::migrate_format_v1(&root, "backup").unwrap_err();

    assert!(matches!(
        error,
        mem_core::error::MemError::MigrationState(_)
    ));
    assert_eq!(std::fs::read(config_path).unwrap(), before);
}

#[test]
fn format_migration_refuses_corrupt_agenda_plan_without_switching() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("legacy-corrupt-agenda");
    Store::create(&root).unwrap();
    let config_path = root.join("mem.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    config["format_version"] = serde_json::json!(1);
    config.as_object_mut().unwrap().remove("required_features");
    std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    let agenda_dir = root.join("agenda");
    std::fs::create_dir_all(&agenda_dir).unwrap();
    std::fs::write(agenda_dir.join("plan-broken.json"), b"{broken").unwrap();
    let before = std::fs::read(&config_path).unwrap();

    let error = Store::migrate_format_v1(&root, "backup").unwrap_err();

    assert!(matches!(
        error,
        mem_core::error::MemError::MigrationState(_)
    ));
    assert_eq!(std::fs::read(config_path).unwrap(), before);
}

#[test]
fn format_migration_refuses_nonempty_feature_state_without_mutation() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("legacy-featured");
    Store::create(&root).unwrap();
    let config_path = root.join("mem.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    config["format_version"] = serde_json::json!(1);
    config["required_features"] = serde_json::json!(["some-feature/v1"]);
    std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    let before = std::fs::read(&config_path).unwrap();

    let error = Store::migrate_format_v1(&root, "backup").unwrap_err();

    assert!(matches!(
        error,
        mem_core::error::MemError::MigrationState(_)
    ));
    assert_eq!(std::fs::read(config_path).unwrap(), before);
}

#[test]
fn migration_fails_fast_when_new_protocol_writer_holds_store_barrier() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("migration-lock");
    Store::create(&root).unwrap();
    let config_path = root.join("mem.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    config["format_version"] = serde_json::json!(1);
    config.as_object_mut().unwrap().remove("required_features");
    std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    let store = Store::open(&root, OpenMode::ForceReadOnly).unwrap();
    let gate = mem_core::lock::barrier_shared(&store).unwrap();

    let started = std::time::Instant::now();
    let error = Store::migrate_format_v1(&root, "backup").unwrap_err();

    assert!(matches!(
        error,
        mem_core::error::MemError::LockTimeout { .. }
    ));
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    assert_eq!(
        std::fs::read(&config_path).unwrap(),
        serde_json::to_vec(&config).unwrap()
    );
    drop(gate);
}

#[test]
fn missing_required_features_allow_only_explicit_readonly_and_block_existing_handle_writes() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("store");
    Store::create(&root).unwrap();
    let store = Store::open(&root, OpenMode::Auto).unwrap();
    let mem_json = root.join("mem.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&mem_json).unwrap()).unwrap();
    config["required_features"] = serde_json::json!(["event.future/v1", "record.future/v1"]);
    std::fs::write(&mem_json, serde_json::to_vec(&config).unwrap()).unwrap();

    assert!(
        matches!(store.check_writable(), Err(mem_core::error::MemError::MissingFeatures(features)) if features == vec!["event.future/v1", "record.future/v1"])
    );
    assert!(
        matches!(Store::open(&root, OpenMode::Auto), Err(mem_core::error::MemError::MissingFeatures(features)) if features.len() == 2)
    );
    assert!(
        matches!(Store::open(&root, OpenMode::ForceReadOnly), Err(mem_core::error::MemError::MissingFeatures(features)) if features.len() == 2)
    );
}

#[test]
fn existing_handle_rejects_missing_required_features_field_for_reads_writes_and_maintenance() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("missing-required-field");
    Store::create(&root).unwrap();
    let store = Store::open(&root, OpenMode::Auto).unwrap();
    let path = store.abs("mem.json");
    let mut raw: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    raw.as_object_mut().unwrap().remove("required_features");
    std::fs::write(&path, serde_json::to_vec(&raw).unwrap()).unwrap();

    assert!(matches!(
        store.current_format_version(),
        Err(mem_core::error::MemError::Corrupt { .. })
    ));
    assert!(matches!(
        store.check_writable(),
        Err(mem_core::error::MemError::Corrupt { .. })
    ));
    assert!(matches!(
        store.acquire_index_maintenance(),
        Err(mem_core::error::MemError::Corrupt { .. })
    ));
}

#[test]
fn required_feature_ids_must_be_valid_and_unique_before_open_probe() {
    for value in [
        serde_json::json!(["missing-format"]),
        serde_json::json!(["record.future/v0"]),
        serde_json::json!(["record.future/v1", "record.future/v1"]),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("invalid-feature-list");
        Store::create(&root).unwrap();
        let path = root.join("mem.json");
        let mut config: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        config["required_features"] = value;
        std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
        std::fs::remove_dir_all(root.join(".tmp")).unwrap();

        assert!(matches!(
            Store::open(&root, OpenMode::Auto),
            Err(mem_core::error::MemError::Corrupt { .. })
        ));
        assert!(
            !root.join(".tmp").exists(),
            "格式清单非法必须早于可写探测失败"
        );
    }
}

#[test]
fn feature_registration_rejects_unknown_feature_without_changing_store() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("store");
    Store::create(&root).unwrap();
    let store = Store::open(&root, OpenMode::Auto).unwrap();
    let path = store.abs("mem.json");
    let before = std::fs::read(&path).unwrap();

    let error = store
        .register_required_feature("record.future/v1")
        .unwrap_err();

    assert!(
        matches!(error, mem_core::error::MemError::MissingFeatures(features) if features == vec!["record.future/v1"])
    );
    assert_eq!(std::fs::read(path).unwrap(), before);
}

#[test]
fn feature_registration_batch_empty_is_noop_and_unsupported_is_net_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("store");
    Store::create(&root).unwrap();
    let store = Store::open(&root, OpenMode::Auto).unwrap();
    let before = std::fs::read(store.abs("mem.json")).unwrap();

    store.register_required_features(&[]).unwrap();
    store
        .register_required_features(&["not-a-feature", "not-a-feature"])
        .unwrap_err();
    let missing = store
        .register_required_features(&["record.future/v1", "event.future/v1"])
        .unwrap_err();
    assert!(matches!(
        missing,
        mem_core::error::MemError::MissingFeatures(features)
            if features == vec!["event.future/v1", "record.future/v1"]
    ));
    store
        .register_required_feature("record.future/v1")
        .unwrap_err();
    assert!(store.config().required_features.is_empty());
    assert_eq!(std::fs::read(store.abs("mem.json")).unwrap(), before);

    let readonly = Store::open(&root, OpenMode::ForceReadOnly).unwrap();
    readonly.register_required_features(&[]).unwrap();
}

#[test]
fn feature_registration_rejects_invalid_identifier_as_structured_error() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("store");
    Store::create(&root).unwrap();
    let store = Store::open(&root, OpenMode::Auto).unwrap();
    let path = store.abs("mem.json");
    let before = std::fs::read(&path).unwrap();

    let error = store
        .register_required_feature("not-a-feature")
        .unwrap_err();

    assert!(
        matches!(error, mem_core::error::MemError::InvalidFeature(id) if id == "not-a-feature")
    );
    assert_eq!(std::fs::read(path).unwrap(), before);
}

#[test]
fn config_value_update_rechecks_features_and_preserves_disk_on_rejection() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("store");
    Store::create(&root).unwrap();
    let store = Store::open(&root, OpenMode::Auto).unwrap();
    let path = store.abs("mem.json");
    let mut raw: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    raw["required_features"] = serde_json::json!(["config.future/v1"]);
    std::fs::write(&path, serde_json::to_vec(&raw).unwrap()).unwrap();
    let before = std::fs::read(&path).unwrap();

    let error = store
        .set_config_value("default_view", serde_json::json!("view-id"))
        .unwrap_err();

    assert!(
        matches!(error, mem_core::error::MemError::MissingFeatures(features) if features == vec!["config.future/v1"])
    );
    assert_eq!(std::fs::read(path).unwrap(), before);
}

#[test]
fn invalid_known_config_value_is_rejected_before_disk_or_cache_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("invalid-config-update");
    Store::create(&root).unwrap();
    let store = Store::open(&root, OpenMode::Auto).unwrap();
    let path = store.abs("mem.json");
    let before_disk = std::fs::read(&path).unwrap();
    let before_cache = store.config();

    assert!(
        store
            .set_config_value("budgets", serde_json::json!("not-an-object"))
            .is_err()
    );

    assert_eq!(std::fs::read(&path).unwrap(), before_disk);
    assert_eq!(store.config(), before_cache);
}

#[test]
fn readonly_store_cannot_turn_external_barrier_into_index_maintenance_permit() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("readonly-permit");
    Store::create(&root).unwrap();
    let readonly = Store::open(&root, OpenMode::ForceReadOnly).unwrap();
    let barrier = mem_core::lock::barrier_exclusive(&readonly).unwrap();

    assert!(matches!(
        readonly.index_maintenance_from_guard(barrier),
        Err(mem_core::error::MemError::ReadOnly)
    ));
    assert!(!root.join("index/index.sqlite").exists());
}

#[test]
#[cfg(debug_assertions)]
fn rust_store_create_does_not_publish_openable_config_before_layout_is_ready() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("atomic-create");
    let permit = Store::acquire_init_lock(&root).unwrap();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (continue_tx, continue_rx) = std::sync::mpsc::channel();
    let root_for_thread = root.clone();
    let create = std::thread::spawn(move || {
        Store::create_under_init_lock_test_with_pause(&root_for_thread, &permit, || {
            ready_tx.send(()).unwrap();
            continue_rx.recv().unwrap();
        })
        .unwrap();
    });

    ready_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    assert!(
        !root.join("mem.json").exists(),
        "配置是可打开状态的发布点，布局尚未完成时不应存在"
    );
    assert!(matches!(
        Store::open(&root, OpenMode::Auto),
        Err(mem_core::error::MemError::Corrupt { .. })
    ));
    continue_tx.send(()).unwrap();
    create.join().unwrap();
    assert!(Store::open(&root, OpenMode::Auto).is_ok());

    let interrupted = tmp.path().join("interrupted-create");
    let first_permit = Store::acquire_init_lock(&interrupted).unwrap();
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        Store::create_under_init_lock_test_with_pause(&interrupted, &first_permit, || {
            panic!("simulate process interruption before config publication");
        })
        .unwrap();
    }));
    assert!(failed.is_err());
    drop(first_permit);
    assert!(!interrupted.join("mem.json").exists());
    let retry_permit = Store::acquire_init_lock(&interrupted).unwrap();
    Store::create_under_init_lock(&interrupted, &retry_permit).unwrap();
    assert!(Store::open(&interrupted, OpenMode::Auto).is_ok());
}

#[test]
fn index_maintenance_permit_cannot_be_rebound_to_another_store() {
    let tmp = tempfile::tempdir().unwrap();
    let first_root = tmp.path().join("first");
    let second_root = tmp.path().join("second");
    Store::create(&first_root).unwrap();
    Store::create(&second_root).unwrap();
    let first = Store::open(&first_root, OpenMode::Auto).unwrap();
    let second = Store::open(&second_root, OpenMode::Auto).unwrap();
    let permit = first.acquire_index_maintenance().unwrap();

    assert!(matches!(
        permit.check_store(&second),
        Err(mem_core::error::MemError::BadArgument(_))
    ));
    let checkpoint = mem_core::freshness::Checkpoint {
        format_version: second.config().format_version,
        ts: mem_core::clock::now_rfc3339(),
        entries: Default::default(),
        index_epoch: 1,
    };
    assert!(mem_core::freshness::save_checkpoint(&second, &checkpoint, &permit).is_err());
    assert!(!mem_core::freshness::checkpoint_path(&second).exists());
    drop(permit);
    let barrier = mem_core::lock::barrier_exclusive(&first).unwrap();
    assert!(matches!(
        second.index_maintenance_from_guard(barrier),
        Err(mem_core::error::MemError::BadArgument(_))
    ));
}

#[test]
fn reconcile_refuses_git_byte_policy_conflict_before_any_mutation_or_callback() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("git-policy-conflict");
    Store::create(&root).unwrap();
    std::fs::create_dir(root.join(".git")).unwrap();
    std::fs::write(root.join(".gitattributes"), b"*.md text=auto\n").unwrap();
    std::fs::create_dir_all(root.join(".pending-commits")).unwrap();
    std::fs::write(root.join(".pending-commits/keep.json"), b"pending").unwrap();
    let store = Store::open(&root, OpenMode::Auto).unwrap();
    let before_events = std::fs::read(root.join("events.jsonl")).unwrap();
    let before_pending = std::fs::read(root.join(".pending-commits/keep.json")).unwrap();
    let callbacks_called = std::sync::atomic::AtomicBool::new(false);
    let permit = store.acquire_index_maintenance().unwrap();

    let result = mem_core::sidecar::reconcile_under_maintenance(
        &store,
        &permit,
        Some(&|_, _| {
            callbacks_called.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(mem_core::freshness::MaintainOutcome {
                index_epoch: 1,
                scan_entries: Default::default(),
            })
        }),
        || {
            callbacks_called.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(0)
        },
    );

    assert!(matches!(
        result,
        Err(mem_core::error::MemError::GitAttributesConflict { .. })
    ));
    assert!(!callbacks_called.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(
        std::fs::read(root.join("events.jsonl")).unwrap(),
        before_events
    );
    assert_eq!(
        std::fs::read(root.join(".pending-commits/keep.json")).unwrap(),
        before_pending
    );
    assert!(!root.join("index/index.sqlite").exists());
}

#[test]
fn config_cannot_mutate_compatibility_fields_through_generic_setter() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("store");
    Store::create(&root).unwrap();
    let store = Store::open(&root, OpenMode::Auto).unwrap();
    let path = store.abs("mem.json");
    let before = std::fs::read(&path).unwrap();

    for key in ["format_version", "required_features"] {
        let error = store
            .set_config_value(key, serde_json::json!("bypass"))
            .unwrap_err();
        assert!(matches!(error, mem_core::error::MemError::BadArgument(_)));
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}

#[test]
fn store_readonly_mode_rejects_writes_without_touching_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("store");
    Store::create(&root).unwrap();
    let store = Store::open(&root, OpenMode::ForceReadOnly).unwrap();
    assert!(!store.writable());
    let err = store.check_writable().unwrap_err();
    assert!(err.to_string().contains("只读"), "{err}");
}

#[test]
fn store_detects_unwritable_volume_without_writing_truth() {
    // 自然探测：.tmp 无法创建（此处用同名普通文件占位模拟不可写）→ writable=false。
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("store");
    Store::create(&root).unwrap();
    std::fs::remove_dir_all(root.join(".tmp")).unwrap();
    std::fs::write(root.join(".tmp"), b"not a dir").unwrap();
    let store = Store::open(&root, OpenMode::Auto).unwrap();
    assert!(!store.writable());
}

// ---------- platform 原语 ----------

#[test]
fn platform_process_alive_and_atomic_write() {
    assert!(mem_core::platform::process_alive(std::process::id()));
    assert!(
        !mem_core::platform::process_alive(u32::MAX - 7),
        "不存在/不可查询的 PID 判死"
    );

    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("f.md");
    mem_core::platform::atomic_write(&target, b"one").unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"one");
    mem_core::platform::atomic_write(&target, b"two").unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"two", "原子覆盖");
    // 成功后 .tmp 无残留（崩溃原子：temp 只在写期间存在）。
}

#[test]
fn platform_lock_file_exclusive_smoke() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("barrier.lock");
    let g1 = mem_core::platform::LockFile::acquire_exclusive(&f).expect("第一把独占");
    let r2 = mem_core::platform::LockFile::try_acquire_exclusive(&f);
    assert!(r2.is_err(), "同进程重入独占必须失败（字节范围锁语义）");
    drop(g1);
    let g3 = mem_core::platform::LockFile::try_acquire_exclusive(&f).expect("释放后可再取");
    drop(g3);
    // 共享锁：两把共享可共存。
    let s1 = mem_core::platform::LockFile::try_acquire_shared(&f).expect("s1");
    let s2 = mem_core::platform::LockFile::try_acquire_shared(&f).expect("s2");
    drop((s1, s2));
    let x = mem_core::platform::LockFile::try_acquire_exclusive(&f).expect("共享释放后独占可取");
    drop(x);
}

#[test]
fn platform_create_new_is_atomic_exclusive() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("new.md");
    assert!(mem_core::platform::create_new(&f, b"first").is_ok());
    assert!(
        mem_core::platform::create_new(&f, b"second").is_err(),
        "R3.3：原子独占创建"
    );
    assert_eq!(std::fs::read(&f).unwrap(), b"first");
}

#[test]
fn init_permit_cannot_create_or_seed_a_different_store() {
    let tmp = tempfile::tempdir().unwrap();
    let authorized = tmp.path().join("authorized");
    let other = tmp.path().join("other");
    let permit = Store::acquire_init_lock(&authorized).unwrap();

    let err = Store::create_under_init_lock(&other, &permit).unwrap_err();
    assert!(matches!(err, mem_core::error::MemError::BadArgument(_)));
    let err = mem_core::gitattrs::seed_at_root(&other, &permit).unwrap_err();
    assert!(matches!(err, mem_core::error::MemError::BadArgument(_)));
    assert!(!other.exists());
}

#[test]
fn init_permit_cannot_seed_an_existing_store_outside_bootstrap() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("existing");
    Store::create(&root).unwrap();
    std::fs::write(root.join(".gitattributes"), b"custom\n").unwrap();
    let before = snapshot_tree(&root);
    let permit = Store::acquire_init_lock(&root).unwrap();
    let err = mem_core::gitattrs::seed_at_root(&root, &permit).unwrap_err();
    assert!(matches!(err, mem_core::error::MemError::BadArgument(_)));
    assert_eq!(snapshot_tree(&root), before);
}

#[test]
fn maintenance_permit_rejects_read_only_target_handle_for_same_store() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("store");
    Store::create(&root).unwrap();
    let writable = Store::open(&root, OpenMode::Auto).unwrap();
    let readonly = Store::open(&root, OpenMode::ForceReadOnly).unwrap();
    let permit = writable.acquire_index_maintenance().unwrap();
    assert!(matches!(
        permit.check_store(&readonly),
        Err(mem_core::error::MemError::ReadOnly)
    ));
    let before = snapshot_tree(&root);
    let err =
        mem_core::freshness::save_checkpoint(&readonly, &Default::default(), &permit).unwrap_err();
    assert!(err.to_string().contains("只读"));
    assert_eq!(snapshot_tree(&root), before);
}

#[test]
fn ordinary_open_rejects_interrupted_cli_initialization_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("store");
    Store::create(&root).unwrap();
    std::fs::write(root.join(".init-in-progress"), b"incomplete").unwrap();

    let err = Store::open(&root, OpenMode::Auto)
        .err()
        .expect("半成品初始化不得作为可用数据店打开");
    assert!(
        matches!(err, mem_core::error::MemError::MigrationState(_)),
        "{err}"
    );
    let permit = Store::acquire_init_lock(&root).unwrap();
    assert!(
        Store::open_under_init_lock(&root, OpenMode::Auto, &permit).is_ok(),
        "持初始化许可的自举路径必须能读取当前店状态"
    );
}

#[test]
fn migration_refuses_interrupted_cli_initialization_marker_without_mutation() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("store");
    Store::create(&root).unwrap();
    let config_path = root.join("mem.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    config["format_version"] = serde_json::json!(1);
    config.as_object_mut().unwrap().remove("required_features");
    std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    let before = std::fs::read(&config_path).unwrap();
    std::fs::write(root.join(".init-in-progress"), b"interrupted").unwrap();

    let err = Store::migrate_format_v1(&root, "backup-id").unwrap_err();
    assert!(
        matches!(err, mem_core::error::MemError::MigrationState(_)),
        "{err}"
    );
    assert_eq!(std::fs::read(config_path).unwrap(), before);
}
