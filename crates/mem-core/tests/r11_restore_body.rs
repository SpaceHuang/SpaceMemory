//! R11 恢复通道核心测试（BD4；验收 6 的机制半边——发现半边由 deep_digest_audit
//! 既测试面覆盖）。窗口语义、来源三形、响亮失败与回显契约逐条锁定。

use std::collections::HashMap;

use mem_core::error::MemError;
use mem_core::event::{EventLog, Op};
use mem_core::ops::{self, AddArgs, RestoreSource, WriteContext};
use mem_core::store::{OpenMode, Store};
use mem_core::token::{self, Token};
use mem_schema::TypeSet;

fn fresh_store() -> (tempfile::TempDir, Store, TypeSet) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("store");
    Store::create(&root).unwrap();
    // .git 形态标记（history/git 源窗口判定的前提；fake 通道绕过真实 git 子进程）。
    std::fs::create_dir_all(root.join(".git")).unwrap();
    let types_dir = root.join("types");
    std::fs::create_dir_all(types_dir.join("memory")).unwrap();
    std::fs::write(
        types_dir.join("memory/schema.json"),
        r#"{
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object", "x-grade": "strict", "x-unique-key": [],
        "properties": { "title": { "type": "string", "minLength": 1 } },
        "required": ["title"]
      }"#,
    )
    .unwrap();
    let store = Store::open(&root, OpenMode::Auto).unwrap();
    let types = TypeSet::load_dir(&types_dir).unwrap();
    (tmp, store, types)
}

/// 无 .git 形态（红线10：`.git` 是附属物；git 缺席的店合法）。
fn fresh_store_nogit() -> (tempfile::TempDir, Store, TypeSet) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("store");
    Store::create(&root).unwrap();
    let types_dir = root.join("types");
    std::fs::create_dir_all(types_dir.join("memory")).unwrap();
    std::fs::write(
        types_dir.join("memory/schema.json"),
        r#"{
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object", "x-grade": "strict", "x-unique-key": [],
        "properties": { "title": { "type": "string", "minLength": 1 } },
        "required": ["title"]
      }"#,
    )
    .unwrap();
    let store = Store::open(&root, OpenMode::Auto).unwrap();
    let types = TypeSet::load_dir(&types_dir).unwrap();
    (tmp, store, types)
}

fn ctx<'a>(store: &'a Store, types: &'a TypeSet) -> WriteContext<'a> {
    WriteContext {
        store,
        types,
        by: "test".into(),
        committer: None,
        session_id: None,
    }
}

fn add_memory(store: &Store, types: &TypeSet, title: &str, body: &[u8]) -> (String, Token) {
    let out = ops::add(
        &ctx(store, types),
        AddArgs {
            type_name: "memory".into(),
            payload: serde_json::json!({"title": title}),
            body: body.to_vec(),
            principal: None,
        },
    )
    .unwrap();
    (out.record.envelope.id.clone(), out.token)
}

/// 外部改动正文字节（翻转一个字节；frontmatter 不动）。
fn corrupt_body(store: &Store, rel: &str) -> Vec<u8> {
    let mut bytes = std::fs::read(store.abs(rel)).unwrap();
    // 正文 = 文件尾部（首个闭合分隔符行之后）→ 起点 = 文件长 - 正文长。
    let body = mem_core::record::find_body_after_first_delimiter(&bytes).expect("损坏前可提取正文");
    let body_start = bytes.len() - body.len();
    bytes[body_start] ^= 0xFF;
    std::fs::write(store.abs(rel), &bytes).unwrap();
    bytes
}

/// fake 历史通道：rel+rev → 文件字节。
struct FakeHistory {
    files: HashMap<(String, String), Vec<u8>>,
    versions: HashMap<String, Vec<String>>,
}

impl FakeHistory {
    fn with_version(rel: &str, rev: &str, bytes: &[u8]) -> Self {
        let mut h = Self { files: HashMap::new(), versions: HashMap::new() };
        h.files.insert((rel.to_string(), rev.to_string()), bytes.to_vec());
        h.versions.insert(rel.to_string(), vec![rev.to_string()]);
        h
    }
}

impl ops::BodyHistory for FakeHistory {
    fn file_bytes_at(&self, rel: &str, rev: &str) -> mem_core::error::MemResult<Option<Vec<u8>>> {
        Ok(self.files.get(&(rel.to_string(), rev.to_string())).cloned())
    }
    fn list_versions(&self, rel: &str) -> mem_core::error::MemResult<Vec<String>> {
        Ok(self.versions.get(rel).cloned().unwrap_or_default())
    }
}

// ---------- 正向：history 源（窗口内） ----------

#[test]
fn history_source_restores_bytes_in_window() {
    let (tmp, store, types) = fresh_store();
    let orig_body = b"original body bytes";
    let (id, _tok) = add_memory(&store, &types, "restore-me", orig_body);
    let hit = ops::get(&store, &id).unwrap();
    let rel = hit.rel.clone();
    // 损坏前的整文件字节 = 机制写入并随写路径同批提交进 git 的形态（常态锚）。
    let pristine_bytes = std::fs::read(store.abs(&rel)).unwrap();
    let anchor = token::sha256_hex(&pristine_bytes);
    let corrupted = corrupt_body(&store, &rel);
    // 发现半边：verify --deep 报漂移（既有审计，追认为验收承诺）。
    let audit = ops::deep_digest_audit(&store).unwrap();
    assert!(audit.iter().any(|l| l.contains(&rel)), "deep 审计报出漂移：{audit:?}");

    // 恢复：history 自动锚定（窗口内：漂移仍在 ∧ 最后事件为机制写入登记）。
    let fake = FakeHistory::with_version(&rel, "sha-1", &pristine_bytes);
    let expect = token::for_bytes(hit.record.envelope.version, &corrupted);
    let out = ops::restore_body(&ctx(&store, &types), &id, &expect, &RestoreSource::History, Some(&fake))
        .expect("窗口内 history 锚定成功");
    // 正文 == 所选来源（哈希相等）。
    assert_eq!(out.record.body, orig_body, "恢复后正文 == 来源正文区");
    assert_eq!(out.token.version, 2, "版本 +1");
    // 信封/载荷原样保留。
    assert_eq!(out.record.envelope.id, id);
    assert_eq!(out.record.envelope.r#type, "memory");
    assert_eq!(out.record.payload, serde_json::json!({"title": "restore-me"}));
    // 事件留痕：op=restore_body，前后全文摘要。
    let last = EventLog::new(&store).last_event_for(&id).unwrap().unwrap();
    assert_eq!(last.op, Op::RestoreBody);
    assert_eq!(last.old_digest, Some(token::sha256_hex(&corrupted)));
    assert_eq!(last.new_digest, Some(out.token.digest.clone()));
    // 来源出处回显：形态、锚定摘要、版本、复制前后摘要。
    assert_eq!(out.source_echo["form"], "history");
    assert_eq!(out.source_echo["anchor"], anchor);
    assert_eq!(out.source_echo["commit"], "sha-1");
    assert_eq!(out.source_echo["body_sha256_source"], token::sha256_hex(orig_body));
    assert_eq!(out.source_echo["body_sha256_written"], token::sha256_hex(orig_body));
    // 复跑 --deep：该记录不再漂移。
    let audit2 = ops::deep_digest_audit(&store).unwrap();
    assert!(!audit2.iter().any(|l| l.contains(&rel)), "恢复后深档审计收敛：{audit2:?}");
    let _ = tmp;
}

// ---------- 反例（验收 6 ia）：回填登记损坏字节后 history 必须响亮拒绝 ----------

#[test]
fn history_source_rejects_after_backfill_registered_corruption() {
    let (tmp, store, types) = fresh_store();
    let (id, _tok) = add_memory(&store, &types, "backfilled", b"body v1");
    let hit = ops::get(&store, &id).unwrap();
    let rel = hit.rel.clone();
    let pristine = std::fs::read(store.abs(&rel)).unwrap();
    let corrupted = corrupt_body(&store, &rel);
    // reconcile：回填把损坏字节登记为最后事件（外部改动转正）。
    let report = mem_core::event::reconcile_backfill(&store, "mem", None).unwrap();
    assert_eq!(report.appended.len(), 1, "回填登记损坏字节");
    // history 必须**响亮拒绝**——不得把盘上（损坏）字节写成新版本冒充治愈。
    let fake = FakeHistory::with_version(&rel, "sha-1", &pristine);
    let expect = token::for_bytes(hit.record.envelope.version, &corrupted);
    let err = ops::restore_body(&ctx(&store, &types), &id, &expect, &RestoreSource::History, Some(&fake))
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("拒绝"), "响亮拒绝：{msg}");
    // 指引含最后**机制写入**登记摘要（Create 事件的登记摘要）。
    assert!(msg.contains(&token::sha256_hex(&pristine)), "指引含最后机制写入登记摘要：{msg}");
    // 未落任何新版本（盘上字节未变）。
    assert_eq!(std::fs::read(store.abs(&rel)).unwrap(), corrupted, "窗口外拒绝不写盘");
    let _ = tmp;
}

// ---------- 反例（验收 6 ib）：合法更新收敛后 history 同样拒绝 ----------

#[test]
fn history_source_rejects_when_drift_converged_by_legit_update() {
    let (tmp, store, types) = fresh_store();
    let (id, tok) = add_memory(&store, &types, "converged", b"body v1");
    let rel = ops::get(&store, &id).unwrap().rel.clone();
    let pristine = std::fs::read(store.abs(&rel)).unwrap();
    let corrupted = corrupt_body(&store, &rel);
    // 合法载荷更新（正文保持）→ 最后登记 = 盘上字节（漂移收敛）。
    // 翻转字节必改全文件摘要 → 用盘上（损坏）字节构造当前令牌。
    let corrupted_tok = token::for_bytes(1, &corrupted);
    assert_ne!(corrupted_tok.digest, tok.digest, "损坏即令牌失效（红线19）");
    let _ = ops::update(
        &ctx(&store, &types),
        &id,
        &corrupted_tok,
        serde_json::json!({"title": "converged-v2"}),
        None,
    )
    .unwrap();
    let fake = FakeHistory::with_version(&rel, "sha-1", &pristine);
    let hit = ops::get(&store, &id).unwrap();
    let err = ops::restore_body(&ctx(&store, &types), &id, &hit.token, &RestoreSource::History, Some(&fake))
        .unwrap_err();
    assert!(err.to_string().contains("拒绝"), "漂移已收敛 → 响亮拒绝：{err}");
    let _ = tmp;
}

// ---------- 显式来源：git:<版本> 与 file:<店外路径> ----------

#[test]
fn git_version_source_restores_explicitly() {
    let (tmp, store, types) = fresh_store();
    let (id, _tok) = add_memory(&store, &types, "explicit-git", b"body v1");
    let rel = ops::get(&store, &id).unwrap().rel.clone();
    let pristine = std::fs::read(store.abs(&rel)).unwrap();
    let corrupted = corrupt_body(&store, &rel);
    // 显式 git:<版本>：不受窗口约束（调用方显式选择承担来源可信性）。
    let fake = FakeHistory::with_version(&rel, "abc123", &pristine);
    let expect = token::for_bytes(1, &corrupted);
    let out = ops::restore_body(
        &ctx(&store, &types),
        &id,
        &expect,
        &RestoreSource::Git { rev: "abc123".into() },
        Some(&fake),
    )
    .unwrap();
    assert_eq!(out.record.body, b"body v1");
    assert_eq!(out.source_echo["form"], "git");
    assert_eq!(out.source_echo["rev"], "abc123");
    let _ = tmp;
}

#[test]
fn file_source_restores_from_outside_backup_and_rejects_store_paths() {
    let (tmp, store, types) = fresh_store();
    let (id, _tok) = add_memory(&store, &types, "file-src", b"body v1");
    let rel = ops::get(&store, &id).unwrap().rel.clone();
    let pristine = std::fs::read(store.abs(&rel)).unwrap();
    let corrupted = corrupt_body(&store, &rel);
    // 店内路径 → 响亮拒绝（防旁挂写入；先做——成功恢复会推进版本使令牌失效）。
    let inside = store.abs(&rel);
    let expect = token::for_bytes(1, &corrupted);
    let err = ops::restore_body(
        &ctx(&store, &types),
        &id,
        &expect,
        &RestoreSource::File { path: inside },
        None,
    )
    .unwrap_err();
    assert!(err.to_string().contains("店外"), "{err}");
    // 店外备份（记录格式文件，取其正文区）。
    let backup = tmp.path().join("backup.md");
    std::fs::write(&backup, &pristine).unwrap();
    let out = ops::restore_body(
        &ctx(&store, &types),
        &id,
        &expect,
        &RestoreSource::File { path: backup.clone() },
        None,
    )
    .unwrap();
    assert_eq!(out.record.body, b"body v1");
    assert_eq!(out.source_echo["form"], "file");
}

// ---------- 无 .git：history/git 源整体结构化报错，file: 仍可用 ----------

#[test]
fn git_absent_store_structured_error_for_git_sources() {
    // 无 .git 形态（红线10：.git 是附属物；git 缺席的店合法）。
    let (tmp, store, types) = fresh_store_nogit();
    let (id, _tok) = add_memory(&store, &types, "nogit", b"body v1");
    let rel = ops::get(&store, &id).unwrap().rel.clone();
    let corrupted = corrupt_body(&store, &rel);
    let expect = token::for_bytes(1, &corrupted);
    // fake 通道在场（不会被触及）——.git 缺席在窗口判定之前整体结构化报错。
    let fake = FakeHistory::with_version(&rel, "sha-1", b"anything");
    let e1 = ops::restore_body(
        &ctx(&store, &types),
        &id,
        &expect,
        &RestoreSource::History,
        Some(&fake),
    )
    .unwrap_err();
    assert!(e1.to_string().contains(".git"), "{e1}");
    let e2 = ops::restore_body(
        &ctx(&store, &types),
        &id,
        &expect,
        &RestoreSource::Git { rev: "HEAD".into() },
        Some(&fake),
    )
    .unwrap_err();
    assert!(e2.to_string().contains(".git"), "{e2}");
    // file: 源仍可用。
    let backup = tmp.path().join("b.md");
    std::fs::write(&backup, b"---\n{\"envelope\":{\"created\":\"t\",\"id\":\"x\",\"modified\":\"t\",\"refs\":[],\"type\":\"memory\",\"version\":1},\"payload\":{\"title\":\"t\"}}\n---\nrecovered body").unwrap();
    let out = ops::restore_body(
        &ctx(&store, &types),
        &id,
        &expect,
        &RestoreSource::File { path: backup },
        None,
    )
    .unwrap();
    assert_eq!(out.record.body, b"recovered body");
}

// ---------- 锚无匹配：响亮失败、区分成因、绝不回退次优匹配 ----------

#[test]
fn anchor_not_found_loud_failure_distinguishes_causes() {
    let (_tmp, store, types) = fresh_store();
    let (id, _tok) = add_memory(&store, &types, "nomatch", b"body v1");
    let rel = ops::get(&store, &id).unwrap().rel.clone();
    let corrupted = corrupt_body(&store, &rel);
    // 历史在但**无匹配版本**（字节摘要 ≠ 锚）。
    let fake = FakeHistory::with_version(&rel, "sha-1", b"totally different bytes");
    let expect = token::for_bytes(1, &corrupted);
    let err = ops::restore_body(&ctx(&store, &types), &id, &expect, &RestoreSource::History, Some(&fake))
        .unwrap_err();
    assert!(err.to_string().contains("未在 git 历史中找到"), "{err}");
    // commit-pending 在场 → 成因指明"等待收敛后重试"。
    let pdir = store.abs(".pending-commits");
    std::fs::create_dir_all(&pdir).unwrap();
    std::fs::write(pdir.join("1.json"), b"{\"paths\":[],\"ts\":\"t\",\"error\":\"e\"}").unwrap();
    let err2 = ops::restore_body(&ctx(&store, &types), &id, &expect, &RestoreSource::History, Some(&fake))
        .unwrap_err();
    assert!(err2.to_string().contains("commit-pending"), "{err2}");
}

// ---------- 并发与形态守卫 ----------

#[test]
fn cas_conflict_rejected_and_file_untouched() {
    let (tmp, store, types) = fresh_store();
    let (id, _tok) = add_memory(&store, &types, "cas", b"body v1");
    let rel = ops::get(&store, &id).unwrap().rel.clone();
    let before = std::fs::read(store.abs(&rel)).unwrap();
    let wrong = Token { version: 1, digest: "0".repeat(64) };
    let err = ops::restore_body(
        &ctx(&store, &types),
        &id,
        &wrong,
        &RestoreSource::File { path: tmp.path().join("nope.md") },
        None,
    )
    .unwrap_err();
    assert!(matches!(err, MemError::Conflict { .. }), "{err}");
    assert_eq!(std::fs::read(store.abs(&rel)).unwrap(), before, "CAS 失败不写盘");
}

#[test]
fn attic_record_rejected() {
    let (_tmp, store, types) = fresh_store();
    let (id, tok) = add_memory(&store, &types, "gone", b"body v1");
    ops::delete_record(&ctx(&store, &types), &id, &tok, true).unwrap();
    let err = ops::restore_body(
        &ctx(&store, &types),
        &id,
        &tok,
        &RestoreSource::History,
        None,
    )
    .unwrap_err();
    assert!(err.to_string().contains("records/"), "attic 记录不支持 restore-body：{err}");
}
