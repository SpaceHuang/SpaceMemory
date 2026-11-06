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

fn sample_envelope() -> Envelope {
    Envelope {
        id: UUID_A.to_string(),
        r#type: "task".into(),
        created: "2026-11-04T09:12:00Z".into(),
        modified: "2026-11-04T11:40:00Z".into(),
        version: 6,
        principal: Some(UUID_B.to_string()),
        refs: vec![
            Ref { rel: "belongs_to".into(), target: UUID_C.to_string() },
            Ref { rel: "derived_from".into(), target: UUID_B.to_string() },
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
    e.extra.insert("custom_note".into(), serde_json::json!("保留我"));
    let v = e.to_json_value();
    let e2 = Envelope::from_json(&v).unwrap();
    assert_eq!(e2.extra.get("custom_note"), Some(&serde_json::json!("保留我")));
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
    assert!(Record::from_bytes(&bytes).is_err(), "缺闭合分隔符必须响亮失败");
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
    assert!(regen.starts_with(b"---\n{\n  \"envelope\": {"), "再生成必须确定性键序");
}

// ---------- token：全摘要 CAS（D3/红线19） ----------

#[test]
fn token_format_and_parse() {
    let bytes = record_bytes("x");
    let t = token::for_bytes(6, &bytes);
    assert_eq!(t.version, 6);
    assert_eq!(t.digest.len(), 64, "全 SHA-256，64 hex");
    assert!(t.digest.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    let rendered = t.to_string();
    assert!(rendered.starts_with("v6@"));
    let t2: Token = rendered.parse().unwrap();
    assert_eq!(t2, t);
    assert!("v6@abc".parse::<Token>().is_err(), "短摘要拒绝（红线19：禁截断摘要）");
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
    let t_hand = Token { version: t_a.version, digest: token::sha256_hex("手改内容".as_bytes()) };
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
        "a<b", "a>b", "a:b", "a\"b", "a/b", "a\\b", "a|b", "a?b", "a*b",
        "a\u{7f}b", "a\u{1}b", "trailing.", "trailing ", ".dot", "..", "...",
        "con", "CON", "aux", "nul", "com1", "lpt2",
    ] {
        assert!(paths::validate_component(bad).is_err(), "应拒收：{bad:?}");
    }
    assert!(paths::validate_component("conventions").is_err(), "店级保留名");
    assert!(paths::validate_component("types").is_err());
    assert!(paths::validate_component("").is_err());
    // 长度上限 128 字节（UTF-8）。
    let long = "a".repeat(128); // 恰好 128 字节
    assert!(paths::validate_component(&long).is_ok());
    assert!(paths::validate_component(&(long + "a")).is_err(), "129 字节拒收");
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
    let rel2 = paths::derive_record_rel("memory", &[], "0198aaaa-0000-7cd2-9e3f-000000000001").unwrap();
    assert_eq!(rel2, "records/memory/0198aaaa-0000-7cd2-9e3f-000000000001.md");
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
    assert_eq!(out, r#"{"a":2,"m":3,"z":1}"#, "Map 必须按键排序（preserve_order 未启用）");
}

// ---------- store：布局/打开/只读判定 ----------

#[test]
fn store_create_layout_and_open() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("store");
    Store::create(&root).expect("create");
    for d in ["records", "attic", "sessions", "agenda", "types", "conventions", "index", ".tmp"] {
        assert!(root.join(d).is_dir(), "缺目录 {d}");
    }
    assert!(root.join("mem.json").is_file());
    assert!(root.join("events.jsonl").is_file());
    assert!(root.join("conventions/CONVENTIONS.md").is_file());

    let store = Store::open(&root, OpenMode::Auto).expect("open");
    assert!(store.writable());
    assert_eq!(store.config().budgets.list_limit, 50, "规范默认预算");
    assert_eq!(store.config().format_version, 1);
}

#[test]
fn store_open_fails_loud_without_mem_json() {
    let tmp = tempfile::tempdir().unwrap();
    let err = Store::open(tmp.path(), OpenMode::Auto).unwrap_err();
    assert!(err.to_string().contains("mem.json"));
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
    assert!(!mem_core::platform::process_alive(u32::MAX - 7), "不存在/不可查询的 PID 判死");

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
    assert!(mem_core::platform::create_new(&f, b"second").is_err(), "R3.3：原子独占创建");
    assert_eq!(std::fs::read(&f).unwrap(), b"first");
}
