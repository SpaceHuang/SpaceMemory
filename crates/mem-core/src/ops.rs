//! 机制层操作：add / update / link / delete / restore / merge / get / scan。
//!
//! 写路径九步（D3）：共享门闩 → 记录锁 → 全摘要令牌 CAS → 类型声明校验 →
//! temp+原子改名 → 序列锁内追加事件 → git 同批提交（记录路径 + events.jsonl，
//! 失败转 commit-pending 不回滚）→ 释放记录锁（顺带登记台账 trail）→ 释放门闩。
//!
//! 引用纪律：payload 的 x-ref 字段（非 null）由机制**镜像**进 envelope.refs
//! （rel = 字段名）——跨类型通用操作（过滤/级联/反向索引）只依赖信封（R8.4），
//! 镜像在 add/update 内同步维护，payload 是唯一写源。

use serde::{Deserialize, Serialize};

use crate::envelope::{Envelope, Ref, new_uuid_v7};
use crate::error::{MemError, MemResult};
use crate::event::{EventLog, Kind, Op};
use crate::lock;
use crate::paths;
use crate::record::{self, Record};
use crate::session;
use crate::store::Store;
use crate::token::{self, Token};
use mem_schema::{TypeDecl, TypeSet};

/// git 提交汇（D4/D17）：路径集合一次提交；mem-git 提供实现。
pub trait CommitSink: Send + Sync {
    /// Ok(Some(sha)) 成功；Err → commit-pending（真相层不回滚）。
    fn commit(&self, paths: &[String]) -> MemResult<Option<String>>;
}

/// 历史读取（fix 流：git 历史取上一版 frontmatter，D18/R6.3）。
pub trait HistoryStore: Send + Sync {
    /// HEAD 中该相对路径的上一版内容；无历史 → Ok(None)。
    fn previous_file(&self, rel: &str) -> MemResult<Option<Vec<u8>>>;
}

/// restore_body 的只读历史通道（R11/BD4 来源三形之 `git:<版本>` 与 `history`；
/// mem-git 实现，测试用 fake）。机制保证与来源可信性分离：本 trait 只负责
/// "按版本取字节/列版本"——可信性由调用方显式选择承担（§3.6 责任分界）。
pub trait BodyHistory: Send + Sync {
    /// 指定版本（完整 SHA 或引用）该相对路径的完整文件字节；路径在该版本不存在
    /// → Ok(None)；坏版本引用/无 .git → Err（响亮，错误信息区分成因）。
    fn file_bytes_at(&self, rel: &str, rev: &str) -> MemResult<Option<Vec<u8>>>;
    /// 按路径列历史版本（自新至旧的完整 SHA 列表）；无 .git → Err。
    fn list_versions(&self, rel: &str) -> MemResult<Vec<String>>;
}

/// 写路径上下文。
#[non_exhaustive]
pub struct WriteContext<'a> {
    pub store: &'a Store,
    pub types: &'a TypeSet,
    /// 操作者归属（"principal-id(label)"；机制回填为 "mem"）。
    pub by: String,
    pub committer: Option<&'a dyn CommitSink>,
    /// 会话台账 id（写路径 trail 兜底登记）。
    pub session_id: Option<&'a str>,
}

impl<'a> WriteContext<'a> {
    /// 建立下游受管写上下文；附加 Git 汇总和会话轨迹使用 builder 方法。
    pub fn new(store: &'a Store, types: &'a TypeSet, by: impl Into<String>) -> Self {
        Self {
            store,
            types,
            by: by.into(),
            committer: None,
            session_id: None,
        }
    }

    pub fn with_committer(mut self, committer: Option<&'a dyn CommitSink>) -> Self {
        self.committer = committer;
        self
    }

    pub fn with_session_id(mut self, session_id: Option<&'a str>) -> Self {
        self.session_id = session_id;
        self
    }
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Scanned {
    pub rel: String,
    pub record: Record,
    pub token: Token,
}

#[derive(Debug, Default)]
#[non_exhaustive]
pub struct ScanResult {
    pub records: Vec<Scanned>,
    /// 结构化坏记录（rel, 告警行）——同一次扫描的坏条目与不可读文件（R10.3 决策⑦）。
    /// 告警行 = "跳过坏条目 {rel}：{原因}"（修复入口由检索/清单响应层追加）；
    /// 与 `warnings` 同源双视图：bad 供排序/截断/索引落库，warnings 供 verify 等人读面。
    pub bad: Vec<(String, String)>,
    pub warnings: Vec<String>,
}

impl ScanResult {
    fn push_bad(&mut self, rel: String, line: String) {
        self.bad.push((rel, line.clone()));
        self.warnings.push(line);
    }
}

/// 直读扫描 records/（读路径零写盘；R6.2：坏条目跳过 + 告警）。
pub fn scan_all(store: &Store) -> MemResult<ScanResult> {
    scan_all_with_hook(store, None)
}

fn scan_all_with_hook(
    store: &Store,
    after_record: Option<&dyn Fn(&Scanned)>,
) -> MemResult<ScanResult> {
    store.read_compatible_missing_features()?;
    let mut out = ScanResult::default();
    for rel in crate::event::enumerate_md(store, "records")? {
        let bytes = match std::fs::read(store.abs(&rel)) {
            Ok(b) => b,
            Err(e) => {
                out.push_bad(rel.clone(), format!("跳过不可读 {rel}：{e}"));
                continue;
            }
        };
        match Record::from_bytes(&bytes) {
            Ok(rec) => {
                out.records.push(Scanned {
                    rel,
                    token: token::for_bytes(rec.envelope.version, &bytes),
                    record: rec,
                });
                if let (Some(hook), Some(scanned)) = (after_record, out.records.last()) {
                    hook(scanned);
                }
            }
            Err(e) => out.push_bad(rel.clone(), format!("跳过坏条目 {rel}：{e}")),
        }
    }
    store.read_compatible_missing_features()?;
    Ok(out)
}

/// 仅供 debug 集成测试在逐文件扫描的可控交错点执行并发写；不进入 release 构建。
#[doc(hidden)]
#[cfg(all(debug_assertions, not(test)))]
pub fn scan_all_with_hook_test(
    store: &Store,
    after_record: impl Fn(&Scanned),
) -> MemResult<ScanResult> {
    scan_all_with_hook(store, Some(&after_record))
}

/// 按 id / 别名 / 路径 解析记录（读路径；存在性含 attic 的读取通道）。
pub fn get(store: &Store, id_or_path: &str) -> MemResult<Scanned> {
    store.read_compatible_missing_features()?;
    let result = get_unchecked_read_compatibility(store, id_or_path);
    store.read_compatible_missing_features()?;
    result
}

fn get_unchecked_read_compatibility(store: &Store, id_or_path: &str) -> MemResult<Scanned> {
    if id_or_path.starts_with("records/") || id_or_path.starts_with("attic/") {
        paths::validate_store_record_path(store, id_or_path)?;
        // r4 H1：**单次读取**同时派生 Record 与 Token——两次磁盘读取会把"版本 A
        // 的内容 + 版本 B 的令牌"拼在一起，调用方按 A 构造更新却携 B 令牌会绕过
        // CAS 语义覆盖 B。
        let abs = store.abs(id_or_path);
        let bytes = std::fs::read(&abs).map_err(|_e| MemError::MissingRecord {
            id_or_path: id_or_path.into(),
        })?;
        let record = Record::from_bytes(&bytes).map_err(|e| MemError::Corrupt {
            path: id_or_path.into(),
            reason: e.to_string(),
        })?;
        let t = token::for_bytes(record.envelope.version, &bytes);
        return Ok(Scanned {
            rel: id_or_path.into(),
            record,
            token: t,
        });
    }
    let scan = scan_all(store)?;
    if let Some(hit) = scan
        .records
        .iter()
        .find(|r| r.record.envelope.id == id_or_path)
    {
        return Ok(hit.clone());
    }
    // 别名解析层（D11：别名只作用于查找解析层）。
    if let Some(hit) = scan.records.iter().find(|r| {
        r.record
            .envelope
            .aliases
            .iter()
            .any(|a| paths::fold(a) == paths::fold(id_or_path))
    }) {
        return Ok(hit.clone());
    }
    // attic（受管 tombstone 可读）。
    for rel in crate::event::enumerate_md(store, "attic")? {
        if let Ok(bytes) = std::fs::read(store.abs(&rel)) {
            if let Ok(rec) = Record::from_bytes(&bytes) {
                if rec.envelope.id == id_or_path {
                    return Ok(Scanned {
                        rel,
                        token: token::for_bytes(rec.envelope.version, &bytes),
                        record: rec,
                    });
                }
            }
        }
    }
    Err(MemError::MissingRecord {
        id_or_path: id_or_path.into(),
    })
}

#[cfg(test)]
mod r15_path_tests {
    use crate::paths::validate_store_record_path;

    #[test]
    fn store_record_paths_reject_parent_and_non_normal_components() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("store");
        crate::store::Store::create(&root).unwrap();
        let store = crate::store::Store::open(&root, crate::store::OpenMode::Auto).unwrap();
        for p in [
            "records/../../outside.md",
            "attic/a/../outside.md",
            "records//../x.md",
        ] {
            assert!(
                validate_store_record_path(&store, p).is_err(),
                "must reject {p}"
            );
        }
        assert!(validate_store_record_path(&store, "records/task/a.md").is_ok());
        assert!(validate_store_record_path(&store, "attic/task/a.md").is_ok());
    }
}

// ---------- add ----------

pub struct AddArgs {
    pub type_name: String,
    pub payload: serde_json::Value,
    pub body: Vec<u8>,
    pub principal: Option<String>,
}

#[derive(Debug)]
#[non_exhaustive]
pub struct Added {
    pub rel: String,
    pub record: Record,
    pub token: Token,
    /// false = 输者：返回既有记录及令牌（D11）。
    pub created: bool,
    /// lenient 降级告警等（R8.6）。
    pub warnings: Vec<String>,
}

/// 唯一键成分提取：x-unique-key 字段 →（引用字段取目标 ID，标量字段取原值）。
fn key_components(decl: &TypeDecl, payload: &serde_json::Value) -> MemResult<Vec<String>> {
    let mut out = Vec::new();
    for field in decl.unique_key() {
        let v = payload.get(field).ok_or_else(|| MemError::SchemaReject {
            type_name: decl.type_name().to_string(),
            violations: vec![mem_schema::Violation {
                path: format!("/{field}"),
                message: "唯一键字段缺失（路径推导依据，D19）".into(),
            }],
        })?;
        if v.is_null() {
            return Err(MemError::SchemaReject {
                type_name: decl.type_name().to_string(),
                violations: vec![mem_schema::Violation {
                    path: format!("/{field}"),
                    message: "唯一键字段为 null".into(),
                }],
            });
        }
        out.push(v.as_str().unwrap_or(&v.to_string()).to_string());
    }
    Ok(out)
}

/// 别名同步（查找解析层，D11）：唯一键标量字段值 → aliases（如 task 的 slug）。
/// slug 是键成分；它进入别名层使 `get <slug>` 可解析，不改变身份=ID 的事实。
fn sync_key_aliases(decl: &TypeDecl, payload: &serde_json::Value, aliases: &mut Vec<String>) {
    for field in decl.unique_key() {
        if let Some(serde_json::Value::String(v)) = payload.get(field) {
            if !v.is_empty() && !aliases.iter().any(|a| paths::fold(a) == paths::fold(v)) {
                aliases.push(v.clone());
            }
        }
    }
}

/// 镜像纪律：x-ref 字段（非 null）→ envelope.refs 元素（rel = 字段名）。
fn mirror_refs(decl: &TypeDecl, payload: &serde_json::Value, keep: &[Ref]) -> Vec<Ref> {
    let mirrored: Vec<String> = decl.ref_specs().keys().cloned().collect();
    let mut refs: Vec<Ref> = keep
        .iter()
        .filter(|r| !mirrored.contains(&r.rel))
        .cloned()
        .collect();
    for (field, _spec) in decl.ref_specs() {
        if let Some(serde_json::Value::String(t)) = payload.get(field) {
            if !t.is_empty() {
                refs.push(Ref {
                    rel: field.clone(),
                    target: t.clone(),
                });
            }
        }
    }
    refs
}

fn validate_payload_and_refs(
    store: &Store,
    decl: &TypeDecl,
    payload: &serde_json::Value,
    principal: &Option<String>,
) -> MemResult<Vec<String>> {
    let mut warnings = Vec::new();
    let violations = decl.validate_payload(payload);
    match decl.policy(&violations) {
        mem_schema::Policy::Reject => {
            return Err(MemError::SchemaReject {
                type_name: decl.type_name().to_string(),
                violations,
            });
        }
        mem_schema::Policy::Warn => {
            for v in &violations {
                warnings.push(format!(
                    "lenient 降级：{} {}（{}）",
                    decl.type_name(),
                    v.path,
                    v.message
                ));
            }
        }
        mem_schema::Policy::Accept => {}
    }
    // 未知字段告警（R8.6：容忍 + 告警，不静默）——payload 键 ∉ properties。
    let known = decl
        .raw()
        .get("properties")
        .and_then(|v| v.as_object())
        .map(|o| o.keys().cloned().collect::<std::collections::HashSet<_>>())
        .unwrap_or_default();
    if let Some(obj) = payload.as_object() {
        for k in obj.keys() {
            if !known.contains(k) {
                warnings.push(format!(
                    "未知字段（类型声明未定义）：{}/{}",
                    decl.type_name(),
                    k
                ));
            }
        }
    }
    // x-ref 存在性（R8.5）：目标必须现存于 records/。
    let scan = scan_all(store)?;
    let exists = |t: &str, id: &str| {
        scan.records
            .iter()
            .any(|r| r.record.envelope.r#type == t && r.record.envelope.id == id)
    };
    for v in decl.check_ref_existence(payload, exists) {
        return Err(MemError::RefTargetMissing {
            rel: v.path.trim_start_matches('/').to_string(),
            target: v.message,
        });
    }
    // principal 存在性强制（红线12）。
    if let Some(p) = principal {
        let ok = scan
            .records
            .iter()
            .any(|r| r.record.envelope.r#type == "principal" && r.record.envelope.id == *p);
        if !ok {
            return Err(MemError::RefTargetMissing {
                rel: "principal".into(),
                target: format!("principal 记录不存在：{p}（防错报，不防伪装）"),
            });
        }
    }
    Ok(warnings)
}

/// 唯一键路径独占创建（R3.3/D11）：输者返回既有记录及令牌。
// ---------- 归属解析（写路径归属绑定，方案 principal-binding §2.1） ----------

/// 归属参数解析（单源）：短名（slug）或记录 ID → principal 记录 ID。
/// CLI 与 MCP 的同名重复实现收口于此（决策②：前端零逻辑）。
/// 目标不存在时按既有"归属不存在"形态报（RefTargetMissing，防错报），不透传 MissingRecord。
pub fn resolve_principal_ref(store: &Store, id_or_slug: &str) -> MemResult<String> {
    let hit = get(store, id_or_slug).map_err(|e| match e {
        MemError::MissingRecord { .. } => MemError::RefTargetMissing {
            rel: "principal".into(),
            target: format!("principal 记录不存在：{id_or_slug}（防错报，不防伪装）"),
        },
        other => other,
    })?;
    if hit.record.envelope.r#type != "principal" {
        return Err(MemError::BadArgument(format!(
            "{id_or_slug} 不是 principal 记录"
        )));
    }
    Ok(hit.record.envelope.id)
}

/// principal 记录当前是否存在于 records/（锁外预检与锁内复核共用同一判定）。
fn principal_exists_in_records(store: &Store, principal_id: &str) -> Option<String> {
    let scan = match scan_all(store) {
        Ok(s) => s,
        Err(_) => return None,
    };
    scan.records
        .iter()
        .find(|r| r.record.envelope.r#type == "principal" && r.record.envelope.id == principal_id)
        .map(|r| r.rel.clone())
}

/// 归属解析链第一段（决策①）：显式参数 > 会话台账 > 响亮拒绝。
/// 返回 (归属 ID, 冲突告警)。台账缺失/损坏视为无台账（身份不从残缺现场猜出）。
/// principal 类型豁免由调用方先行判断（决策④）。
fn resolve_ownership(
    ctx: &WriteContext,
    explicit: &Option<String>,
) -> MemResult<(Option<String>, Vec<String>)> {
    let mut warnings = Vec::new();
    let Some(explicit) = explicit.clone() else {
        // 无显式归属 → 台账继承；无台账 → 响亮拒绝（错误给出路，M4 先例形态）。
        let Some(sid) = ctx.session_id else {
            return Err(MemError::BadArgument(
                "归属缺失：必须显式提供 principal，或先 mem_session_start 建台账由其继承 \
                （principal 类型豁免——身份/注册类记录可无归属）"
                    .into(),
            ));
        };
        let ledger = session::get(ctx.store, sid)?.ok_or_else(|| {
            MemError::BadArgument(format!(
                "归属缺失：台账 {sid} 不存在或已损坏——先 mem_session_start 建台账，\
                 或显式提供 principal（principal 类型豁免）"
            ))
        })?;
        return Ok((Some(ledger.principal), warnings));
    };
    let resolved = resolve_principal_ref(ctx.store, &explicit)?;
    // 显式与台账不一致：显式胜出 + 中性事实告警（决策③；跨归属共享内容属合法常态）。
    if let Some(sid) = ctx.session_id {
        if let Ok(Some(ledger)) = session::get(ctx.store, sid) {
            if ledger.principal != resolved {
                warnings.push(format!(
                    "显式 principal 与台账 principal 不同（{} vs {}）",
                    resolved, ledger.principal
                ));
            }
        }
    }
    Ok((Some(resolved), warnings))
}

pub fn add(ctx: &WriteContext, args: AddArgs) -> MemResult<Added> {
    add_inner(ctx, args, None)
}

/// 仅供 debug 集成测试注入受控交错；暂停钩子不属于下游写上下文。
#[doc(hidden)]
#[cfg(all(debug_assertions, not(test)))]
pub fn add_with_pause_hook_test(
    ctx: &WriteContext,
    args: AddArgs,
    hook: crate::event::PauseHook,
) -> MemResult<Added> {
    add_inner(ctx, args, Some(hook.as_ref()))
}

fn add_inner(
    ctx: &WriteContext,
    args: AddArgs,
    pause_hook: Option<&(dyn Fn(&str) + Send + Sync)>,
) -> MemResult<Added> {
    ctx.store.check_writable()?;
    let decl = ctx
        .types
        .get(&args.type_name)
        .ok_or_else(|| MemError::UnknownType(args.type_name.clone()))?;
    // 归属两阶段校验·第一阶段（锁外预检，方案 §2.1）：解析链 + 存在性预检。
    // 位置在最前段——gitattrs 写前核验与任何锁文件创建之前（锁外拒绝 = 全店逐字节一致，决策⑥）。
    // 豁免：principal 类型（自举与注册；显式提供时仍校验，决策④）。
    let (resolved_principal, warnings) = if args.type_name == "principal" {
        match &args.principal {
            None => (None, Vec::new()),
            Some(p) => {
                let (r, w) = resolve_ownership(ctx, &Some(p.clone()))?;
                (r, w)
            }
        }
    } else {
        resolve_ownership(ctx, &args.principal)?
    };
    if let Some(pid) = &resolved_principal {
        if principal_exists_in_records(ctx.store, pid).is_none() {
            return Err(MemError::RefTargetMissing {
                rel: "principal".into(),
                target: format!("principal 记录不存在：{pid}（防错报，不防伪装）"),
            });
        }
    }
    if let Some(hook) = pause_hook {
        hook("principal_precheck");
    }
    // 引用存在性与发布必须在同一屏障内；新增 x-dag 节点也与全图修改串行。
    let _barrier = if decl.dag() {
        lock::AnyBarrierGuard::Exclusive(lock::barrier_exclusive(ctx.store)?)
    } else {
        lock::AnyBarrierGuard::Shared(lock::barrier_shared(ctx.store)?)
    };
    ctx.store.check_compatible_for_write()?;
    // 归属存在性由两阶段校验承担（§2.1）——此处不传归属，避免暂停点之后的
    // 重复扫描与不一致文案（决策⑥：预检文案只属于锁外拒绝路径）。
    let no_principal: Option<String> = None;
    let warnings = validate_payload_and_refs(ctx.store, decl, &args.payload, &no_principal)?
        .into_iter()
        .chain(warnings)
        .collect();
    let comps = key_components(decl, &args.payload)?;
    let id = new_uuid_v7();
    let rel = paths::derive_record_rel(&args.type_name, &comps, &id)?;
    paths::validate_store_record_path(ctx.store, &rel)?;
    let now = crate::clock::now_rfc3339();
    let envelope = Envelope {
        id: id.clone(),
        r#type: args.type_name.clone(),
        created: now.clone(),
        modified: now,
        version: 1,
        principal: resolved_principal.clone(),
        refs: mirror_refs(decl, &args.payload, &[]),
        aliases: {
            let mut a = Vec::new();
            sync_key_aliases(decl, &args.payload, &mut a);
            a
        },
        evidence_cutoff_seq: None,
        commit: None,
        extra: Default::default(),
    };
    if decl.dag() {
        dag_candidate_precheck(ctx, &envelope.id, &envelope.r#type, &envelope.refs)?;
    }
    let bytes = record::serialize_new(&envelope, &args.payload, &args.body);

    let _rec = lock::record_lock(ctx.store, &rel, std::time::Duration::from_secs(30))?;
    // 归属两阶段校验·第二阶段（锁内复核，方案 §2.1 决策⑥）：预检通过后、任何写盘
    // 之前，持归属 principal 的记录锁（锁序：新记录锁 → principal 锁 → 事件序列锁，
    // 单向无环）重新确认身份仍在 records/——与删除在 principal 锁上串行，悬空归属
    // 在受管并发下结构性排除。锁文件豁免于真相层净零断言（决策⑥）。
    let principal_rel = match &resolved_principal {
        Some(pid) => match principal_exists_in_records(ctx.store, pid) {
            Some(rel) => Some((pid.clone(), rel)),
            None => {
                return Err(MemError::RefTargetMissing {
                    rel: "principal".into(),
                    target: format!(
                        "创建期间归属身份已被删除：{pid}（可恢复后重试，或改用其他身份）"
                    ),
                });
            }
        },
        None => None,
    };
    // 同路径自锁防御：显式指定自身为归属（重复创建同一 principal）时，归属锁与
    // 新记录锁指向同一路径——记录锁不可重入，二次获取会等待 30 秒超时而非走
    // 输者协议。同路径即复用已持有的记录锁完成复核。
    let same_path = principal_rel
        .as_ref()
        .map_or(false, |(_, prel)| prel == &rel);
    let _principal_lock = if same_path {
        None
    } else {
        match &principal_rel {
            Some((_, rel)) => Some(lock::record_lock(
                ctx.store,
                rel,
                std::time::Duration::from_secs(30),
            )?),
            None => None,
        }
    };
    if let Some((pid, _)) = &principal_rel {
        if principal_exists_in_records(ctx.store, pid).is_none() {
            return Err(MemError::RefTargetMissing {
                rel: "principal".into(),
                target: format!("创建期间归属身份已被删除：{pid}（可恢复后重试，或改用其他身份）"),
            });
        }
    }
    // BD5 ②③：受管写前核验——位于锁内复核之后、记录文件创建之前，保证锁内
    // 复核拒绝路径不留下 .gitattributes（决策⑥：锁内拒绝真相层净零）。
    crate::gitattrs::ensure_for_write(ctx.store)?;
    // 唯一性强制点：路径原子独占（不在索引，红线8）。
    let target = ctx.store.abs(&rel);
    if target.is_file() {
        // 输者协议：返回既有记录及令牌。
        let existing = get(ctx.store, &rel)?;
        return Ok(Added {
            rel,
            record: existing.record,
            token: existing.token,
            created: false,
            warnings,
        });
    }
    // 占位独占创建（并发双 create 只有一者成功）。
    if let Err(e) = crate::platform::create_new(&target, &bytes) {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            let existing = get(ctx.store, &rel)?;
            return Ok(Added {
                rel,
                record: existing.record,
                token: existing.token,
                created: false,
                warnings,
            });
        }
        return Err(MemError::io(target.display().to_string(), e));
    }
    if let Some(hook) = pause_hook {
        hook("record_published");
    }
    // 占位内容正确（create_new 已写入完整字节）；追加事件。
    let log = EventLog::new(ctx.store);
    log.append_simple(
        &id,
        &rel,
        Op::Create,
        None,
        Some(1),
        None,
        Some(&token::sha256_hex(&bytes)),
        &ctx.by,
    )?;
    if let Some(hook) = pause_hook {
        hook("event_appended");
    }
    commit_same_batch(ctx, &[rel.clone()], 1)?;
    register_trail(ctx, &id);
    Ok(Added {
        rel,
        record: Record::from_bytes(&bytes).expect("自产字节必可解析"),
        token: token::for_bytes(1, &bytes),
        created: true,
        warnings,
    })
}

// ---------- update ----------

#[derive(Debug)]
#[non_exhaustive]
pub struct Updated {
    pub rel: String,
    pub record: Record,
    pub token: Token,
    pub warnings: Vec<String>,
}

/// 读改写（强制令牌 CAS，红线19；唯一键字段不可变——移动语义转维护例程）。
pub fn update(
    ctx: &WriteContext,
    id_or_path: &str,
    expect: &Token,
    payload: serde_json::Value,
    body: Option<Vec<u8>>,
) -> MemResult<Updated> {
    ctx.store.check_writable()?;
    let cur = get(ctx.store, id_or_path)?;
    if cur.rel.starts_with("attic/") {
        return Err(MemError::BadArgument(
            "attic 中的删除记录不能直接更新；请先 restore".into(),
        ));
    }
    let decl = ctx
        .types
        .get(&cur.record.envelope.r#type)
        .ok_or_else(|| MemError::UnknownType(cur.record.envelope.r#type.clone()))?;
    let _barrier = if decl.dag() {
        lock::AnyBarrierGuard::Exclusive(lock::barrier_exclusive(ctx.store)?)
    } else {
        lock::AnyBarrierGuard::Shared(lock::barrier_shared(ctx.store)?)
    };
    ctx.store.check_compatible_for_write()?;
    update_locked(ctx, id_or_path, expect, payload, body)
}

/// [`update`] 的门闩内变体（调用方已持门闩——维护例程/apply 的计划级独占用；
/// 自身仍取记录锁与序列锁，锁序不变）。
pub(crate) fn update_locked(
    ctx: &WriteContext,
    id_or_path: &str,
    expect: &Token,
    payload: serde_json::Value,
    body: Option<Vec<u8>>,
) -> MemResult<Updated> {
    crate::gitattrs::ensure_for_write(ctx.store)?; // BD5 ②③
    let cur = get(ctx.store, id_or_path)?;
    let decl = ctx
        .types
        .get(&cur.record.envelope.r#type)
        .ok_or_else(|| MemError::UnknownType(cur.record.envelope.r#type.clone()))?;
    let warnings =
        validate_payload_and_refs(ctx.store, decl, &payload, &cur.record.envelope.principal)?;

    let _rec = lock::record_lock(ctx.store, &cur.rel, std::time::Duration::from_secs(30))?;
    // 锁内重读 + CAS（版本与全摘要，红线19）。
    let fresh_bytes =
        std::fs::read(ctx.store.abs(&cur.rel)).map_err(|e| MemError::io(cur.rel.clone(), e))?;
    let fresh = Record::from_bytes(&fresh_bytes)?;
    let current = token::for_bytes(fresh.envelope.version, &fresh_bytes);
    if let Err(_e) = token::cas_check(expect, &current) {
        return Err(MemError::Conflict {
            expected: expect.clone(),
            current,
        });
    }
    // 唯一键不可变（D19/D11：改动 = 移动，v1 拒收）。
    let changed: Vec<String> = decl
        .unique_key()
        .iter()
        .filter(|f| fresh.payload.get(*f) != payload.get(*f))
        .cloned()
        .collect();
    if !changed.is_empty() {
        return Err(MemError::UniqueKeyImmut { fields: changed });
    }
    // 再生成 frontmatter（version+1），正文按字节保持（红线27）。
    let new_body = body.unwrap_or_else(|| fresh.body.clone());
    let mut env = fresh.envelope.clone();
    env.version += 1;
    env.modified = crate::clock::now_rfc3339();
    env.refs = mirror_refs(decl, &payload, &fresh.envelope.refs);
    if decl.dag() {
        dag_candidate_precheck(ctx, &env.id, &env.r#type, &env.refs)?;
    }
    sync_key_aliases(decl, &payload, &mut env.aliases);
    let bytes = record::serialize_new(&env, &payload, &new_body);
    crate::platform::atomic_write(&ctx.store.abs(&cur.rel), &bytes)
        .map_err(|e| MemError::io(cur.rel.clone(), e))?;
    let new_token = token::for_bytes(env.version, &bytes);
    let log = EventLog::new(ctx.store);
    log.append_simple(
        &env.id,
        &cur.rel,
        Op::Update,
        Some(fresh.envelope.version),
        Some(env.version),
        Some(&current.digest),
        Some(&new_token.digest.clone()),
        &ctx.by,
    )?;
    commit_same_batch(ctx, &[cur.rel.clone()], 0)?;
    register_trail(ctx, &env.id);
    Ok(Updated {
        rel: cur.rel,
        record: Record::from_bytes(&bytes).expect("自产字节必可解析"),
        token: new_token,
        warnings,
    })
}

// ---------- link ----------

/// 建立引用（两次单记录令牌 CAS；x-dag 类型派生 rel 强制无环，D16/红线17）。
pub fn link(
    ctx: &WriteContext,
    a_id: &str,
    b_id: &str,
    rel: &str,
    expect_a: &Token,
    expect_b: &Token,
) -> MemResult<(Token, Token)> {
    ctx.store.check_writable()?;
    crate::gitattrs::ensure_for_write(ctx.store)?; // BD5 ②③
    let initial_a = get(ctx.store, a_id)?;
    let initial_b = get(ctx.store, b_id)?;
    ensure_link_endpoint_is_live(&initial_a, "源")?;
    ensure_link_endpoint_is_live(&initial_b, "目标")?;
    let is_dag_edge = ctx
        .types
        .get(&initial_a.record.envelope.r#type)
        .is_some_and(|d| d.dag() && d.derived_rel() == rel);
    let _barrier = if is_dag_edge {
        lock::AnyBarrierGuard::Exclusive(lock::barrier_exclusive(ctx.store)?)
    } else {
        lock::AnyBarrierGuard::Shared(lock::barrier_shared(ctx.store)?)
    };
    ctx.store.check_compatible_for_write()?;
    // 在屏障内重解析，删除/恢复不能在验证与发布之间改变端点所在位置。
    let a = get(ctx.store, a_id)?;
    let b = get(ctx.store, b_id)?;
    ensure_link_endpoint_is_live(&a, "源")?;
    ensure_link_endpoint_is_live(&b, "目标")?;
    if a.record.envelope.id == b.record.envelope.id {
        return Err(MemError::BadArgument("自引用不允许（环的特例）".into()));
    }
    // 锁序：按 rel 字典序取两把记录锁（避免死锁）。
    let mut rels = vec![a.rel.clone(), b.rel.clone()];
    rels.sort();
    let _l1 = lock::record_lock(ctx.store, &rels[0], std::time::Duration::from_secs(30))?;
    let _l2 = lock::record_lock(ctx.store, &rels[1], std::time::Duration::from_secs(30))?;
    // 两枚令牌都可能因自己刚更新而过期——逐个 CAS + 写。
    let fresh_a = read_fresh(ctx.store, &a.rel)?;
    let tok_a = token::for_bytes(fresh_a.0.envelope.version, &fresh_a.1);
    token::cas_check(expect_a, &tok_a).map_err(|_| MemError::Conflict {
        expected: expect_a.clone(),
        current: tok_a.clone(),
    })?;
    let fresh_b = read_fresh(ctx.store, &b.rel)?;
    let tok_b = token::for_bytes(fresh_b.0.envelope.version, &fresh_b.1);
    token::cas_check(expect_b, &tok_b).map_err(|_| MemError::Conflict {
        expected: expect_b.clone(),
        current: tok_b.clone(),
    })?;
    // 边只落在**源记录** a 上（引用方向 = a --rel--> b；双向落边会凭空成环）。
    // 目标 b 仅做存在性 + 令牌 CAS 校验（调用方认知一致），不写不改版本。
    if is_dag_edge {
        let mut candidate_refs = fresh_a.0.envelope.refs.clone();
        candidate_refs.push(Ref {
            rel: rel.to_string(),
            target: fresh_b.0.envelope.id.clone(),
        });
        dag_candidate_precheck(
            ctx,
            &fresh_a.0.envelope.id,
            &fresh_a.0.envelope.r#type,
            &candidate_refs,
        )?;
    }
    let (env_a, bytes_a) = apply_ref(
        ctx,
        &a.rel,
        &fresh_a.0,
        &fresh_a.1,
        rel,
        &b.record.envelope.id,
    )?;
    let log = EventLog::new(ctx.store);
    let nt_a = token::for_bytes(env_a.version, &bytes_a);
    log.append_simple(
        &env_a.id,
        &a.rel,
        Op::Link,
        Some(fresh_a.0.envelope.version),
        Some(env_a.version),
        Some(&tok_a.digest),
        Some(&nt_a.digest.clone()),
        &ctx.by,
    )?;
    commit_same_batch(ctx, &[a.rel.clone()], 0)?;
    register_trail(ctx, &env_a.id);
    Ok((nt_a, tok_b))
}

fn read_fresh(store: &Store, rel: &str) -> MemResult<(Record, Vec<u8>)> {
    let bytes = std::fs::read(store.abs(rel)).map_err(|e| MemError::io(rel, e))?;
    Ok((Record::from_bytes(&bytes)?, bytes))
}

fn apply_ref(
    ctx: &WriteContext,
    rel: &str,
    rec: &Record,
    _bytes: &[u8],
    edge_rel: &str,
    target: &str,
) -> MemResult<(Envelope, Vec<u8>)> {
    let mut env = rec.envelope.clone();
    env.version += 1;
    env.modified = crate::clock::now_rfc3339();
    env.refs.push(Ref {
        rel: edge_rel.to_string(),
        target: target.to_string(),
    });
    let out = record::serialize_new(&env, &rec.payload, &rec.body);
    crate::platform::atomic_write(&ctx.store.abs(rel), &out).map_err(|e| MemError::io(rel, e))?;
    Ok((env, out))
}

fn ensure_link_endpoint_is_live(record: &Scanned, side: &str) -> MemResult<()> {
    if !record.rel.starts_with("records/") {
        return Err(MemError::BadArgument(format!(
            "link 的{side}记录已删除并位于 attic，不能新增引用；如需继续请先 restore"
        )));
    }
    Ok(())
}

/// 用候选记录替换其派生边后检查整张 x-dag 图。调用方必须持有独占门闩，
/// 这样 update、link 与维护计划的图变更不会并发拼出环。
fn dag_candidate_precheck(
    ctx: &WriteContext,
    candidate_id: &str,
    type_name: &str,
    candidate_refs: &[Ref],
) -> MemResult<()> {
    let Some(decl) = ctx.types.get(type_name).filter(|d| d.dag()) else {
        return Ok(());
    };
    let rel = decl.derived_rel();
    let scan = scan_all(ctx.store)?;
    dag_graph_precheck(
        type_name,
        rel,
        scan.records
            .iter()
            .filter(|r| {
                r.record.envelope.r#type == type_name && r.record.envelope.id != candidate_id
            })
            .map(|r| {
                (
                    r.record.envelope.id.as_str(),
                    r.record.envelope.refs.as_slice(),
                )
            })
            .chain(std::iter::once((candidate_id, candidate_refs))),
    )
}

fn dag_graph_precheck<'a>(
    type_name: &str,
    rel: &str,
    records: impl Iterator<Item = (&'a str, &'a [Ref])>,
) -> MemResult<()> {
    let mut adj: std::collections::HashMap<String, Vec<String>> = Default::default();
    for (id, refs) in records {
        for e in refs {
            if e.rel == rel {
                adj.entry(id.to_string())
                    .or_default()
                    .push(e.target.clone());
            }
        }
    }
    fn visit<'a>(
        node: &'a str,
        adj: &'a std::collections::HashMap<String, Vec<String>>,
        active: &mut std::collections::HashSet<&'a str>,
        done: &mut std::collections::HashSet<&'a str>,
    ) -> bool {
        if active.contains(node) {
            return true;
        }
        if done.contains(node) {
            return false;
        }
        active.insert(node);
        if let Some(next) = adj.get(node) {
            for target in next {
                if visit(target, adj, active, done) {
                    return true;
                }
            }
        }
        active.remove(node);
        done.insert(node);
        false
    }
    let mut active = std::collections::HashSet::new();
    let mut done = std::collections::HashSet::new();
    for node in adj.keys() {
        if visit(node, &adj, &mut active, &mut done) {
            return Err(MemError::CycleDetected(format!(
                "{type_name} 类型派生图成环（rel={rel}），涉及 {node}"
            )));
        }
    }
    Ok(())
}

fn dag_merge_precheck(
    ctx: &WriteContext,
    scan: &ScanResult,
    dup_id: &str,
    canonical_id: &str,
) -> MemResult<()> {
    for decl in ctx.types.decls().filter(|d| d.dag()) {
        let type_name = decl.type_name();
        let rel = decl.derived_rel();
        let transformed: Vec<(String, Vec<Ref>)> = scan
            .records
            .iter()
            .filter(|r| r.record.envelope.r#type == type_name && r.record.envelope.id != dup_id)
            .map(|r| {
                let refs = r
                    .record
                    .envelope
                    .refs
                    .iter()
                    .filter_map(|e| {
                        if r.record.envelope.id == canonical_id
                            && e.rel == rel
                            && e.target == dup_id
                        {
                            None
                        } else {
                            let mut edge = e.clone();
                            if edge.target == dup_id {
                                edge.target = canonical_id.to_string();
                            }
                            Some(edge)
                        }
                    })
                    .collect();
                (r.record.envelope.id.clone(), refs)
            })
            .collect();
        dag_graph_precheck(
            type_name,
            rel,
            transformed
                .iter()
                .map(|(id, refs)| (id.as_str(), refs.as_slice())),
        )?;
    }
    Ok(())
}

// ---------- delete / restore ----------

/// 归属依赖者：信封归属字段指向该 id 的现存记录（方案 principal-binding 决策⑦）。
/// 归属字段不在 refs 集合内，独立扫描——两个域的统一审计职责在 verify（决策⑦）。
fn ownership_dependents(store: &Store, id: &str) -> MemResult<Vec<(String, String)>> {
    let scan = scan_all(store)?;
    Ok(scan
        .records
        .iter()
        .filter(|r| r.record.envelope.principal.as_deref() == Some(id))
        .map(|r| (r.record.envelope.id.clone(), r.rel.clone()))
        .collect())
}

/// 删除依赖阻止/告警判定（引用边 ∪ 归属字段；默认阻止，force 显式越过）。
/// 锁外预检与锁内复核共用同一判定——告警计数以锁内复核为准（决策⑦）。
fn delete_dependency_guard(
    dependents: &[(String, String)],
    owned: &[(String, String)],
    target_id: &str,
    force: bool,
    warnings: &mut Vec<String>,
) -> MemResult<()> {
    if !force {
        if !dependents.is_empty() || !owned.is_empty() {
            let mut parts = Vec::new();
            if !dependents.is_empty() {
                parts.push(format!(
                    "{} 条引用（{:?}）",
                    dependents.len(),
                    dependents
                        .iter()
                        .map(|(id, rel)| format!("{id}:{rel}"))
                        .collect::<Vec<_>>()
                ));
            }
            if !owned.is_empty() {
                parts.push(format!(
                    "{} 条归属（{:?}）",
                    owned.len(),
                    owned
                        .iter()
                        .map(|(id, rel)| format!("{id}:{rel}"))
                        .collect::<Vec<_>>()
                ));
            }
            return Err(MemError::BadArgument(format!(
                "删除被依赖阻止：{} 指向 {}——先处理依赖或使用 force（受管 tombstone 可恢复）",
                parts.join("、"),
                target_id
            )));
        }
        return Ok(());
    }
    if !dependents.is_empty() {
        warnings.push(format!(
            "force 删除：{} 条引用悬空（断链注记由 verify/agenda 报告）",
            dependents.len()
        ));
    }
    if !owned.is_empty() {
        warnings.push(format!(
            "force 删除：{} 条记录归属悬空（断链注记由 verify 报告）",
            owned.len()
        ));
    }
    Ok(())
}

#[derive(Debug)]
#[non_exhaustive]
pub struct Deleted {
    pub rel: String, // attic 路径
    pub token: Token,
    /// force 越过的警示（全量标注，D17）。
    pub warnings: Vec<String>,
}

/// 删除（受管 tombstone：移入 attic；派生/一般引用默认阻止，force 显式越过并警示）。
pub fn delete_record(
    ctx: &WriteContext,
    id_or_path: &str,
    expect: &Token,
    force: bool,
) -> MemResult<Deleted> {
    ctx.store.check_writable()?;
    // 反向引用检查必须与所有引用写入互斥，否则检查后可并发新增悬空引用。
    let _barrier = lock::barrier_exclusive(ctx.store)?;
    ctx.store.check_compatible_for_write()?;
    delete_locked(ctx, id_or_path, expect, force)
}

/// [`delete_record`] 的门闩内变体（调用方已持门闩——apply 计划级独占用）。
pub(crate) fn delete_locked(
    ctx: &WriteContext,
    id_or_path: &str,
    expect: &Token,
    force: bool,
) -> MemResult<Deleted> {
    crate::gitattrs::ensure_for_write(ctx.store)?; // BD5 ②③
    let cur = get(ctx.store, id_or_path)?;
    if cur.rel.starts_with("attic/") {
        return Err(MemError::BadArgument("已是 tombstone".into()));
    }
    // 删除依赖（R3.4/R9.2 引用边 ∪ 归属字段，方案 principal-binding 决策⑦）：
    // 锁外预检（快速失败）。
    let dependents = reverse_refs(ctx.store, &cur.record.envelope.id)?;
    let owned = ownership_dependents(ctx.store, &cur.record.envelope.id)?;
    let mut warnings = Vec::new();
    delete_dependency_guard(
        &dependents,
        &owned,
        &cur.record.envelope.id,
        force,
        &mut warnings,
    )?;
    let attic_rel = paths::attic_rel(&cur.rel)?;
    paths::validate_store_record_path(ctx.store, &attic_rel)?;
    let _rec = lock::record_lock(ctx.store, &cur.rel, std::time::Duration::from_secs(30))?;
    // 依赖锁内复核（决策⑦）："删除预检通过 → add 创建归属记录 → 删除取锁执行"的
    // 窗口在此堵住——同判定再算一遍，告警计数以锁内复核为准。
    let dependents_in_lock = reverse_refs(ctx.store, &cur.record.envelope.id)?;
    let owned_in_lock = ownership_dependents(ctx.store, &cur.record.envelope.id)?;
    let mut warnings_in_lock = Vec::new();
    delete_dependency_guard(
        &dependents_in_lock,
        &owned_in_lock,
        &cur.record.envelope.id,
        force,
        &mut warnings_in_lock,
    )?;
    warnings = warnings_in_lock;
    let (fresh, bytes) = read_fresh(ctx.store, &cur.rel)?;
    let current = token::for_bytes(fresh.envelope.version, &bytes);
    token::cas_check(expect, &current)?;
    // tombstone 最后（D17 维护流程）：原子改名进 attic。
    let attic_abs = ctx.store.abs(&attic_rel);
    std::fs::create_dir_all(attic_abs.parent().unwrap())
        .map_err(|e| MemError::io(attic_abs.display().to_string(), e))?;
    crate::platform::rename_noreplace(&ctx.store.abs(&cur.rel), &attic_abs)
        .map_err(|e| MemError::io(attic_abs.display().to_string(), e))?;
    let log = EventLog::new(ctx.store);
    log.append_simple(
        &fresh.envelope.id,
        &attic_rel,
        Op::Delete,
        Some(fresh.envelope.version),
        Some(fresh.envelope.version),
        Some(&current.digest.clone()),
        Some(&current.digest.clone()),
        &ctx.by,
    )?;
    // H2：rename 双侧入批——旧 records 路径的删除进本次提交（否则 HEAD 永留已删文件）。
    commit_same_batch(ctx, &[cur.rel.clone(), attic_rel.clone()], 0)?;
    register_trail(ctx, &fresh.envelope.id);
    Ok(Deleted {
        rel: attic_rel,
        token: current,
        warnings,
    })
}

/// 仅供核心库集成测试在已持有门闩的时序点执行。
#[doc(hidden)]
#[cfg(debug_assertions)]
pub fn delete_locked_test(
    ctx: &WriteContext,
    id_or_path: &str,
    expect: &Token,
    force: bool,
) -> MemResult<Deleted> {
    delete_locked(ctx, id_or_path, expect, force)
}

#[derive(Debug)]
#[non_exhaustive]
pub struct Restored {
    pub rel: String,
    pub token: Token,
}

/// 恢复（attic → records 同路径；records 侧已被重建 → 结构化冲突转人工）。
pub fn restore_record(
    ctx: &WriteContext,
    id_or_path: &str,
    expect: Option<&Token>,
) -> MemResult<Restored> {
    ctx.store.check_writable()?;
    let tomb = get(ctx.store, id_or_path)?;
    let is_dag = ctx
        .types
        .get(&tomb.record.envelope.r#type)
        .is_some_and(|d| d.dag());
    let _barrier = if is_dag {
        lock::AnyBarrierGuard::Exclusive(lock::barrier_exclusive(ctx.store)?)
    } else {
        lock::AnyBarrierGuard::Shared(lock::barrier_shared(ctx.store)?)
    };
    ctx.store.check_compatible_for_write()?;
    restore_locked(ctx, id_or_path, expect)
}

/// [`restore_record`] 的门闩内变体（调用方已持门闩——apply 计划级独占用）。
pub(crate) fn restore_locked(
    ctx: &WriteContext,
    id_or_path: &str,
    expect: Option<&Token>,
) -> MemResult<Restored> {
    crate::gitattrs::ensure_for_write(ctx.store)?; // BD5 ②③
    let tomb = get(ctx.store, id_or_path)?;
    if !tomb.rel.starts_with("attic/") {
        return Err(MemError::BadArgument("记录不在 attic（非受管删除）".into()));
    }
    let records_rel = paths::records_rel(&tomb.rel)?;
    paths::validate_store_record_path(ctx.store, &records_rel)?;
    // 同规范路径锁：restore 与缺失记录回填/重建互斥（红线24）。
    let _rec = lock::record_lock(ctx.store, &records_rel, std::time::Duration::from_secs(30))?;
    if ctx.store.abs(&records_rel).is_file() {
        return Err(MemError::CreateConflict {
            path: records_rel,
            existing_id: tomb.record.envelope.id.clone(),
        });
    }
    let (fresh, bytes) = read_fresh(ctx.store, &tomb.rel)?;
    if let Some(expect) = expect {
        let current = token::for_bytes(fresh.envelope.version, &bytes);
        token::cas_check(expect, &current)?;
    }
    if let Some(decl) = ctx.types.get(&fresh.envelope.r#type).filter(|d| d.dag()) {
        dag_candidate_precheck(
            ctx,
            &fresh.envelope.id,
            decl.type_name(),
            &fresh.envelope.refs,
        )?;
    }
    let target_abs = ctx.store.abs(&records_rel);
    std::fs::create_dir_all(target_abs.parent().unwrap())
        .map_err(|e| MemError::io(target_abs.display().to_string(), e))?;
    std::fs::rename(ctx.store.abs(&tomb.rel), &target_abs)
        .map_err(|e| MemError::io(target_abs.display().to_string(), e))?;
    let log = EventLog::new(ctx.store);
    log.append_simple(
        &fresh.envelope.id,
        &records_rel,
        Op::Restore,
        Some(fresh.envelope.version),
        Some(fresh.envelope.version),
        Some(&token::sha256_hex(&bytes)),
        Some(&token::sha256_hex(&bytes)),
        &ctx.by,
    )?;
    // H2：rename 双侧入批——attic 侧旧路径的消失进本次提交。
    commit_same_batch(ctx, &[tomb.rel.clone(), records_rel.clone()], 0)?;
    register_trail(ctx, &fresh.envelope.id);
    Ok(Restored {
        rel: records_rel,
        token: token::for_bytes(fresh.envelope.version, &bytes),
    })
}

/// 反向索引：谁引用了我（信封 refs；R9.2）。
pub fn reverse_refs(store: &Store, id: &str) -> MemResult<Vec<(String, String)>> {
    let scan = scan_all(store)?;
    let mut out = Vec::new();
    for r in &scan.records {
        for e in &r.record.envelope.refs {
            if e.target == id {
                out.push((r.record.envelope.id.clone(), e.rel.clone()));
            }
        }
    }
    Ok(out)
}

// ---------- commit-pending ----------

/// git 同批提交的统一入口（D17）：记录路径 + events.jsonl 同批；失败转
/// commit-pending（不分配序号、不回滚真相层）。通用写路径（[`commit_same_batch`]
/// 的调用方）与 fix（实施评审 B1：修复版本必须入史，否则随后 restore-body
/// history 的锚摘要不在 git 历史中——复合修复链断裂）共用本纪律。
pub(crate) fn commit_batch(
    store: &Store,
    committer: Option<&dyn CommitSink>,
    record_paths: &[String],
) -> MemResult<()> {
    let Some(sink) = committer else { return Ok(()) };
    let mut batch: Vec<String> = record_paths.to_vec();
    batch.push("events.jsonl".into());
    // BD5 ②：行尾规则文件随受管写批次入树（幂等——已入树且内容相符时对
    // path-scoped 提交无 diff；init 暂存的规则由此随首次提交入树；commit-pending
    // 批次同样携带，直至核验入树）。冲突状态在写入口已结构化拒绝（③），
    // 走到这里的批次必然是默认内容或待补种状态。
    if store.root().join(".git").exists() {
        batch.push(crate::gitattrs::GITATTRIBUTES_REL.into());
    }
    match sink.commit(&batch) {
        Ok(_) => Ok(()),
        Err(e) => {
            // commit-pending：不分配序号、不回滚真相层（D17）。
            let dir = store.abs(".pending-commits");
            std::fs::create_dir_all(&dir)
                .map_err(|er| MemError::io(dir.display().to_string(), er))?;
            let name = format!(
                "{}.json",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            );
            let payload = serde_json::json!({
                "paths": batch,
                "ts": crate::clock::now_rfc3339(),
                "error": e.to_string(),
            });
            std::fs::write(
                dir.join(&name),
                serde_json::to_vec_pretty(&payload).unwrap(),
            )
            .map_err(|er| MemError::io(dir.join(name).display().to_string(), er))?;
            Ok(())
        }
    }
}

/// [`commit_batch`] 的写路径变体（取 WriteContext 的 store/committer）。
fn commit_same_batch(ctx: &WriteContext, record_paths: &[String], _seq: u64) -> MemResult<()> {
    commit_batch(ctx.store, ctx.committer, record_paths)
}

fn register_trail(ctx: &WriteContext, record_id: &str) {
    if let Some(sid) = ctx.session_id {
        let _ = crate::session::register_trail(ctx.store, sid, record_id);
    }
}

/// pending 对账条目（reconcile 重试提交）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingCommit {
    pub paths: Vec<String>,
    pub ts: String,
    pub error: String,
}

pub fn list_pending_commits(store: &Store) -> MemResult<Vec<(String, PendingCommit)>> {
    store.read_compatible_missing_features()?;
    let result = list_pending_commits_unchecked(store);
    store.read_compatible_missing_features()?;
    result
}

fn list_pending_commits_unchecked(store: &Store) -> MemResult<Vec<(String, PendingCommit)>> {
    let dir = store.abs(".pending-commits");
    let mut out = Vec::new();
    if !dir.is_dir() {
        return Ok(out);
    }
    for e in std::fs::read_dir(&dir).map_err(|e| MemError::io(dir.display().to_string(), e))? {
        let e = e.map_err(|e| MemError::io(dir.display().to_string(), e))?;
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let bytes = std::fs::read(&p).map_err(|e| MemError::io(p.display().to_string(), e))?;
        let pc: PendingCommit =
            serde_json::from_slice(&bytes).map_err(|er| MemError::PlanCorrupt {
                path: p.display().to_string(),
                reason: er.to_string(),
            })?;
        out.push((p.display().to_string(), pc));
    }
    Ok(out)
}

/// merge（D17）：独占门闩内——别名移交 + 引用重挂 + dup tombstone 最后；单提交。
/// 细节收口见开放问题32；v1 语义：dup 不可再恢复为独立身份（重挂即收敛）。
pub fn merge(
    ctx: &WriteContext,
    dup_id: &str,
    canonical_id: &str,
    expect_dup: &Token,
    expect_canonical: &Token,
) -> MemResult<Merged> {
    ctx.store.check_writable()?;
    let _barrier = lock::barrier_exclusive(ctx.store)?;
    ctx.store.check_compatible_for_write()?;
    merge_locked(ctx, dup_id, canonical_id, expect_dup, expect_canonical)
}

/// [`merge`] 的门闩内变体（调用方已持独占门闩——apply 的计划级独占用）。
pub(crate) fn merge_locked(
    ctx: &WriteContext,
    dup_id: &str,
    canonical_id: &str,
    expect_dup: &Token,
    expect_canonical: &Token,
) -> MemResult<Merged> {
    crate::gitattrs::ensure_for_write(ctx.store)?; // BD5 ②③
    let dup = get(ctx.store, dup_id)?;
    let canonical = get(ctx.store, canonical_id)?;
    if dup.record.envelope.id == canonical.record.envelope.id {
        return Err(MemError::BadArgument("不能与自身 merge".into()));
    }
    // 预先检查确定性 tombstone 冲突，避免之后已经重挂引用/改 canonical 才失败。
    // 末尾 rename_noreplace 仍保留，防范屏障外部写入或文件系统错误。
    let dup_attic_preflight = paths::attic_rel(&dup.rel)?;
    paths::validate_store_record_path(ctx.store, &dup_attic_preflight)?;
    if std::fs::symlink_metadata(ctx.store.abs(&dup_attic_preflight)).is_ok() {
        return Err(MemError::io(
            dup_attic_preflight.clone(),
            std::io::Error::new(std::io::ErrorKind::AlreadyExists, "tombstone 目标已存在"),
        ));
    }
    let mut rels = vec![dup.rel.clone(), canonical.rel.clone()];
    rels.sort();
    let _l1 = lock::record_lock(ctx.store, &rels[0], std::time::Duration::from_secs(30))?;
    let _l2 = lock::record_lock(ctx.store, &rels[1], std::time::Duration::from_secs(30))?;
    // CAS 两端。
    let (fresh_dup, dup_bytes) = read_fresh(ctx.store, &dup.rel)?;
    let tok_dup = token::for_bytes(fresh_dup.envelope.version, &dup_bytes);
    token::cas_check(expect_dup, &tok_dup)?;
    let (fresh_can, can_bytes) = read_fresh(ctx.store, &canonical.rel)?;
    let tok_can = token::for_bytes(fresh_can.envelope.version, &can_bytes);
    token::cas_check(expect_canonical, &tok_can)?;

    let mut touched: Vec<String> = Vec::new();
    let log = EventLog::new(ctx.store);
    let scan = scan_all(ctx.store)?;
    dag_merge_precheck(
        ctx,
        &scan,
        &dup.record.envelope.id,
        &canonical.record.envelope.id,
    )?;

    // 1) 引用重挂：第三方记录中指向 dup 的信封引用 → canonical（逐记录重读——
    //    独占门闩内无并发写）。r4 B4：canonical 与 dup **不走本循环**——
    //    canonical 由步骤 2 基于锁内最新状态收敛写（旧实现让循环以陈旧快照改写
    //    canonical、别名移交再以另一份陈旧快照覆盖回去：引用重挂被丢 + 事件链
    //    old_v 重复）；dup 即将 tombstone，其自身出边无需重挂。
    for r in &scan.records {
        if r.record.envelope.id == canonical.record.envelope.id
            || r.record.envelope.id == dup.record.envelope.id
        {
            continue;
        }
        // r5 B4：x-ref 字段集取自**引用方自身**的类型声明——待改字段属于引用方
        // 记录，dup 的类型声明与它无关（旧实现按 dup 类型取字段，payload 与
        // envelope 分裂）。
        let ref_fields: Vec<String> = ctx
            .types
            .get(&r.record.envelope.r#type)
            .map(|d| d.ref_specs().keys().cloned().collect())
            .unwrap_or_default();
        let needs = r
            .record
            .envelope
            .refs
            .iter()
            .any(|e| e.target == dup.record.envelope.id)
            || (!ref_fields.is_empty()
                && r.record
                    .payload
                    .as_object()
                    .map(|o| {
                        o.iter().any(|(k, v)| {
                            ref_fields.contains(k)
                                && v.as_str() == Some(dup.record.envelope.id.as_str())
                        })
                    })
                    .unwrap_or(false));
        if !needs {
            continue;
        }
        let (fresh, bytes) = read_fresh(ctx.store, &r.rel)?;
        let cur_tok = token::for_bytes(fresh.envelope.version, &bytes);
        let mut env = fresh.envelope.clone();
        env.version += 1;
        env.modified = crate::clock::now_rfc3339();
        for e in &mut env.refs {
            if e.target == dup.record.envelope.id {
                e.target = canonical.record.envelope.id.clone();
            }
        }
        // payload 层 x-ref 字段重挂（仅**引用方声明**的 x-ref 字段；r4 收紧 + r5 按
        // 引用方类型）。
        let mut payload = fresh.payload.clone();
        if let Some(o) = payload.as_object_mut() {
            for (k, v) in o.iter_mut() {
                if ref_fields.contains(k) && v.as_str() == Some(dup.record.envelope.id.as_str()) {
                    *v = serde_json::Value::String(canonical.record.envelope.id.clone());
                }
            }
        }
        let bytes_out = record::serialize_new(&env, &payload, &fresh.body);
        crate::platform::atomic_write(&ctx.store.abs(&r.rel), &bytes_out)
            .map_err(|e| MemError::io(r.rel.clone(), e))?;
        let nt = token::for_bytes(env.version, &bytes_out);
        log.append_simple(
            &env.id,
            &r.rel,
            Op::Merge,
            Some(fresh.envelope.version),
            Some(env.version),
            Some(&cur_tok.digest),
            Some(&nt.digest.clone()),
            &ctx.by,
        )?;
        touched.push(r.rel.clone());
    }

    // 2) canonical 收敛写（r4 B4）：**一次原子写 + 一条事件**——
    //    a) 指向 dup 的引用**删除**（dup 并入自身 = 自引用，link 协议禁止）；
    //    b) 别名移交（别名只作用于查找解析层，D11）。
    //    fresh_can 来自锁内重读且循环不再触碰 canonical → 版本链连续（old_v
    //    = fresh_can 版本，new_v = +1）。
    let mut env_can = fresh_can.envelope.clone();
    env_can.refs.retain(|e| e.target != dup.record.envelope.id);
    let mut aliases = env_can.aliases.clone();
    for a in &fresh_dup.envelope.aliases {
        if !aliases.contains(a) {
            aliases.push(a.clone());
        }
    }
    env_can.aliases = aliases;
    env_can.version += 1;
    env_can.modified = crate::clock::now_rfc3339();
    // r5 B4：canonical 自身 payload 的 x-ref 字段若指向 dup → 置 null（并入自身
    // = 自引用，与信封删边同构；payload 与 envelope 必须在同一次收敛写中一致）。
    let mut can_payload = fresh_can.payload.clone();
    if let Some(decl) = ctx.types.get(&fresh_can.envelope.r#type) {
        let ref_fields: Vec<String> = decl.ref_specs().keys().cloned().collect();
        if let Some(o) = can_payload.as_object_mut() {
            for (k, v) in o.iter_mut() {
                if ref_fields.contains(k) && v.as_str() == Some(dup.record.envelope.id.as_str()) {
                    *v = serde_json::Value::Null;
                }
            }
        }
    }
    let can_out = record::serialize_new(&env_can, &can_payload, &fresh_can.body);
    crate::platform::atomic_write(&ctx.store.abs(&canonical.rel), &can_out)
        .map_err(|e| MemError::io(canonical.rel.clone(), e))?;
    let nt_can = token::for_bytes(env_can.version, &can_out);
    log.append_simple(
        &env_can.id,
        &canonical.rel,
        Op::Merge,
        Some(fresh_can.envelope.version),
        Some(env_can.version),
        Some(&tok_can.digest),
        Some(&nt_can.digest.clone()),
        &ctx.by,
    )?;
    touched.push(canonical.rel.clone());

    // 3) tombstone 最后（D17）：dup 进 attic + merge 事件。
    let dup_attic = paths::attic_rel(&dup.rel)?;
    let attic_abs = ctx.store.abs(&dup_attic);
    std::fs::create_dir_all(attic_abs.parent().unwrap())
        .map_err(|e| MemError::io(attic_abs.display().to_string(), e))?;
    crate::platform::rename_noreplace(&ctx.store.abs(&dup.rel), &attic_abs)
        .map_err(|e| MemError::io(attic_abs.display().to_string(), e))?;
    log.append_simple(
        &fresh_dup.envelope.id,
        &dup_attic,
        Op::Merge,
        Some(fresh_dup.envelope.version),
        Some(fresh_dup.envelope.version),
        Some(&tok_dup.digest.clone()),
        Some(&tok_dup.digest.clone()),
        &ctx.by,
    )?;
    // H2：rename 语义双侧入批——旧 records 侧路径的删除也要进本次提交，
    // 否则 HEAD 与真相层长期分叉（worktree 永久脏）。
    touched.push(dup.rel.clone());
    touched.push(dup_attic.clone());

    // 4) 单提交（含 events.jsonl 同批）。
    commit_same_batch(ctx, &touched, 0)?;
    Ok(Merged {
        canonical: canonical.record.envelope.id.clone(),
        dup: dup.record.envelope.id.clone(),
        repointed: touched.len(),
        canonical_token: nt_can,
    })
}

#[derive(Debug)]
pub struct Merged {
    pub canonical: String,
    pub dup: String,
    pub repointed: usize,
    pub canonical_token: Token,
}

// ---------- fix（R6.3 修复通道） ----------

#[derive(Debug)]
pub struct Fixed {
    pub rel: String,
    pub record: Record,
    pub token: Token,
}

/// 修复记录（显式操作；事件 op=fix）。
///
/// - frontmatter 可解析：仅确定性再生成（规范化）+ 声明重校验（降级告警）；
/// - frontmatter 损坏：优先用调用方提供的 envelope/payload；否则从历史
///   （[`HistoryStore`]）取上一版 frontmatter + **保留当前正文字节**（红线27）；
///   两者皆无 → 响亮错误（绝不静默捏造）。
///
/// `committer`：git 同批提交汇（B1，实施评审）——fix 是机制写入，修复版本必须
/// 与 events.jsonl 同批入史（无 .git → 传 None，历史缺席合法红线10）；提交失败转
/// commit-pending，不得静默无历史：修复产物与事件登记摘要不一致的缺口会让随后
/// `restore-body history` 的锚不在 git 历史中（复合修复链 §3.6 断裂）。
pub fn fix_record(
    store: &Store,
    types: &TypeSet,
    id_or_path: &str,
    history: Option<&dyn HistoryStore>,
    provided: Option<(Envelope, serde_json::Value)>,
    expect: Option<&Token>,
    committer: Option<&dyn CommitSink>,
) -> MemResult<Fixed> {
    store.check_writable()?;
    crate::gitattrs::ensure_for_write(store)?; // BD5 ②③：受管写前核验
    // H1：定位（只读解析路径；真正的内容读取全部在锁内）。
    let rel = match get(store, id_or_path) {
        Ok(hit) => hit.rel,
        Err(MemError::MissingRecord { .. } | MemError::Corrupt { .. })
            if id_or_path.starts_with("records/") || id_or_path.starts_with("attic/") =>
        {
            paths::validate_store_record_path(store, id_or_path)?;
            id_or_path.to_string()
        }
        Err(e @ MemError::BadArgument(_)) => return Err(e),
        Err(_) => {
            let log = EventLog::new(store).read_log()?;
            log.events
                .iter()
                .rev()
                .find(|e| e.id == id_or_path)
                .map(|e| e.path.clone())
                .ok_or_else(|| MemError::MissingRecord {
                    id_or_path: id_or_path.into(),
                })?
        }
    };
    paths::validate_store_record_path(store, &rel)?;
    // fix 可能重新引入类型声明中的派生边；整个读—校验—写过程使用图级独占屏障，
    // 不在共享锁下检查后再升级，以免与其他 DAG 写入共同发布成环。
    let _barrier = lock::barrier_exclusive(store)?;
    store.check_compatible_for_write()?;
    let _rec = lock::record_lock(store, &rel, std::time::Duration::from_secs(30))?;
    // H1：锁内读——并发窗口内的 update 成果不会被 fix 的旧字节覆盖。
    let bytes = std::fs::read(store.abs(&rel)).map_err(|e| MemError::io(rel.clone(), e))?;
    let current = token_of_bytes(&bytes);
    let (env, payload, parseable) = match Record::from_bytes(&bytes) {
        Ok(rec) => {
            // 可解析：规范化再生成（确定性序列化）。红线19：可解析读改写必须 CAS。
            if let Some(expect) = expect {
                token::cas_check(expect, &current)?;
            } else {
                return Err(MemError::BadArgument(format!(
                    "fix 拒绝（{rel}）：记录可解析，必须携带 expect 令牌（红线19）；损坏记录的修复才豁免 CAS"
                )));
            }
            (rec.envelope, rec.payload, true)
        }
        Err(broken) => {
            // 损坏记录：无从取令牌（正文本体就是抢救对象），豁免 CAS；
            // 锁内读保证正文取最新字节。
            if let Some((e, p)) = provided {
                (e, p, false)
            } else if let Some(h) = history {
                let prev = h
                    .previous_file(&rel)?
                    .ok_or_else(|| MemError::BadArgument(format!(
                        "fix 失败（{rel}）：frontmatter 损坏（{broken}）且无历史版本可用——请通过 --envelope/--payload 提供重建原料（R6.3 双通道）"
                    )))?;
                let prev_rec = Record::from_bytes(&prev)
                    .map_err(|e| MemError::BadArgument(format!("历史版本也损坏（{rel}）：{e}")))?;
                (prev_rec.envelope, prev_rec.payload, false)
            } else {
                return Err(MemError::BadArgument(format!(
                    "fix 失败（{rel}）：frontmatter 损坏（{broken}）且未提供重建原料"
                )));
            }
        }
    };
    // 声明重校验（lenient 降级告警照常；strict 坏载荷 → 拒收，保持响亮）。
    if let Some(decl) = types.get(&env.r#type) {
        let violations = decl.validate_payload(&payload);
        if let mem_schema::Policy::Reject = decl.policy(&violations) {
            return Err(MemError::SchemaReject {
                type_name: env.r#type.clone(),
                violations,
            });
        }
        if decl.dag() {
            dag_candidate_precheck(
                &WriteContext::new(store, types, "mem fix").with_committer(committer),
                &env.id,
                &env.r#type,
                &env.refs,
            )?;
        }
    }
    let body = if parseable {
        Record::from_bytes(&bytes).expect("已验证可解析").body
    } else {
        split_body_bytes(&bytes)
    };
    let old_digest = token::sha256_hex(&bytes);
    let out = record::serialize_new(&env, &payload, &body);
    crate::platform::atomic_write(&store.abs(&rel), &out)
        .map_err(|e| MemError::io(rel.clone(), e))?;
    let new_token = token::for_bytes(env.version, &out);
    let log = EventLog::new(store);
    log.append_simple(
        &env.id,
        &rel,
        Op::Fix,
        Some(env.version),
        Some(env.version),
        Some(&old_digest),
        Some(&new_token.digest),
        "mem fix",
    )?;
    // B1：修复版本与事件同批入史（统一写路径提交纪律，D17）——失败转
    // commit-pending，不回滚真相层。
    commit_batch(store, committer, &[rel.clone()])?;
    Ok(Fixed {
        rel,
        record: Record::from_bytes(&out).expect("自产字节必可解析"),
        token: new_token,
    })
}

/// 从损坏文件字节中提取正文：**首个**闭合 `---` 行之后（D18"至下一个 `---`"；
/// 此前取最后一个 `---`，正文含水平线即静默丢字节——B2，修复通道违反红线27）。
fn split_body_bytes(bytes: &[u8]) -> Vec<u8> {
    crate::record::find_body_after_first_delimiter(bytes).unwrap_or_else(|_| bytes.to_vec())
}

/// 文件字节的当前令牌（version 取自信封；损坏时以 1 占位——CAS 只对可解析路径生效）。
fn token_of_bytes(bytes: &[u8]) -> Token {
    let version = Record::from_bytes(bytes)
        .map(|r| r.envelope.version)
        .unwrap_or(1);
    token::for_bytes(version.max(1), bytes)
}

// ---------- restore-body（R11 恢复通道，BD4） ----------

/// 恢复来源三形（§3.6）。
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum RestoreSource {
    /// `git:<版本>`——指定历史版本的正文区（来源可信性由调用方显式承担）。
    Git { rev: String },
    /// `file:<店外路径>`——店外备份的记录格式文件，取其正文区。
    File { path: std::path::PathBuf },
    /// `history`——自动锚定，仅在可机械判定的窗口内可用（见下）。
    History,
}

#[derive(Debug)]
#[non_exhaustive]
pub struct RestoredBody {
    pub rel: String,
    pub record: Record,
    pub token: Token,
    /// 来源出处回显（§3.6 机制保证）：来源形态、锚定摘要或版本、复制前/后正文区
    /// 摘要——供调用方复核。机制保证 = 字节完整复制；来源可信性由调用方确认，
    /// 两者显式分离。
    pub source_echo: serde_json::Value,
}

/// 恢复正文（经确认的内容写操作；R11.6/验收 6）。
///
/// 正文区替换为**来源字节**，信封（版本、时间戳除外）与载荷原样保留，走既有更新
/// 写路径（共享门闩/记录锁/全摘要令牌 CAS/事件追加/git 同批提交）；事件操作名
/// `restore_body`，携带恢复前/后全文摘要。
///
/// 与 fix 的分界（§3.6 操作组合）：结构头坏 → 先 `fix`（保正文字节的结构修复）；
/// 正文坏 → `restore-body`（换正文字节的内容写）。frontmatter 必须可解析——
/// 两者皆坏时本命令响亮拒绝（先 fix）。
///
/// `history` 自动锚定的**可用窗口**（两条件同时成立）：
/// 1. 漂移仍在：磁盘全文摘要 ≠ 最后事件登记摘要（成因唯一：磁盘被外部改动而
///    日志未登记——最后事件登记的恰是外部改动前机制写入的字节）；
/// 2. 最后事件是机制写入登记（kind=op，非回填——回填登记摘要描述外部字节，
///    可能是损坏本身，机械不可分辨，不得作可信锚）。
/// 窗口内：锚 = 最后事件登记摘要，自新至旧扫 git 历史取全文摘要相符的版本。
/// 窗口外/锚无匹配：**响亮拒绝**，绝不回退次优匹配、绝不把盘上字节冒充治愈；
/// 响应附可行动指引（最后机制写入登记摘要及匹配的历史版本，若找到）。
pub fn restore_body(
    ctx: &WriteContext,
    id_or_path: &str,
    expect: &Token,
    source: &RestoreSource,
    history: Option<&dyn BodyHistory>,
) -> MemResult<RestoredBody> {
    restore_body_inner(ctx, id_or_path, expect, source, history, None)
}

/// 仅供 debug 集成测试注入特性登记与依赖写之间的进程交错。
#[doc(hidden)]
#[cfg(all(debug_assertions, not(test)))]
pub fn restore_body_with_pause_hook_test(
    ctx: &WriteContext,
    id_or_path: &str,
    expect: &Token,
    source: &RestoreSource,
    history: Option<&dyn BodyHistory>,
    hook: crate::event::PauseHook,
) -> MemResult<RestoredBody> {
    restore_body_inner(
        ctx,
        id_or_path,
        expect,
        source,
        history,
        Some(hook.as_ref()),
    )
}

fn restore_body_inner(
    ctx: &WriteContext,
    id_or_path: &str,
    expect: &Token,
    source: &RestoreSource,
    history: Option<&dyn BodyHistory>,
    pause_hook: Option<&(dyn Fn(&str) + Send + Sync)>,
) -> MemResult<RestoredBody> {
    ctx.store.check_writable()?;
    let hit = get(ctx.store, id_or_path)?;
    if !hit.rel.starts_with("records/") {
        return Err(MemError::BadArgument(format!(
            "restore-body 仅支持 records/ 内的记录（attic 用 mem restore）：{}",
            hit.rel
        )));
    }
    let rel = hit.rel.clone();
    // restore_body 引入事件语义；在独占门闩内先登记，再在同一边界中写记录和事件。
    // 不能先取共享门闩再升级，否则与其他写者交错时会违反锁序或自锁。
    let barrier = lock::barrier_exclusive(ctx.store)?;
    ctx.store.check_compatible_for_write()?;
    let _rec = lock::record_lock(ctx.store, &rel, std::time::Duration::from_secs(30))?;
    // 锁内重读 + CAS（红线19：经确认的内容写仍是读改写）。
    let fresh_bytes =
        std::fs::read(ctx.store.abs(&rel)).map_err(|e| MemError::io(rel.clone(), e))?;
    // frontmatter 必须可解析（§3.6：两者皆坏 → 先 fix；响亮拒绝不静默）。
    let fresh = Record::from_bytes(&fresh_bytes)?;
    let current = token::for_bytes(fresh.envelope.version, &fresh_bytes);
    token::cas_check(expect, &current)?;

    let (source_body, mut echo) =
        resolve_restore_source(ctx, &rel, &fresh.envelope.id, &fresh_bytes, source, history)?;

    // 构造新字节：信封/载荷原样保留（版本+1、modified 刷新），正文替换。
    let mut env = fresh.envelope.clone();
    env.version += 1;
    env.modified = crate::clock::now_rfc3339();
    let out_bytes = record::serialize_new(&env, &fresh.payload, &source_body);
    // 机制保证：落盘正文区摘要 == 来源正文区摘要（不符即响亮失败、不留半成品）。
    let written_body = record::find_body_after_first_delimiter(&out_bytes)?;
    let source_digest = token::sha256_hex(&source_body);
    let written_digest = token::sha256_hex(&written_body);
    if written_digest != source_digest {
        return Err(MemError::Corrupt {
            path: rel.clone(),
            reason: format!(
                "restore-body 复制保真校验失败：落盘正文区摘要 {written_digest} ≠ 来源 {source_digest}（不留半成品）"
            ),
        });
    }
    echo["body_sha256_source"] = serde_json::json!(source_digest);
    echo["body_sha256_written"] = serde_json::json!(written_digest);
    ctx.store
        .register_required_features_under_exclusive(&["event.restore_body/v1"], &barrier)?;
    if let Some(hook) = pause_hook {
        hook("feature_registered");
    }
    crate::gitattrs::ensure_for_write(ctx.store)?; // BD5 ②③：受管写前核验
    crate::platform::atomic_write(&ctx.store.abs(&rel), &out_bytes)
        .map_err(|e| MemError::io(rel.clone(), e))?;
    if let Some(hook) = pause_hook {
        hook("record_published");
    }
    let new_token = token::for_bytes(env.version, &out_bytes);
    let log = EventLog::new(ctx.store);
    log.append_simple(
        &env.id,
        &rel,
        Op::RestoreBody,
        Some(fresh.envelope.version),
        Some(env.version),
        Some(&current.digest),
        Some(&new_token.digest),
        &ctx.by,
    )?;
    if let Some(hook) = pause_hook {
        hook("event_appended");
    }
    commit_same_batch(ctx, &[rel.clone()], 0)?;
    register_trail(ctx, &env.id);
    Ok(RestoredBody {
        rel,
        record: Record::from_bytes(&out_bytes).expect("自产字节必可解析"),
        token: new_token,
        source_echo: echo,
    })
}

/// 解析来源三形 → (来源正文字节, 出处回显)。
fn resolve_restore_source(
    ctx: &WriteContext,
    rel: &str,
    id: &str,
    fresh_bytes: &[u8],
    source: &RestoreSource,
    history: Option<&dyn BodyHistory>,
) -> MemResult<(Vec<u8>, serde_json::Value)> {
    match source {
        RestoreSource::Git { rev } => {
            // 无 .git → 整体结构化报错（git 缺席即无历史通道；file: 源仍可用）。
            if !ctx.store.root().join(".git").exists() {
                return Err(MemError::Git(
                    "无 .git——git: 源不可用（git 缺席即无历史通道；file: 源仍可用）".into(),
                ));
            }
            let h = history.ok_or_else(|| {
                MemError::BadArgument("git: 源需要历史通道（BodyHistory）".into())
            })?;
            let bytes = h
                .file_bytes_at(rel, rev)?
                .ok_or_else(|| MemError::BadArgument(format!("历史版本 {rev} 无该文件：{rel}")))?;
            let body = record::find_body_after_first_delimiter(&bytes)?;
            Ok((body, serde_json::json!({ "form": "git", "rev": rev })))
        }
        RestoreSource::File { path } => {
            if crate::paths::path_is_inside(ctx.store.root(), path) {
                return Err(MemError::BadArgument(format!(
                    "file: 源必须位于店外（{} 在店目录内）——防旁挂写入",
                    path.display()
                )));
            }
            let bytes = std::fs::read(path).map_err(|e| {
                MemError::BadArgument(format!("file: 源不可读（{}）：{e}", path.display()))
            })?;
            let body = record::find_body_after_first_delimiter(&bytes)?;
            Ok((
                body,
                serde_json::json!({ "form": "file", "path": path.display().to_string() }),
            ))
        }
        RestoreSource::History => {
            // 无 .git → 整体结构化报错（窗口判定之前——git 缺席即无历史通道）。
            if !ctx.store.root().join(".git").exists() {
                return Err(MemError::Git(
                    "无 .git——history 源不可用（git 缺席即无历史通道；file: 源仍可用）".into(),
                ));
            }
            let h = history.ok_or_else(|| {
                MemError::BadArgument("history 源需要历史通道（BodyHistory）".into())
            })?;
            let log = EventLog::new(ctx.store).read_log()?;
            let last = log.events.iter().rev().find(|e| e.id == id);
            let Some(last) = last else {
                return Err(MemError::BadArgument(format!(
                    "history 锚定失败（{rel}）：无事件历史——改用显式来源 git:<版本> 或 file:<店外路径>"
                )));
            };
            let disk_digest = token::sha256_hex(fresh_bytes);
            let drifted = last.new_digest.as_deref() != Some(disk_digest.as_str());
            let mech_last = last.kind == Kind::Op;
            if !drifted || !mech_last {
                return Err(history_window_rejection(ctx, h, id, rel));
            }
            let anchor = last.new_digest.clone().ok_or_else(|| {
                MemError::BadArgument(format!("history 锚定失败（{rel}）：最后事件无登记摘要"))
            })?;
            // 自新至旧扫历史取全文摘要相符的版本（典型库首数个版本即命中）。
            for sha in h.list_versions(rel)? {
                if let Some(bytes) = h.file_bytes_at(rel, &sha)? {
                    if token::sha256_hex(&bytes) == anchor {
                        let body = record::find_body_after_first_delimiter(&bytes)?;
                        return Ok((
                            body,
                            serde_json::json!({ "form": "history", "anchor": anchor, "commit": sha }),
                        ));
                    }
                }
            }
            // 无匹配：响亮失败、区分成因，绝不回退次优匹配。
            let pending = list_pending_commits(ctx.store)?;
            Err(MemError::BadArgument(format!(
                "history 锚定失败（{rel}）：锚摘要 {anchor} 未在 git 历史中找到——{}；改用显式来源 git:<版本> 或 file:<店外路径>",
                if pending.is_empty() {
                    "锚版本不在跟踪范围或历史被改写"
                } else {
                    "commit-pending 未收敛（等待 mem reconcile 重试提交后重试）"
                }
            )))
        }
    }
}

/// 窗口外拒绝：附可行动指引——该记录最后**机制写入**登记的摘要，及 git 历史中
/// 匹配该摘要的版本（若找到），引导显式 `git:<版本>` 或 `file:`（绝不把盘上
/// （可能损坏的）字节写成新版本冒充治愈）。
fn history_window_rejection(
    ctx: &WriteContext,
    h: &dyn BodyHistory,
    id: &str,
    rel: &str,
) -> MemError {
    let log = EventLog::new(ctx.store);
    let last_mech = log.read_log().ok().and_then(|l| {
        l.events
            .iter()
            .rev()
            .find(|e| e.id == id && e.kind == Kind::Op && e.new_digest.is_some())
            .cloned()
    });
    let Some(mech) = last_mech else {
        return MemError::BadArgument(format!(
            "history 自动锚定拒绝（{rel}）：窗口外（漂移已收敛或最后事件为回填）——机械上无法推出损坏前版本，不得把盘上字节冒充治愈；无机制写入事件可指引——改用显式来源 git:<版本> 或 file:<店外路径>"
        ));
    };
    let digest = mech.new_digest.clone().unwrap_or_default();
    // git 历史中匹配该摘要的版本（若找到）。
    let matched = h.list_versions(rel).ok().and_then(|versions| {
        versions.into_iter().find(|sha| {
            h.file_bytes_at(rel, sha)
                .ok()
                .flatten()
                .map(|b| token::sha256_hex(&b) == digest)
                .unwrap_or(false)
        })
    });
    let guidance = match matched {
        Some(sha) => format!(
            "该记录最后机制写入登记摘要 {digest}，对应历史版本 {sha}——显式指定 --source git:{sha} 或 file:<店外路径>"
        ),
        None => format!(
            "该记录最后机制写入登记摘要 {digest}（git 历史中无匹配版本）——显式指定 git:<版本> 或 file:<店外路径>"
        ),
    };
    MemError::BadArgument(format!(
        "history 自动锚定拒绝（{rel}）：窗口外（漂移已收敛或最后事件为回填）——机械上无法推出损坏前版本，不得把盘上字节冒充治愈。{guidance}"
    ))
}

/// --deep 维护审计（H3）：**磁盘摘要 vs 事件日志最后 new_digest**。
///
/// 此前实现是"scan 时算一次、再读同文件算一次"的自比自——任何漂移都检不出。
/// 真实对照物是事件日志：不一致 = 保版本手改/漂移（回填待执行）或日志落后。
/// 返回人读报告（不拒收——审计例程，R5.5）。
pub fn deep_digest_audit(store: &Store) -> MemResult<Vec<String>> {
    let scan = scan_all(store)?;
    let log = EventLog::new(store).read_log()?;
    let mut out = Vec::new();
    for r in &scan.records {
        let bytes = std::fs::read(store.abs(&r.rel)).unwrap_or_default();
        let disk = token::sha256_hex(&bytes);
        let last = log
            .events
            .iter()
            .rev()
            .find(|e| e.id == r.record.envelope.id);
        match last {
            None => out.push(format!(
                "无事件记录（--deep）：{}（创建事件缺失——backfill 待执行）",
                r.rel
            )),
            Some(ev) => {
                if ev.new_digest.as_deref() != Some(disk.as_str()) {
                    out.push(format!(
                        "磁盘与事件日志不一致（--deep）：{}（磁盘 {disk} ≠ 日志 {:?}）——保版本手改/漂移（backfill 待执行）或日志落后",
                        r.rel,
                        ev.new_digest.as_deref().unwrap_or("null")
                    ));
                }
            }
        }
    }
    Ok(out)
}

/// 镜像一致性检查（r5 B4 验收：verify 不得对 payload/envelope 分裂显示全绿）。
///
/// 镜像纪律（写路径维持）：payload 的 x-ref 字段（非 null）必须镜像进
/// envelope.refs（rel = 字段名）。此检查按类型声明机械核对两个方向：
/// a) payload x-ref 值在 envelope.refs 中有对应镜像；
/// b) 镜像目标（与 payload 值）指向的记录必须存在（悬空 = 断链）。
/// 需要类型声明（x-ref 字段集合），types 缺声明时跳过该类型（无法判定）。
pub fn mirror_consistency_audit(
    store: &Store,
    types: &mem_schema::TypeSet,
) -> MemResult<Vec<String>> {
    let scan = scan_all(store)?;
    let mut out = Vec::new();
    let ids: std::collections::HashSet<_> = scan
        .records
        .iter()
        .map(|r| r.record.envelope.id.clone())
        .collect();
    for r in &scan.records {
        let Some(decl) = types.get(&r.record.envelope.r#type) else {
            continue;
        };
        let refs = decl.ref_specs();
        let obj = match r.record.payload.as_object() {
            Some(o) => o,
            None => continue,
        };
        for (field, spec) in refs {
            let pv = obj.get(field);
            let mirror = r.record.envelope.refs.iter().find(|e| e.rel == *field);
            match pv {
                Some(serde_json::Value::String(id)) => {
                    // payload 声明引用 → 镜像必须存在且同 target。
                    match mirror {
                        None => out.push(format!(
                            "镜像缺失：{} payload.{}={} 无 envelope 镜像（{}）",
                            r.rel, field, id, r.record.envelope.id
                        )),
                        Some(m) if m.target != *id => out.push(format!(
                            "镜像分裂：{} payload.{}={} 但 envelope.{}={}（{}）",
                            r.rel, field, id, m.rel, m.target, r.record.envelope.id
                        )),
                        _ => {}
                    }
                    // payload 悬空（R3.4 显式报告语义）。
                    if spec.existence && !ids.contains(id) {
                        out.push(format!(
                            "payload 悬空：{} payload.{} -> {} 不存在（{}）",
                            r.rel, field, id, r.record.envelope.id
                        ));
                    }
                }
                Some(serde_json::Value::Null) | None => {}
                Some(_) => {}
            }
        }
    }
    Ok(out)
}

/// 归属完整性审计（方案 principal-binding 决策⑦，评审 B7）。
///
/// 非空 `envelope.principal` 的记录，其目标必须仍为 records/ 中现存的 principal
/// 记录；断链按既有断链注记形态报告三要素（来源记录 from、目标身份 target、
/// 修复入口 repair）。修复入口按缺位形态分写（终审定稿措辞）：目标在隔离区
/// （受管删除的 tombstone）→ "恢复身份记录"；目标缺失（外部删除造成的 absent，
/// 无受管 tombstone）→ 如实报告并给历史/备份恢复指引——恢复通道只对隔离区
/// tombstone 必然可用，不笼统承诺同一入口。
///
/// 与镜像审计的关系：执行层统一扫描引用镜像与归属两个字段域（不把归属隐式
/// 复制进 envelope.refs——信封字段与公开格式零变更，B1 契约）。agenda 不在
/// 本审计的扫描范围（决策⑦）。
pub fn principal_binding_audit(store: &Store) -> MemResult<Vec<serde_json::Value>> {
    let scan = scan_all(store)?;
    let principal_ids: std::collections::HashSet<_> = scan
        .records
        .iter()
        .filter(|r| r.record.envelope.r#type == "principal")
        .map(|r| r.record.envelope.id.clone())
        .collect();
    // 隔离区中的身份 id（受管 tombstone；坏文件跳过——坏条目审计另行覆盖）。
    let mut attic_ids = std::collections::HashSet::new();
    let attic_dir = store.abs("attic/principal");
    if attic_dir.is_dir() {
        for entry in
            std::fs::read_dir(&attic_dir).map_err(|e| MemError::io("attic/principal", e))?
        {
            let path = match entry {
                Ok(e) => e.path(),
                Err(e) => return Err(MemError::io("attic/principal", e)),
            };
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            if let Ok(bytes) = std::fs::read(&path) {
                if let Ok(rec) = Record::from_bytes(&bytes) {
                    attic_ids.insert(rec.envelope.id.clone());
                }
            }
        }
    }
    let mut out = Vec::new();
    for r in &scan.records {
        let Some(p) = r.record.envelope.principal.as_deref() else {
            continue;
        };
        if principal_ids.contains(p) {
            continue;
        }
        let repair = if attic_ids.contains(p) {
            format!("身份已被删除进隔离区——恢复身份记录（mem restore {p}）")
        } else {
            "身份缺失（外部删除，无受管 tombstone）——如实报告缺失；可从历史/备份恢复身份后重建归属"
                .to_string()
        };
        out.push(serde_json::json!({
            "from": r.record.envelope.id,
            "rel": "principal",
            "target": p,
            "repair": repair,
        }));
    }
    Ok(out)
}
