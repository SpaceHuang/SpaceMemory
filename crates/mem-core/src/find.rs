//! 检索与过滤清单的执行器单源（R10 决策②——CLI 与 MCP 两前端同源调用，
//! keyword-search-design.md §2.1–§2.5）。
//!
//! 职责边界：
//! - [`parse_query`]：词项文法（决策①）——Unicode 空白切分、`-` 否定标记、
//!   空查询与纯否定显式拒收（R10.9）；
//! - 组合谓词 [`record_matches_query`]：∀正词项命中 ∧ ∀负词项不命中——谓词本体
//!   仍是 [`crate::search::record_matches`] 单源（R10.2/R10.8），本模块只做外层包装；
//! - 过滤执行器（决策③④）：规范条件 = 字段＋标量文本；声明检查按候选类型集；
//!   精筛按每条记录自身类型的声明裁定等值；跨类型同名字段异形 = 合并解释（A∪B）；
//! - 双执行器：indexed（注入的已证明句柄粗筛 → 逐候选回读真相层 → 同一精筛）与
//!   direct（全量直读 → 同一精筛）。分流依据只有一个："是否拿到已证明句柄"——
//!   证明逻辑不在本模块，`open_proved`（mem-index）是唯一证明点（红线21）。
//!
//! 红线对账（§2.8）：
//! - 8：粗筛只提供超集候选，命中由谓词单源精筛裁定；candidates/bad_records 任一
//!   失败 → 回退 direct 全量应答（reason 携带真实错误），不转空命中/空告警；
//! - 9：offset = 命中序号；截断显式标注 + resume 续读；告警截断同型；
//! - 18：判定与直读全内存（indexed 读只读打开索引，由构造函数保证）；
//! - 21：本模块消费"已证明句柄"，不自行判定新鲜性。

use std::collections::BTreeSet;

use crate::envelope::Envelope;
use crate::error::{MemError, MemResult};
use crate::freshness::ReadMode;
use crate::ops::{self, Scanned};
use crate::paths::fold;
use crate::search::record_matches;
use crate::store::Store;
use mem_schema::{FilterShape, TypeSet};

/// 显式错误 kind（§2.7：4 枚，可机判，两前端同码）。
pub const QUERY_INVALID: &str = "query_invalid";
pub const UNKNOWN_FILTER_FIELD: &str = "unknown_filter_field";
pub const DUPLICATE_FILTER_FIELD: &str = "duplicate_filter_field";
pub const INVALID_FILTER_VALUE: &str = "invalid_filter_value";

fn reject(kind: &str, message: impl Into<String>) -> MemError {
    MemError::ParamRejected { kind: kind.to_string(), message: message.into() }
}

// ---------- 查询文法（决策①，§2.2） ----------

/// 词项：raw 供响应回显，folded 供匹配（执行器层折叠一次，逐记录循环不重复折叠）。
#[derive(Debug, Clone)]
pub struct Term {
    pub raw: String,
    pub folded: String,
}

/// 解析后的查询：肯定/否定词项（至少一个肯定词项）。
#[derive(Debug, Clone)]
pub struct ParsedQuery {
    pub positive: Vec<Term>,
    pub negative: Vec<Term>,
}

/// 词项文法（§2.2）：
/// - 空白 = Unicode White_Space（`char::is_whitespace`，含 U+3000 全角空格）；
/// - 否定词项 = `-` 前缀后随至少一个字符（仅半角 U+002D）；匹配串 = 去前导 `-`；
/// - 恰为 `-` 的词项 = 字面肯定词项；`--x` = 否定词项，匹配串 `-x`；
/// - 空查询 / 纯否定 → `query_invalid`（R10.9：绝不表现为空命中清单）。
pub fn parse_query(query: &str) -> MemResult<ParsedQuery> {
    let mut positive = Vec::new();
    let mut negative = Vec::new();
    for term in query.split_whitespace() {
        match term.strip_prefix('-') {
            Some("") => positive.push(Term { raw: term.to_string(), folded: fold(term) }),
            Some(rest) => negative.push(Term { raw: term.to_string(), folded: fold(rest) }),
            None => positive.push(Term { raw: term.to_string(), folded: fold(term) }),
        }
    }
    if positive.is_empty() {
        return Err(reject(
            QUERY_INVALID,
            "查询须至少含一个肯定词项（空查询与纯否定查询被拒收，R10.9；纯过滤清单请走 mem list / mem_list）",
        ));
    }
    Ok(ParsedQuery { positive, negative })
}

/// 组合谓词（§2.3，外层包装，谓词本体不变）：
/// 命中(r) := ∀p ∈ 肯定词项: record_matches(r, fold(p)) ∧ ∀n ∈ 否定词项: ¬record_matches(r, fold(n))。
pub fn record_matches_query(env: &Envelope, payload: &serde_json::Value, body: &[u8], q: &ParsedQuery) -> bool {
    q.positive.iter().all(|t| record_matches(&env.id, &env.aliases, payload, body, &t.folded))
        && q.negative.iter().all(|t| !record_matches(&env.id, &env.aliases, payload, body, &t.folded))
}

// ---------- 过滤执行器（决策③④，§2.4） ----------

/// 规范过滤条件 = 字段＋标量文本（决策④）：CLI 值原样为文本、MCP filters 值一律
/// JSON 字符串——两端在此汇合同域，合法条件空间逐字相同。
#[derive(Debug, Clone)]
pub struct FilterCond {
    pub field: String,
    pub text: String,
}

/// 解析后的过滤条件：兼容类型集 = 候选类型集中能按自身声明解释该文本的类型
/// （排序确定，供收窄交集与对账）。
struct ResolvedFilter {
    field: String,
    text: String,
    compatible: Vec<String>,
}

/// 类型 T 能否按自身声明解释该文本（兼容判定）。Array 恒可解释——运行时按元素
/// 实际形态裁定，兼容面只影响 indexed 收窄（只会多不会漏，正确性在精筛）。
fn shape_admits_text(shape: &FilterShape, text: &str) -> bool {
    match shape {
        FilterShape::Text => true,
        FilterShape::Number => parse_number_text(text).is_some(),
        FilterShape::Bool => matches!(text, "true" | "false"),
        FilterShape::Array => true,
        FilterShape::Unsupported => false,
    }
}

/// 严格 JSON 数值文法解析（决策④："1" 与 "1.0" 同值；"01"/"+1"/空串非法）。
fn parse_number_text(text: &str) -> Option<serde_json::Number> {
    serde_json::from_str::<serde_json::Number>(text).ok()
}

/// 数值等值：整数按整数比较，其余按 f64（serde_json 的 PosInt/Float 变体不互通，
/// "1" 与 "1.0" 必须同值——决策④）。
fn number_eq(a: &serde_json::Number, b: &serde_json::Number) -> bool {
    if let (Some(x), Some(y)) = (a.as_i64(), b.as_i64()) {
        return x == y;
    }
    if let (Some(x), Some(y)) = (a.as_u64(), b.as_u64()) {
        return x == y;
    }
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// 数组元素等值：按元素实际 JSON 形态裁定（§2.4 等值语义）。
fn array_elem_matches(elem: &serde_json::Value, text: &str) -> bool {
    match elem {
        serde_json::Value::String(s) => fold(s) == fold(text),
        serde_json::Value::Number(n) => {
            parse_number_text(text).map(|p| number_eq(&p, n)).unwrap_or(false)
        }
        serde_json::Value::Bool(b) => text == b.to_string(),
        _ => false, // 嵌套数组/对象/null：不命中不计错
    }
}

/// 过滤值等值（按记录自身类型的声明形态裁定；字段缺失/形态不符 → 不命中不计错，
/// R8.7/R10.10 降级承接）。
fn filter_value_matches(shape: &FilterShape, payload: &serde_json::Value, field: &str, text: &str) -> bool {
    let Some(actual) = payload.get(field) else { return false };
    match shape {
        FilterShape::Text => {
            actual.as_str().map(|s| fold(s) == fold(text)).unwrap_or(false)
        }
        FilterShape::Number => {
            actual.as_number().map(|n| {
                parse_number_text(text).map(|p| number_eq(&p, n)).unwrap_or(false)
            }).unwrap_or(false)
        }
        FilterShape::Bool => {
            actual.as_bool().map(|b| text == b.to_string()).unwrap_or(false)
        }
        FilterShape::Array => {
            actual.as_array().map(|a| a.iter().any(|e| array_elem_matches(e, text))).unwrap_or(false)
        }
        FilterShape::Unsupported => false,
    }
}

/// x-filter 声明审计（R10 决策③配套规则）：支持形态之外的属性声明了 x-filter →
/// `filter_shape_unsupported` 告警（verify 透出；运行时对该字段过滤 → 兼容类型集
/// 为空 → invalid_filter_value）。点名属性缺失在声明加载即响亮失败（与 x-ref
/// 同族，mem-schema LoadError::UnknownFilterField），故此处只剩形态检查。
pub fn filter_declaration_audit(types: &TypeSet) -> Vec<String> {
    let mut out = Vec::new();
    for d in types.decls() {
        for (field, shape) in d.filter_fields() {
            if !shape.is_supported() {
                out.push(format!(
                    "filter_shape_unsupported：{} 字段 {} 的声明形态不在 v1 支持集 \
                     （string/number/boolean/元素为标量的 array）——对该字段过滤将报 invalid_filter_value",
                    d.type_name(),
                    field
                ));
            }
        }
    }
    out
}

/// 候选类型集（§2.4）：type 参数/view 限定到的类型集，未限定时 = 全店已加载类型集。
fn candidate_types(types: &TypeSet, type_narrow: Option<&str>, view_types: &[String]) -> Vec<String> {
    if !view_types.is_empty() {
        return view_types.to_vec();
    }
    match type_narrow {
        Some(t) => vec![t.to_string()],
        None => types.decls().map(|d| d.type_name().to_string()).collect(),
    }
}

/// 声明检查与解析（唯一规则，§2.4）：
/// - 同一字段重复 → `duplicate_filter_field`；
/// - 空文本 → `invalid_filter_value`；
/// - 候选集中无任何类型声明 → `unknown_filter_field`；
/// - 兼容类型集为空（含形态不支持）→ `invalid_filter_value`。
fn resolve_filters(
    types: &TypeSet,
    candidate_types: &[String],
    filters: &[FilterCond],
) -> MemResult<Vec<ResolvedFilter>> {
    let mut seen = BTreeSet::new();
    for f in filters {
        if !seen.insert(f.field.as_str()) {
            return Err(reject(
                DUPLICATE_FILTER_FIELD,
                format!("过滤字段重复：{}（CLI --filter 可重复但同一字段只能出现一次）", f.field),
            ));
        }
    }
    let mut out = Vec::with_capacity(filters.len());
    for f in filters {
        if f.text.is_empty() {
            return Err(reject(
                INVALID_FILTER_VALUE,
                format!("过滤值不能为空（字段 {}）", f.field),
            ));
        }
        let declaring: Vec<&str> = candidate_types
            .iter()
            .filter(|t| {
                types.get(t).map(|d| d.filter_fields().contains_key(&f.field)).unwrap_or(false)
            })
            .map(|t| t.as_str())
            .collect();
        if declaring.is_empty() {
            return Err(reject(
                UNKNOWN_FILTER_FIELD,
                format!(
                    "候选类型集中无任何类型声明可过滤字段 {}（防手滑静默空结果；用 --type 收窄或检查声明）",
                    f.field
                ),
            ));
        }
        let compatible: Vec<String> = declaring
            .into_iter()
            .filter(|t| {
                let d = types.get(t).expect("declaring 已核验存在");
                shape_admits_text(d.filter_fields().get(&f.field).expect("declaring 已核验声明"), &f.text)
            })
            .map(|t| t.to_string())
            .collect();
        if compatible.is_empty() {
            return Err(reject(
                INVALID_FILTER_VALUE,
                format!(
                    "过滤值 {:?} 与候选集中全部声明字段 {} 的类型均不兼容（兼容类型集为空；形态不支持过滤的字段同型拒收）",
                    f.text, f.field
                ),
            ));
        }
        out.push(ResolvedFilter { field: f.field.clone(), text: f.text.clone(), compatible });
    }
    Ok(out)
}

/// 多条件的收窄类型集 = 各兼容类型集的交集（排序确定）。
fn narrowing_of(resolved: &[ResolvedFilter]) -> Option<Vec<String>> {
    let mut iter = resolved.iter();
    let first = iter.next()?;
    let mut acc: BTreeSet<String> = first.compatible.iter().cloned().collect();
    for f in iter {
        let cur: BTreeSet<String> = f.compatible.iter().cloned().collect();
        acc = acc.intersection(&cur).cloned().collect();
    }
    Some(acc.into_iter().collect())
}

/// 精筛（每条记录，同一函数服务两执行器）：类型限定 ∧ 组合谓词 ∧ 过滤。
fn record_passes(
    rec: &Scanned,
    pq: Option<&ParsedQuery>,
    view: Option<&crate::view::ViewFilter>,
    resolved: &[ResolvedFilter],
    types: &TypeSet,
    type_narrow: Option<&str>,
) -> bool {
    let env = &rec.record.envelope;
    if let Some(t) = type_narrow {
        if env.r#type != t {
            return false;
        }
    }
    if let Some(vf) = view {
        if !vf.matches(env) {
            return false;
        }
    }
    if let Some(q) = pq {
        if !record_matches_query(env, &rec.record.payload, &rec.record.body, q) {
            return false;
        }
    }
    if !resolved.is_empty() {
        // 精筛按每条记录自身类型的声明裁定等值（评审 B2）；未声明类型 / 未声明
        // 字段 / 形态不符声明 → 不命中、不报错（§2.4 降级承接）。
        let Some(decl) = types.get(&env.r#type) else { return false };
        for f in resolved {
            match decl.filter_fields().get(&f.field) {
                Some(shape) => {
                    if !filter_value_matches(shape, &rec.record.payload, &f.field, &f.text) {
                        return false;
                    }
                }
                None => return false,
            }
        }
    }
    true
}

// ---------- 索引粗筛句柄（决策②，§2.1） ----------

/// 粗筛候选行。find（词项/过滤在场）只消费 id + rel（真相回读提供其余字段）；
/// 纯过滤清单（无词项无过滤）在新鲜性已证明的前提下直接消费全部字段（与既有
/// list 快路径同信任级——行为等价迁移）。
#[derive(Debug, Clone)]
pub struct PrefilterRow {
    pub id: String,
    pub type_name: String,
    pub rel: String,
    pub version: u64,
    pub digest: String,
    pub modified: String,
}

/// 已证明索引句柄的粗筛接口（决策②）。**证明逻辑不出现在 trait**——epoch/schema
/// 门禁在唯一构造函数（mem-index::open_proved）内完成；实现方只承载两个查询。
pub trait FindPrefilter: Send + Sync {
    /// 超集候选：正词项折叠串合取（可为空 = 无文本条件）+ 可选类型收窄。
    /// 每词项 LIKE 条件是其谓词命中的超集（只多不会漏，红线8）；负词项与过滤
    /// 不下推（§2.1）。失败 → 调用方回退 direct（不转空命中）。
    fn candidates(&self, terms_folded: &[String], types: Option<&[String]>) -> MemResult<Vec<PrefilterRow>>;

    /// 坏记录清单：(rel, 告警行) 按 rel 升序最多 cap 条 + 是否还有更多。
    /// 失败 → 调用方回退 direct（禁止空告警的 indexed，决策⑦）。
    fn bad_records(&self, cap: usize) -> MemResult<(Vec<(String, String)>, bool)>;
}

// ---------- 执行器入口 ----------

/// 命中条目 = 定位信息 + 钻取入口（id 与完整修订令牌，红线19；不携带全文，R10.7）。
#[derive(Debug, Clone)]
pub struct Hit {
    pub id: String,
    pub type_name: String,
    pub rel: String,
    pub modified: String,
    pub version: u64,
    pub token: String,
}

#[derive(Debug)]
pub struct FindOutcome {
    pub mode: ReadMode,
    pub hits: Vec<Hit>,
    pub truncated: bool,
    /// 续读路径（红线9）：命中序号。
    pub resume_offset: Option<usize>,
    /// 查询回显（原始词项；find 用，纯过滤清单为空）。
    pub positive_terms: Vec<String>,
    pub negative_terms: Vec<String>,
    /// 过滤回显（字段→文本）。
    pub filters_applied: Vec<(String, String)>,
    /// 坏记录告警（三要素模板；rel 升序、上限 list_limit_max）。
    pub warnings: Vec<String>,
    pub warnings_truncated: bool,
}

/// 执行参数。不变量：`prefilter` 为 Some 时 `mode` 必为 Indexed（两前端经
/// open_proved 组装——拿到句柄即已证明，mode 是同一构造的产物）。
pub struct FindParams<'a> {
    pub store: &'a Store,
    pub types: &'a TypeSet,
    /// None = 纯过滤清单（list 语义，无文本词项）；Some = 检索（须含肯定词项）。
    pub query: Option<&'a str>,
    pub filters: &'a [FilterCond],
    /// list --type 的候选类型限定（find 恒 None——检索的类型限定是基线未来项）。
    pub type_narrow: Option<&'a str>,
    /// list --view 的信封级过滤（声明式查询模板；v1 视图路径恒直读）。
    pub view: Option<&'a crate::view::ViewFilter>,
    pub limit: usize,
    pub offset: usize,
    pub mode: ReadMode,
    pub prefilter: Option<&'a dyn FindPrefilter>,
}

/// 执行器单源入口（两前端同源调用，§2.1 调用链）。
pub fn execute(params: FindParams) -> MemResult<FindOutcome> {
    let cap = params.store.config().budgets.list_limit_max as usize;
    let pq = params.query.map(parse_query).transpose()?;
    let view_types: Vec<String> = params
        .view
        .map(|v| v.types.clone())
        .unwrap_or_default();
    let cands = candidate_types(params.types, params.type_narrow, &view_types);
    let resolved = resolve_filters(params.types, &cands, params.filters)?;

    // indexed 尝试（分流依据唯一：是否拿到已证明句柄——§2.1）。视图路径 v1 恒直读。
    if params.view.is_none() {
        if let Some(pf) = params.prefilter {
            match try_indexed(&params, pf, pq.as_ref(), &resolved, cap) {
                Ok(outcome) => return Ok(outcome),
                Err(reason) => {
                    // 放弃本次 indexed 应答、回退 direct 全量应答——reason 携带真实
                    // 错误；不得把失败转为空命中或空告警（红线8/决策⑦）。
                    let fallback_mode = ReadMode::Direct { reason };
                    return run_direct(params, pq.as_ref(), &resolved, fallback_mode);
                }
            }
        }
    }
    let mode = params.mode.clone();
    run_direct(params, pq.as_ref(), &resolved, mode)
}

/// indexed 路径（§2.1 4a）：粗筛超集 → 逐候选回读真相层 → 同一精筛 → 命中序分页；
/// 告警取自 bad_records（同一次物化扫描）。
fn try_indexed(
    params: &FindParams,
    pf: &dyn FindPrefilter,
    pq: Option<&ParsedQuery>,
    resolved: &[ResolvedFilter],
    cap: usize,
) -> Result<FindOutcome, String> {
    let terms: Vec<String> = pq
        .map(|q| q.positive.iter().map(|t| t.folded.clone()).collect())
        .unwrap_or_default();
    // 收窄：多条件 = 兼容集交集；纯词项查询无类型收窄；无过滤的清单按 type 收窄。
    let narrowing = if !resolved.is_empty() {
        narrowing_of(resolved)
    } else {
        params.type_narrow.map(|t| vec![t.to_string()])
    };
    let rows = pf
        .candidates(&terms, narrowing.as_deref())
        .map_err(|e| format!("索引候选查询失败——放弃 indexed 应答，回退 direct（红线8）：{e}"))?;
    let (bad, bad_more) = pf
        .bad_records(cap)
        .map_err(|e| format!("坏记录清单读取失败——放弃 indexed 应答，回退 direct（红线8）：{e}"))?;

    // 纯过滤清单（无词项无过滤）：新鲜性已证明 → 索引行即答案（与既有 list 快路径
    // 同信任级，行为等价迁移）；其余一律回读真相层精筛（红线8：索引不 gate 正确性）。
    let hits = if pq.is_none() && resolved.is_empty() {
        rows.iter()
            .map(|r| Hit {
                id: r.id.clone(),
                type_name: r.type_name.clone(),
                rel: r.rel.clone(),
                modified: r.modified.clone(),
                version: r.version,
                token: format!("v{}@{}", r.version, r.digest),
            })
            .collect()
    } else {
        let mut hits = Vec::new();
        for row in &rows {
            // 文件已消失（删除竞态残影）→ 跳过；路径漂移 → 按 id 兜底解析。
            if let Some(rec) = read_candidate(params.store, &row.rel, &row.id) {
                if record_passes(&rec, pq, params.view, resolved, params.types, params.type_narrow) {
                    hits.push(Hit {
                        id: rec.record.envelope.id.clone(),
                        type_name: rec.record.envelope.r#type.clone(),
                        rel: rec.rel,
                        modified: rec.record.envelope.modified.clone(),
                        version: rec.record.envelope.version,
                        token: rec.token.to_string(),
                    });
                }
            }
        }
        hits
    };
    Ok(assemble(params.mode.clone(), hits, bad, bad_more, params.limit, params.offset, pq, &params.filters))
}

/// direct 路径（§2.1 4b）：全量直读真相层 → 同一精筛 → 命中序分页；告警取自
/// 本次扫描收集（与 indexed 同模板同排序——排序先行保证截断子集一致，§2.5）。
fn run_direct(
    params: FindParams,
    pq: Option<&ParsedQuery>,
    resolved: &[ResolvedFilter],
    mode: ReadMode,
) -> MemResult<FindOutcome> {
    let cap = params.store.config().budgets.list_limit_max as usize;
    let scan = ops::scan_all(params.store)?;
    // 与 indexed 同序（modified DESC, id DESC）——两执行器命中面与顺序结构性等价。
    let mut records = scan.records;
    records.sort_by(|a, b| {
        b.record.envelope.modified.cmp(&a.record.envelope.modified)
            .then(b.record.envelope.id.cmp(&a.record.envelope.id))
    });
    let mut hits = Vec::new();
    for rec in &records {
        if record_passes(rec, pq, params.view, resolved, params.types, params.type_narrow) {
            hits.push(Hit {
                id: rec.record.envelope.id.clone(),
                type_name: rec.record.envelope.r#type.clone(),
                rel: rec.rel.clone(),
                modified: rec.record.envelope.modified.clone(),
                version: rec.record.envelope.version,
                token: rec.token.to_string(),
            });
        }
    }
    let mut bad = scan.bad;
    bad.sort_by(|a, b| a.0.cmp(&b.0));
    let bad_more = bad.len() > cap;
    bad.truncate(cap);
    Ok(assemble(mode, hits, bad, bad_more, params.limit, params.offset, pq, &params.filters))
}

/// 逐候选回读真相层（红线8：索引不 gate 正确性——精筛按磁盘真相裁定）。
/// rel 直读失败或 id 不符（路径漂移残影）→ 按 id 兜底解析；仍缺失 → 跳过。
fn read_candidate(store: &Store, rel: &str, id: &str) -> Option<Scanned> {
    if let Ok(bytes) = std::fs::read(store.abs(rel)) {
        if let Ok(rec) = crate::record::Record::from_bytes(&bytes) {
            if rec.envelope.id == id {
                let token = crate::token::for_bytes(rec.envelope.version, &bytes);
                return Some(Scanned { rel: rel.to_string(), record: rec, token });
            }
        }
    }
    ops::get(store, id).ok()
}

/// 组装（§2.6 统一响应的数据面）：命中序分页 + 告警三要素模板。
fn assemble(
    mode: ReadMode,
    hits: Vec<Hit>,
    bad: Vec<(String, String)>,
    bad_more: bool,
    limit: usize,
    offset: usize,
    pq: Option<&ParsedQuery>,
    filters: &[FilterCond],
) -> FindOutcome {
    let (page, truncated, resume) = crate::budget::paginate(hits, offset, limit);
    let warnings = bad
        .into_iter()
        .map(|(rel, line)| format!("{line}；修复入口：mem fix {rel}"))
        .collect();
    FindOutcome {
        mode,
        hits: page,
        truncated,
        resume_offset: resume,
        positive_terms: pq.map(|q| q.positive.iter().map(|t| t.raw.clone()).collect()).unwrap_or_default(),
        negative_terms: pq.map(|q| q.negative.iter().map(|t| t.raw.clone()).collect()).unwrap_or_default(),
        filters_applied: filters.iter().map(|f| (f.field.clone(), f.text.clone())).collect(),
        warnings,
        warnings_truncated: bad_more,
    }
}
