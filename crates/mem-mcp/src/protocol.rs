//! MCP JSON-RPC 2.0 协议处理：initialize / tools/list / tools/call（stdio 前端，
//! mem-mcp 无自己的 IO 假设——mem bin 把 stdin 行喂进来、把应答行写回 stdout）。

use std::path::{Path, PathBuf};

use mem_core::error::{MemError, MemResult};
use mem_core::event::EventLog;
use mem_core::ops::{self, WriteContext};
use mem_core::session;
use mem_core::store::{OpenMode, Store};
use mem_core::token::Token;
use mem_git::GitProcess;
use mem_schema::TypeSet;
use serde_json::{json, Value};

const PROTOCOL_VERSION: &str = "2024-11-05";

/// 单请求处理：Some(应答 JSON-RPC object)；None = 通知（无应答）。
/// 检索/过滤参数的显式拒收（R10 §2.7）以 JSON-RPC error 应答，data.kind 携带
/// 机判 kind——与 CLI JSON 错误体同码。
pub fn handle_request(store_root: &Path, req: &Value) -> Option<Value> {
    let method = req.get("method")?.as_str()?;
    let id = req.get("id").cloned();
    let result: MemResult<Value> = match method {
        "initialize" => Ok(initialize(store_root, req.get("params"))),
        "notifications/initialized" | "initialized" => return None,
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": crate::tools::tool_definitions() })),
        "tools/call" => tool_call(store_root, req.get("params")),
        other => Err(MemError::BadArgument(format!("未知方法 {other}"))),
    };
    let id = id?;
    Some(match result {
        Ok(v) => json!({ "jsonrpc": "2.0", "id": id, "result": v }),
        Err(e) => {
            let mut error = json!({ "code": -32603, "message": e.to_string() });
            if let Some(kind) = e.param_kind() {
                error["data"] = json!({ "kind": kind });
            }
            json!({ "jsonrpc": "2.0", "id": id, "error": error })
        }
    })
}

fn initialize(store_root: &Path, _params: Option<&Value>) -> Value {
    // MCP initialize = 显式建立操作（D13.1）：此处只做协议握手；台账建立必须经
    // mem_session_start 显式提供 principal（红线25：不能从协议噪声推断身份）。
    let _ = store_root;
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "mem", "version": env!("CARGO_PKG_VERSION") },
        "instructions": "SpaceMemory 机制层。先 mem_session_start 显式建立台账（principal 必填，防错报不防伪装）；写工具携带 expect 全令牌（红线19）。",
    })
}

fn tool_call(store_root: &Path, params: Option<&Value>) -> MemResult<Value> {
    let p = params.ok_or_else(|| MemError::BadArgument("tools/call 缺 params".into()))?;
    let name = p
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| MemError::BadArgument("tools/call 缺 name".into()))?;
    let args = p.get("arguments").cloned().unwrap_or_else(|| json!({}));
    let out = dispatch_tool(store_root, name, &args)?;
    Ok(json!({
        "content": [ { "type": "text", "text": serde_json::to_string_pretty(&out).unwrap_or_default() } ],
        "isError": false,
    }))
}

macro_rules! arg_str {
    ($args:expr, $k:literal) => {
        $args.get($k).and_then(|v| v.as_str()).map(String::from)
    };
}

fn dispatch_tool(root: &Path, name: &str, args: &Value) -> Result<Value, MemError> {
    let store = Store::open(root, OpenMode::Auto)?;
    let types = TypeSet::load_dir(&store.abs("types")).map_err(|e| {
        MemError::Corrupt { path: "types".into(), reason: e.to_string() }
    })?;
    match name {
        "mem_brief" => {
            let checkpoint = mem_core::freshness::load_checkpoint(&store);
            let hello = mem_core::sidecar::probe(&store);
            let mode = mem_core::freshness::consult_with_hello(&store, checkpoint.as_ref(), hello.as_ref());
            let (ledgers, _) = session::scan_ledgers(&store)?;
            let principal = arg_str!(args, "principal");
            let identity = ledgers
                .iter()
                .filter(|l| principal.as_deref().map(|p| l.principal.contains(p)).unwrap_or(true))
                .max_by(|a, b| a.last_write.cmp(&b.last_write))
                .map(|l| json!({"session": l.id, "status": l.status, "watermark": l.watermark}));
            let view_ref = arg_str!(args, "view");
            // r12 B7-3：MCP 与 CLI 同预算——条数（brief_items）+ 字节（brief_bytes，
            // 下限校验单源）双层。offset 输入端接受（resume.offset 续读可执行）。
            let budget = store.config().budgets.brief_items as usize;
            let offset_arg = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let r = match view_ref {
                Some(v) => mem_core::view::execute_from(&store, &v, budget, offset_arg)?,
                None => mem_core::view::execute_filter(&store, &mem_core::view::ViewFilter::default(), budget, offset_arg)?,
            };
            let since = args.get("since").and_then(|v| v.as_u64());
            let since_info = since.map(|n| {
                let log = EventLog::new(&store).read_log().unwrap_or_default();
                json!({"changes_since": log.events.iter().filter(|e| e.seq > n).count()})
            });
            // r9 语义（与 CLI 同源实现）：条目完整 JSON 超字节预算 → 跳过 + 清单；
            // items 数组严格 ≤ 预算；resume.offset = 下一未处理项（跳过项经
            // skipped_over_budget 可见，单条读取用 mem_show 绕开分页预算）。
            let items: Vec<Value> = r.items.iter().map(|i| json!({
                "id": i.envelope.id, "type": i.envelope.r#type,
                "modified": i.envelope.modified, "token": i.token.to_string(),
            })).collect();
            let byte_budget = mem_core::budget::brief_bytes_budget(&store)?;
            let (kept, skipped, byte_truncated) =
                mem_core::budget::apply_byte_budget(items, byte_budget);
            let truncated = r.truncated || byte_truncated;
            let used = serde_json::to_string(&kept).map(|s| s.len()).unwrap_or(byte_budget);
            Ok(json!({
                "read_mode": mode,
                "identity": identity,
                "identity_note": if identity.is_none() { "无台账——mem_session_start 显式建立（红线25）" } else { "" },
                "items": kept,
                "truncated": truncated,
                "resume": if truncated {
                    json!({"offset": offset_arg + kept.len() + skipped.len()})
                } else { Value::Null },
                "bytes_budget": byte_budget,
                "bytes_used": used,
                "skipped_over_budget": skipped,
                "since": since_info,
            }))
        }
        "mem_session_start" => {
            store.check_writable()?;
            let principal = args.get("principal").and_then(|v| v.as_str()).ok_or_else(|| MemError::BadArgument("principal 必填".into()))?;
            let principal_id = resolve_principal(&store, &principal.to_string())?;
            let handoff = match arg_str!(args, "handoff").as_deref() {
                None => session::HandoffSpec::None,
                Some("auto") => session::HandoffSpec::Auto,
                Some(id) => session::HandoffSpec::Explicit(id.into()),
            };
            let o = session::start(&store, session::StartArgs {
                principal: principal_id,
                view: arg_str!(args, "view"),
                task_ref: arg_str!(args, "task_ref"),
                handoff,
                idle_secs: store.config().session_idle_secs,
            })?;
            Ok(json!({
                "session": o.ledger.id, "status": o.ledger.status,
                "watermark": o.ledger.watermark, "handoff_from": o.ledger.handoff_from,
                "idempotent": o.idempotent,
            }))
        }
        "mem_session_flush" => {
            store.check_writable()?;
            let sid = args.get("session").and_then(|v| v.as_str()).ok_or_else(|| MemError::BadArgument("session 必填".into()))?;
            let trail = args.get("trail").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
            let sink = GitProcess::new(store.root());
            let l = session::flush(&store, sid, trail, Some(&sink))?;
            Ok(json!({"session": l.id, "status": l.status, "watermark": l.watermark}))
        }
        "mem_list" => {
            // R10：filters 增参（决策⑤，值一律 JSON 字符串）+ 统一响应形状；
            // 快路径经 open_proved 构造即证明（决策②——MCP list 不再恒直读），
            // CLI 与 MCP 同源调用 mem-core find::execute。
            let filters = parse_mcp_filters(args.get("filters"))?;
            let limit = mem_core::budget::effective_limit(&store, args.get("limit").and_then(|v| v.as_u64()).map(|v| v as usize));
            let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let proved = proved_access(&store);
            let vf = match arg_str!(args, "view") {
                Some(v) => {
                    let vr = ops::get(&store, &v)?;
                    if vr.record.envelope.r#type != "view" {
                        return Err(MemError::BadArgument(format!("{v} 不是 view 记录")));
                    }
                    Some(mem_core::view::ViewFilter::from_payload(&vr.record.payload)?)
                }
                None => None,
            };
            let prefilter = if vf.is_some() {
                None
            } else {
                proved.index.as_ref().map(|p| p as &dyn mem_core::find::FindPrefilter)
            };
            let outcome = mem_core::find::execute(mem_core::find::FindParams {
                store: &store,
                types: &types,
                query: None,
                filters: &filters,
                type_narrow: arg_str!(args, "type").as_deref(),
                view: vf.as_ref(),
                limit,
                offset,
                mode: proved.mode,
                prefilter,
            })?;
            Ok(json!({
                "read_mode": outcome.mode,
                "items": outcome.hits.iter().map(|h| json!({"id": h.id, "type": h.type_name, "rel": h.rel, "modified": h.modified, "version": h.version, "token": h.token})).collect::<Vec<_>>(),
                "truncated": outcome.truncated,
                "resume": outcome.resume_offset.map(|o| json!({"offset": o})).unwrap_or(Value::Null),
                "filters_applied": filters_applied_json(&outcome.filters_applied),
                "warnings": outcome.warnings,
                "warnings_truncated": outcome.warnings_truncated,
                "note": outcome_note(outcome.truncated, outcome.resume_offset, outcome.warnings_truncated),
            }))
        }
        "mem_find" => {
            // R10 决策⑥：mem_find 新工具——query 必填且须含肯定词项（空查询/纯否定
            // → query_invalid；纯过滤由 mem_list 承载）；filters 值一律 JSON 字符串
            //（决策④：标量文本，类型解释由字段声明承担——非字符串标量显式拒收）。
            let query = arg_str!(args, "query").ok_or_else(|| {
                MemError::ParamRejected {
                    kind: mem_core::find::QUERY_INVALID.into(),
                    message: "query 必填且须含至少一个肯定词项（纯过滤清单请走 mem_list）".into(),
                }
            })?;
            let filters = parse_mcp_filters(args.get("filters"))?;
            let limit = mem_core::budget::effective_limit(&store, args.get("limit").and_then(|v| v.as_u64()).map(|v| v as usize));
            let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let proved = proved_access(&store);
            let outcome = mem_core::find::execute(mem_core::find::FindParams {
                store: &store,
                types: &types,
                query: Some(&query),
                filters: &filters,
                // find 不设 type/view 参数（检索的类型/域限定是基线未来项）。
                type_narrow: None,
                view: None,
                limit,
                offset,
                mode: proved.mode,
                prefilter: proved.index.as_ref().map(|p| p as &dyn mem_core::find::FindPrefilter),
            })?;
            Ok(json!({
                "read_mode": outcome.mode,
                "hits": outcome.hits.iter().map(|h| json!({"id": h.id, "type": h.type_name, "rel": h.rel, "token": h.token})).collect::<Vec<_>>(),
                "limit": limit,
                "truncated": outcome.truncated,
                "resume": outcome.resume_offset.map(|o| json!({"offset": o})).unwrap_or(Value::Null),
                "query": { "positive": outcome.positive_terms, "negative": outcome.negative_terms },
                "filters_applied": filters_applied_json(&outcome.filters_applied),
                "warnings": outcome.warnings,
                "warnings_truncated": outcome.warnings_truncated,
                "note": outcome_note(outcome.truncated, outcome.resume_offset, outcome.warnings_truncated),
            }))
        }
        "mem_show" => {
            let id = args.get("id").and_then(|v| v.as_str()).ok_or_else(|| MemError::BadArgument("id 必填".into()))?;
            // r12 B7-3：预算是配置错误——先于一切工作响亮拒收；MCP 与 CLI 同预算
            // 语义（R4.2/红线9 不因前端豁免）。
            // R11.2/BD2 响应自适应：正文区合法 UTF-8 → 文本分页（切点码点对齐，
            // 数学上字节保真）；不合法 → `body=null`、正文页以 `body_b64` 交付
            // （页 = brief_bytes 个**原始字节**，resume.body_offset 按字节计）。
            // `body_utf8` 恒有字段供机判（BD3）；分页单源 = mem_core::body。
            let budget = mem_core::budget::brief_bytes_budget(&store)?;
            let hit = ops::get(&store, id)?;
            let body_utf8 = mem_core::body::is_valid_utf8(&hit.record.body);
            let off = args.get("body_offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let (body, body_b64, body_truncated, next_off) = if body_utf8 {
                let (text, truncated, next) = mem_core::body::body_page_text(&hit.record.body, off, budget)
                    .expect("合法 UTF-8 正文必有文本通道");
                (json!(text), Value::Null, truncated, next)
            } else {
                let (bytes, truncated, next) = mem_core::body::body_page_bytes(&hit.record.body, off, budget);
                (Value::Null, json!(b64_encode(bytes)), truncated, next)
            };
            Ok(json!({
                "rel": hit.rel,
                "envelope": hit.record.envelope.to_json_value(),
                "payload": hit.record.payload,
                "body": body,
                "body_utf8": body_utf8,
                "body_b64": body_b64,
                "body_bytes_total": hit.record.body.len(),
                "body_truncated": body_truncated,
                "resume": if body_truncated {
                    json!({"body_offset": next_off})
                } else { Value::Null },
                "token": hit.token.to_string(),
            }))
        }
        "mem_trace" => {
            let id = args.get("id").and_then(|v| v.as_str()).ok_or_else(|| MemError::BadArgument("id 必填".into()))?;
            let dir = match arg_str!(args, "dir").as_deref() {
                Some("down") => mem_core::refgraph::TraceDirection::Down,
                _ => mem_core::refgraph::TraceDirection::Up,
            };
            // r12 B7-2：MCP 输入端**接受并执行** skip/limit（与 CLI --skip/--limit
            // 同语义：节点序号续读）——此前 resume 里的 skip/limit 在重放时被忽略，
            // 同一页反复返回。depth/limit 请求值同样受全局预算钳制（与 CLI 一致）。
            let (max_depth, max_nodes) = mem_core::budget::trace_budget(&store);
            let depth = args.get("depth").and_then(|v| v.as_u64()).map(|v| v as usize)
                .unwrap_or(max_depth).min(max_depth);
            let nodes = args.get("limit").and_then(|v| v.as_u64()).map(|v| v as usize)
                .unwrap_or(max_nodes).min(max_nodes);
            let skip_n = args.get("skip").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let outcome = mem_core::refgraph::trace_from(&store, id, dir, depth, nodes, skip_n)?;
            let nodes_out = &outcome.nodes;
            let truncated = outcome.truncated;
            let frontier = &outcome.frontier;
            // r10 B7-3：与 CLI 对齐——深度预算用尽时 resume 携带可执行 frontier。
            // r13 B9-1：resume 分支顺序与 CLI cmd_trace **完全同构**（三分支全齐）：
            // 深度截断且未达全局预算 → depth+1 重跑（此前该分支缺失，落入 null）。
            let depth_limited = truncated && nodes_out.len() < nodes;
            let resume_value = if truncated {
                if depth_limited {
                    if depth >= max_depth {
                        serde_json::json!({
                            "reason": "深度预算已用尽",
                            "budget_depth": max_depth,
                            "frontier": frontier.iter().map(|f| serde_json::json!({
                                "id": f,
                                "next": format!("mem trace {f} --dir {} --depth {}（继续深入）",
                                    if dir == mem_core::refgraph::TraceDirection::Down { "down" } else { "up" }, max_depth),
                            })).collect::<Vec<_>>(),
                        })
                    } else {
                        // r13 B9-1：可执行续读——按 resume.depth 重放（提高深度重跑，
                        // 与 CLI suggest 同语义）。
                        serde_json::json!({
                            "reason": "深度预算截断",
                            "depth": depth + 1,
                            "next": format!("mem_trace {{\"id\":\"{id}\",\"dir\":\"{}\",\"depth\":{}}}（提高深度重跑）",
                                if dir == mem_core::refgraph::TraceDirection::Down { "down" } else { "up" }, depth + 1),
                        })
                    }
                } else {
                    // r11/r12：节点预算截断的 resume 非空且**可执行**——skip 累加
                    // 本页已消费节点（含重放传入的 skip_n），limit = nodes+1 探测余量。
                    serde_json::json!({
                        "reason": "节点预算截断",
                        "skip": skip_n + nodes_out.len(),
                        "limit": nodes + 1,
                    })
                }
            } else {
                serde_json::Value::Null
            };
            Ok(json!({
                "nodes": nodes_out.iter().map(|n| json!({"id": n.id, "rel": n.rel, "depth": n.depth})).collect::<Vec<_>>(),
                "truncated": truncated,
                "resume": resume_value,
            }))
        }
        "mem_add" => {
            let session_id: Option<String> = arg_str!(args, "session");
            let ctx = make_ctx(&store, &types, session_id.as_deref());
            let out = ops::add(&ctx, ops::AddArgs {
                type_name: args.get("type").and_then(|v| v.as_str()).ok_or_else(|| MemError::BadArgument("type 必填".into()))?.to_string(),
                payload: args.get("payload").cloned().ok_or_else(|| MemError::BadArgument("payload 必填".into()))?,
                body: body_bytes(args)?.unwrap_or_default(),
                principal: arg_str!(args, "principal").map(|p| resolve_principal(&store, &p)).transpose()?,
            })?;
            Ok(json!({
                "created": out.created, "id": out.record.envelope.id, "rel": out.rel,
                "token": out.token.to_string(), "warnings": out.warnings,
            }))
        }
        "mem_update" => {
            let expect = parse_expect(args.get("expect"))?;
            // M4：payload 缺省会静默清空载荷（lenient 类型尤甚）——必须显式提供。
            let payload = args.get("payload").cloned().ok_or_else(|| {
                MemError::BadArgument("payload 必填（缺省 = 静默清空载荷，M4 拒绝）".into())
            })?;
            let session_id: Option<String> = arg_str!(args, "session");
            let ctx = make_ctx(&store, &types, session_id.as_deref());
            let out = ops::update(&ctx, args.get("id").and_then(|v| v.as_str()).unwrap_or_default(), &expect,
                payload, body_bytes(args)?)?;
            Ok(json!({
                "id": out.record.envelope.id, "version": out.record.envelope.version,
                "token": out.token.to_string(), "warnings": out.warnings,
            }))
        }
        "mem_link" => {
            let ctx = make_ctx(&store, &types, None);
            let (ta, tb) = ops::link(&ctx,
                args.get("a").and_then(|v| v.as_str()).unwrap_or_default(),
                args.get("b").and_then(|v| v.as_str()).unwrap_or_default(),
                args.get("rel").and_then(|v| v.as_str()).unwrap_or_default(),
                &parse_expect(args.get("expect_a"))?, &parse_expect(args.get("expect_b"))?)?;
            Ok(json!({"a_token": ta.to_string(), "b_token": tb.to_string()}))
        }
        "mem_delete" => {
            let ctx = make_ctx(&store, &types, None);
            let d = ops::delete_record(&ctx,
                args.get("id").and_then(|v| v.as_str()).unwrap_or_default(),
                &parse_expect(args.get("expect"))?,
                args.get("force").and_then(|v| v.as_bool()).unwrap_or(false))?;
            Ok(json!({"tombstone": d.rel, "warnings": d.warnings}))
        }
        "mem_restore" => {
            let ctx = make_ctx(&store, &types, None);
            let r = ops::restore_record(&ctx, args.get("id").and_then(|v| v.as_str()).unwrap_or_default(), None)?;
            Ok(json!({"rel": r.rel, "token": r.token.to_string()}))
        }
        "mem_restore_body" => {
            // R11.5 对等 + R11.6/BD4：恢复正文（经确认的内容写；与 CLI restore-body
            // 同批义务——前端功能对等）。来源三形：git:<版本> | file:<店外路径> | history。
            let id = args.get("id").and_then(|v| v.as_str()).ok_or_else(|| MemError::BadArgument("id 必填".into()))?;
            let expect = parse_expect(args.get("expect"))?;
            let source_spec = arg_str!(args, "source")
                .ok_or_else(|| MemError::BadArgument("source 必填（git:<版本> | file:<店外路径> | history）".into()))?;
            let source = parse_restore_source(&source_spec)?;
            let session_id: Option<String> = arg_str!(args, "session");
            let ctx = make_ctx(&store, &types, session_id.as_deref());
            let history = GitProcess::new(store.root());
            let out = ops::restore_body(&ctx, id, &expect, &source, Some(&history))?;
            Ok(json!({
                "id": out.record.envelope.id,
                "rel": out.rel,
                "version": out.record.envelope.version,
                "token": out.token.to_string(),
                "source": out.source_echo,
            }))
        }
        "mem_diff" => {
            let cfg = store.config().budgets;
            // r13 B9-2：组级 offset 续读（与 CLI cmd_diff 同语义——全量取组数再
            // 分页，精确截断判定；被截断的组经 resume.offset 可达，红线9 不因
            // 前端豁免）。
            let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let d = mem_core::drift::diff_since(&store, args.get("since").and_then(|v| v.as_u64()).unwrap_or(0),
                usize::MAX, cfg.current_drift_limit as usize)?;
            let total = d.changes_since.len();
            let page: Vec<_> = d.changes_since.into_iter()
                .skip(offset).take(cfg.diff_since_limit as usize).collect();
            let consumed = offset + page.len();
            let truncated = consumed < total;
            Ok(json!({
                "since": args.get("since"),
                "changes_since": page.iter().map(|g| json!({"id": g.id, "last_event": g.last_event, "history_len": g.history.len()})).collect::<Vec<_>>(),
                "changes_truncated": truncated,
                "resume": if truncated { json!({"offset": consumed}) } else { Value::Null },
                "total_groups": total,
                "current_drift": d.current_drift.items.iter().map(|i| json!({"id": i.id, "note": i.note, "token": i.token.as_ref().map(|t| t.to_string()), "readonly_note": i.readonly_note})).collect::<Vec<_>>(),
                "drift_truncated": d.current_drift.truncated,
            }))
        }
        "mem_reconcile" => {
            let report = mem_core::event::reconcile_backfill(&store, "mem", None)?;
            // r4 H2/B3：索引刷新走统一收敛原语；MCP 进程无订阅证据 → 检查点
            // **不发布**（此前无条件推进检查点会把旧索引"提前确认"，扩大误信窗口）。
            // sidecar 在场时由其 tick/握手路径收敛发布。
            let mut idx = mem_index::Index::open_store(&store)?;
            idx.rebuild(&store)?;
            idx.set_body_texts(&store)?;
            let _ = idx.epoch();
            Ok(json!({
                "backfill_appended": report.appended.len(),
                "skipped_stable": report.skipped_stable,
                "warnings": report.warnings,
                "index_rebuilt": true,
                "checkpoint_saved": false,
                "checkpoint_note": "检查点仅由 sidecar 收敛原语发布（r4 H2）",
            }))
        }
        "mem_verify" => {
            // r13 B9-3：与 CLI cmd_verify 同一组审计——镜像一致性（payload x-ref
            // vs envelope.refs 双向 + payload 悬空）、计划审计、deep 摘要对照。
            // verify 是对账工具，MCP 前端不得把已知分裂报成全绿（r5-B4 立法场景；
            // r5 登记的"MCP 接入"当时未实装，本轮补齐）。
            let deep = args.get("deep").and_then(|v| v.as_bool()).unwrap_or(false);
            let scan = ops::scan_all(&store)?;
            let log = EventLog::new(&store).read_log()?;
            let holes = mem_core::event::chain_audit(&log.events);
            let ids: std::collections::HashSet<_> = scan.records.iter().map(|r| r.record.envelope.id.clone()).collect();
            let mut dangling = Vec::new();
            for r in &scan.records {
                for e in &r.record.envelope.refs {
                    if !ids.contains(&e.target) {
                        dangling.push(json!({"from": r.record.envelope.id, "rel": e.rel, "target": e.target}));
                    }
                }
            }
            // H3：deep 对照事件日志摘要；r5 B4：镜像一致性 + 计划审计。
            let mut digest_mismatches = Vec::new();
            if deep {
                digest_mismatches = mem_core::ops::deep_digest_audit(&store)?;
            }
            let mirror_report = mem_core::ops::mirror_consistency_audit(&store, &types)?;
            let plan_report = mem_core::agenda::verify_report(&store, &types, deep)?;
            // BD5 ④：.gitattributes 提交树核验（与 CLI 同一组审计）。
            let has_git = store.root().join(".git").exists();
            let head_attr = if has_git {
                GitProcess::new(store.root())
                    .head_file(mem_core::gitattrs::GITATTRIBUTES_REL)
                    .unwrap_or(None)
            } else {
                None
            };
            let gitattrs_audit = mem_core::gitattrs::audit_findings(has_git, head_attr.as_deref());
            let ok = scan.warnings.is_empty()
                && holes.is_empty()
                && dangling.is_empty()
                && digest_mismatches.is_empty()
                && mirror_report.is_empty()
                && plan_report.is_empty()
                && gitattrs_audit.is_empty();
            Ok(json!({
                "ok": ok, "scanned": scan.records.len(), "warnings": scan.warnings,
                "history_holes": holes, "dangling_refs": dangling,
                "payload_mirror_inconsistencies": mirror_report,
                "digest_mismatches": digest_mismatches,
                "gitattributes_audit": gitattrs_audit,
                "plans": plan_report,
            }))
        }
        other => Err(MemError::BadArgument(format!("未知工具 {other}"))),
    }
}

/// 读路径的证明式接入（决策②——调用点不承载新鲜性判断）：与 CLI 同一构造函数
/// `open_proved`（stat 咨询 ∧ 只读打开 ∧ schema 门禁含 bad_records ∧ epoch 同代）。
fn proved_access(store: &Store) -> mem_index::Proved {
    let hello = mem_core::sidecar::probe(store);
    mem_index::open_proved(store, hello.as_ref())
}

/// MCP filters 参数解析（决策④/⑤）：JSON 对象 {"字段": "值"}，值**一律 JSON
/// 字符串**（标量文本，类型解释由字段声明承担）——非字符串 JSON 标量显式拒收
/// invalid_filter_value，两前端合法条件空间逐字相同。
fn parse_mcp_filters(v: Option<&Value>) -> MemResult<Vec<mem_core::find::FilterCond>> {
    let Some(Value::Object(map)) = v else {
        if v.is_some() {
            return Err(MemError::ParamRejected {
                kind: mem_core::find::INVALID_FILTER_VALUE.into(),
                message: "filters 必须是对象 {\"字段\": \"文本\"}".into(),
            });
        }
        return Ok(vec![]);
    };
    map.iter()
        .map(|(k, val)| {
            let Some(text) = val.as_str() else {
                return Err(MemError::ParamRejected {
                    kind: mem_core::find::INVALID_FILTER_VALUE.into(),
                    message: format!(
                        "filters 值须为字符串文本，类型解释由字段声明承担（决策④）：{k} = {val}"
                    ),
                });
            };
            Ok(mem_core::find::FilterCond { field: k.clone(), text: text.to_string() })
        })
        .collect()
}

/// filters_applied 回显（字段→文本对象）。
fn filters_applied_json(filters: &[(String, String)]) -> serde_json::Map<String, Value> {
    let mut m = serde_json::Map::new();
    for (k, v) in filters {
        m.insert(k.clone(), Value::String(v.clone()));
    }
    m
}

/// 统一响应 note（§2.6）：截断给续读指引；告警截断注明 verify 续读通道。
fn outcome_note(truncated: bool, resume: Option<usize>, warnings_truncated: bool) -> String {
    let mut parts = Vec::new();
    if truncated {
        if let Some(off) = resume {
            parts.push(format!("用 offset {off} 续读"));
        }
    }
    if warnings_truncated {
        parts.push("完整坏记录清单经 mem_verify".into());
    }
    parts.join("；")
}

fn make_ctx<'a>(
    store: &'a Store,
    types: &'a TypeSet,
    session_id: Option<&'a str>,
) -> WriteContext<'a> {
    let principal_label = session_id
        .and_then(|sid| session::get(store, sid).ok().flatten())
        .map(|l| l.principal)
        .unwrap_or_default();
    WriteContext {
        store,
        types,
        by: format!("{principal_label}(mcp)"),
        committer: if store.root().join(".git").exists() { Some(&CommitViaCurrent) } else { None },
        session_id,
    }
}

struct CommitViaCurrent;
static CURRENT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

impl ops::CommitSink for CommitViaCurrent {
    fn commit(&self, paths: &[String]) -> mem_core::error::MemResult<Option<String>> {
        let root = CURRENT.get().cloned().unwrap_or_default();
        GitProcess::new(root).commit(paths)
    }
}

pub fn remember_root(root: &Path) {
    let _ = CURRENT.set(root.to_path_buf());
}

fn resolve_principal(store: &Store, id_or_slug: &str) -> MemResult<String> {
    let hit = ops::get(store, id_or_slug)?;
    if hit.record.envelope.r#type != "principal" {
        return Err(MemError::BadArgument(format!("{id_or_slug} 不是 principal 记录")));
    }
    Ok(hit.record.envelope.id)
}

/// M4：正文传输——`body`（UTF-8 字符串）或 `body_b64`（base64 字节保真；
/// 非 UTF-8 正文经 JSON 字符串往返会破坏字节，红线27 的 MCP 通道缺口）。
fn body_bytes(args: &Value) -> MemResult<Option<Vec<u8>>> {
    if let Some(b64) = args.get("body_b64").and_then(|v| v.as_str()) {
        return Ok(Some(b64_decode(b64)?));
    }
    Ok(args.get("body").and_then(|v| v.as_str()).map(|b| b.as_bytes().to_vec()))
}

/// base64 编码（标准字母表 + padding；与写通道 `b64_decode` 同构的保真传输
/// 编码——R11.2/BD2 读通道：非文本正文页经 JSON 交付时的传输编码）。
fn b64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).map(|b| *b as u32);
        let b2 = chunk.get(2).map(|b| *b as u32);
        let n = (b0 << 16) | (b1.unwrap_or(0) << 8) | b2.unwrap_or(0);
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if b1.is_some() { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if b2.is_some() { T[n as usize & 63] as char } else { '=' });
    }
    out
}

fn b64_decode(s: &str) -> MemResult<Vec<u8>> {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::new();
    let mut buf = 0u32;
    let mut bits = 0u32;
    for c in s.bytes() {
        if c == b'=' || c == b'\n' || c == b'\r' {
            continue;
        }
        let v = T
            .iter()
            .position(|&t| t == c)
            .ok_or_else(|| MemError::BadArgument(format!("body_b64 非法字符：{}", c as char)))?
            as u32;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Ok(out)
}

fn parse_expect(v: Option<&Value>) -> MemResult<Token> {
    let s = v
        .and_then(|v| v.as_str())
        .or_else(|| v.and_then(|v| v.get("version")).and_then(|x| x.as_str()))
        .ok_or_else(|| MemError::BadArgument("expect 令牌缺失（红线19：读改写必须携带全令牌）".into()))?;
    s.parse::<Token>().map_err(MemError::BadArgument)
}

/// 来源三形解析（与 CLI parse_restore_source 同构；§3.6）。
fn parse_restore_source(spec: &str) -> MemResult<ops::RestoreSource> {
    if spec == "history" {
        return Ok(ops::RestoreSource::History);
    }
    if let Some(rev) = spec.strip_prefix("git:") {
        if rev.is_empty() {
            return Err(MemError::BadArgument("git: 源缺版本号（git:<版本>）".into()));
        }
        return Ok(ops::RestoreSource::Git { rev: rev.to_string() });
    }
    if let Some(p) = spec.strip_prefix("file:") {
        if p.is_empty() {
            return Err(MemError::BadArgument("file: 源缺路径（file:<店外路径>）".into()));
        }
        return Ok(ops::RestoreSource::File { path: std::path::PathBuf::from(p) });
    }
    Err(MemError::BadArgument(format!(
        "source 必须为 git:<版本> | file:<店外路径> | history，实际 {spec:?}"
    )))
}
