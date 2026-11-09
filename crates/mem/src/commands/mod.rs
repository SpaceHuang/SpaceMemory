//! CLI 子命令实现（§3.3）。
//!
//! 纪律：读命令零写盘（红线18）；brief 无台账如实报告、直读标注（D5/红线25）；
//! 写命令走统一协议（D3）；写路径顺带登记台账 trail、兜底建台账（D13）。

use std::io::Read;

use mem_core::error::{MemError, MemResult};
use mem_core::event::EventLog;
use mem_core::ops::{self, WriteContext};
use mem_core::session;
use mem_core::store::{OpenMode, Store};
use mem_core::token::Token;
use mem_core::ops::CommitSink;
use mem_git::GitProcess;
use mem_schema::TypeSet;

use crate::cli::{Cli, Cmd, SessionCmd};

pub fn dispatch(cli: &Cli) -> Result<i32, String> {
    match run(cli) {
        Ok(code) => Ok(code),
        Err(e @ MemError::Conflict { .. }) => {
            eprintln!("{}", serde_json::json!({ "error": e.to_string(), "conflict": true }));
            Ok(2) // 冲突是独立退出码（一等结果，D3）
        }
        Err(e) => Err(e.to_string()),
    }
}

fn run(cli: &Cli) -> MemResult<i32> {
    match &cli.cmd {
        Cmd::Init { path, template_dir, principal } => {
            cmd_init(path, template_dir.as_deref(), principal)?;
            Ok(0)
        }
        cmd => {
            let store = open_store(cli)?;
            let types = load_types(&store)?;
            match cmd {
                Cmd::Init { .. } => unreachable!(),
                Cmd::Brief { view, principal, since, offset } => {
                    cmd_brief(&store, view.as_deref(), principal.as_deref(), *since, *offset)?;
                    Ok(0)
                }
                Cmd::Session { sub } => match sub {
                    SessionCmd::Start { view, principal, task_ref, handoff } => {
                        cmd_session_start(
                            &store,
                            &types,
                            view.as_deref(),
                            principal,
                            task_ref.as_deref(),
                            handoff.as_deref(),
                        )?;
                        Ok(0)
                    }
                    SessionCmd::Flush { session, trail } => {
                        cmd_session_flush(&store, session, trail.clone())?;
                        Ok(0)
                    }
                },
                Cmd::Reconcile { deep } => {
                    cmd_reconcile(&store, *deep)?;
                    Ok(0)
                }
                Cmd::List { view, type_filter, limit, offset } => {
                    cmd_list(&store, view.as_deref(), type_filter.as_deref(), *limit, *offset)?;
                    Ok(0)
                }
                Cmd::Find { query, limit, offset } => {
                    cmd_find(&store, query, *limit, *offset)?;
                    Ok(0)
                }
                Cmd::Show { id, body, body_offset, body_out } => {
                    cmd_show(&store, id, *body, *body_offset, body_out.as_deref())?;
                    Ok(0)
                }
                Cmd::Trace { id, dir, depth, limit, skip } => {
                    cmd_trace(&store, id, dir, *depth, *limit, *skip)?;
                    Ok(0)
                }
                Cmd::Add { type_name, payload, body, principal, session: sess } => {
                    cmd_add(
                        &store,
                        &types,
                        type_name,
                        payload,
                        body.as_deref(),
                        principal.as_deref(),
                        sess.as_deref(),
                    )?;
                    Ok(0)
                }
                Cmd::Update { id, expect, payload, body_file, session: sess } => {
                    cmd_update(
                        &store,
                        &types,
                        id,
                        expect,
                        payload,
                        body_file.as_deref(),
                        sess.as_deref(),
                    )?;
                    Ok(0)
                }
                Cmd::Link { a, b, rel, expect_a, expect_b } => {
                    cmd_link(&store, &types, a, b, rel, expect_a, expect_b)?;
                    Ok(0)
                }
                Cmd::Merge { dup, canonical, expect_dup, expect_canonical } => {
                    cmd_merge(&store, &types, dup, canonical, expect_dup, expect_canonical)?;
                    Ok(0)
                }
                Cmd::Delete { id, expect, force } => {
                    cmd_delete(&store, &types, id, expect, *force)?;
                    Ok(0)
                }
                Cmd::Restore { id } => {
                    cmd_restore(&store, &types, id)?;
                    Ok(0)
                }
                Cmd::Diff { since, offset } => {
                    cmd_diff(&store, *since, *offset)?;
                    Ok(0)
                }
                Cmd::View { id, limit, offset } => {
                    let (mode, index) = fastpath(&store);
                    // r4 B1：仅"**恰好一个类型** 且 无 refs/principals/since"才可走
                    // 索引（query_by_type 只懂类型单维——此前条件写反，复合视图绕过
                    // 过滤返回视图外记录、空类型视图 index 越界 panic）。
                    let v = ops::get(&store, id)?;
                    let filter = mem_core::view::ViewFilter::from_payload(&v.record.payload)?;
                    let can_index = filter.refs.is_empty()
                        && filter.principals.is_empty()
                        && filter.since.is_none()
                        && filter.types.len() == 1;
                    let limit_n = mem_core::budget::effective_limit(&store, *limit);
                    let fast = match (&index, can_index) {
                        (Some(idx), true) => {
                            idx.query_by_type(&filter.types[0], limit_n, offset.unwrap_or(0)).ok()
                        }
                        _ => None,
                    };
                    match fast {
                        Some(rows) => {
                            // r5 B1：快路径与直读逐 ID 等价性**防御比对**——不一致
                            // 时不报错（索引永不 gate 正确性，红线8；可丢弃缓存不
                            // 得成为读命令单点故障），放弃快路径、**重新执行直读并
                            // 返回直读结果**（read_mode 如实标 direct + reason）。
                            let direct = mem_core::view::execute_from(&store, id, limit_n, offset.unwrap_or(0))?;
                            let fast_ids: Vec<_> = rows.iter().map(|r| r.id.clone()).collect();
                            let direct_ids: Vec<_> = direct.items.iter().map(|i| i.envelope.id.clone()).collect();
                            if fast_ids == direct_ids {
                                out_json(&serde_json::json!({
                                    "read_mode": mode,
                                    "items": rows.iter().map(index_row_to_json).collect::<Vec<_>>(),
                                    "truncated": rows.len() == limit_n,
                                    "resume": if rows.len() == limit_n { serde_json::json!({"offset": offset.unwrap_or(0) + rows.len()}) } else { serde_json::Value::Null },
                                }));
                            } else {
                                out_json(&serde_json::json!({
                                    "read_mode": mem_core::freshness::ReadMode::Direct {
                                        reason: "索引快路径与直读不一致——放弃快路径，返回直读结果（红线8）".into(),
                                    },
                                    "items": direct.items.iter().map(|i| serde_json::json!({
                                        "id": i.envelope.id, "type": i.envelope.r#type, "rel": i.rel,
                                        "modified": i.envelope.modified, "token": i.token.to_string(),
                                    })).collect::<Vec<_>>(),
                                    "truncated": direct.truncated,
                                    "resume": direct.resume_hint.map(|h| serde_json::json!({"offset": h.offset})),
                                }));
                            }
                        }
                        None => {
                            let r = mem_core::view::execute_from(&store, id, limit_n, offset.unwrap_or(0))?;
                            out_json(&serde_json::json!({
                                "read_mode": mode,
                                "items": r.items.iter().map(|i| serde_json::json!({
                                    "id": i.envelope.id, "type": i.envelope.r#type, "rel": i.rel,
                                    "modified": i.envelope.modified, "token": i.token.to_string(),
                                })).collect::<Vec<_>>(),
                                "truncated": r.truncated,
                                "resume": r.resume_hint.map(|h| serde_json::json!({"offset": h.offset})),
                            }));
                        }
                    }
                    Ok(0)
                }
                Cmd::Bind { view } => {
                    cmd_bind(&store, view)?;
                    Ok(0)
                }
                Cmd::Verify { deep } => {
                    cmd_verify(&store, &types, *deep)?;
                    Ok(0)
                }
                Cmd::Agenda { scan } => {
                    cmd_agenda(&store, &types, *scan)?;
                    Ok(0)
                }
                Cmd::Apply { plan_id } => {
                    cmd_apply(&store, &types, plan_id)?;
                    Ok(0)
                }
                Cmd::Fix { id, envelope, payload, expect } => {
                    cmd_fix(&store, &types, id, envelope.as_deref(), payload.as_deref(), expect.as_deref())?;
                    Ok(0)
                }
                Cmd::RestoreBody { id, expect, source } => {
                    cmd_restore_body(&store, &types, id, expect, source)?;
                    Ok(0)
                }
            }
        }
    }
}

// ---------- 公共构件 ----------

/// 店根解析单源（CLI 与 `mem mcp` 共用，r14 MCP 安装）：显式路径 > $MEM_STORE
/// > ~/.agent-memory（用户级数据目录）。MCP 前端不再强依赖 MEM_STORE 环境变量
/// ——此前缺省即退出（部署脚手架）；红线25 不变：台账仍由宿主显式建立。
pub(crate) fn store_root_or_default(explicit: Option<&std::path::Path>) -> std::path::PathBuf {
    if let Some(p) = explicit {
        return p.to_path_buf();
    }
    if let Ok(p) = std::env::var("MEM_STORE") {
        return p.into();
    }
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| ".".into());
    std::path::Path::new(&home).join(".agent-memory")
}

fn store_root(cli: &Cli) -> std::path::PathBuf {
    store_root_or_default(cli.store.as_deref())
}

#[cfg(test)]
mod store_root_tests {
    use super::*;

    #[test]
    fn default_chain_env_then_user_home() {
        // 单测试串行覆盖两级（env 操作跨测试并行会竞态）：
        // 1) MEM_STORE 显式 → 原样采用；
        // 2) 无 MEM_STORE、无 CLI 参数 → 用户级 ~/.agent-memory。
        unsafe { std::env::set_var("MEM_STORE", "X:/mem-env-store") };
        assert_eq!(
            store_root_or_default(None),
            std::path::PathBuf::from("X:/mem-env-store"),
            "$MEM_STORE 显式优先"
        );
        unsafe { std::env::remove_var("MEM_STORE") };
        let r = store_root_or_default(None);
        assert!(r.ends_with(".agent-memory"), "缺省 = 用户级数据目录，实际 {r:?}");
    }
}

fn open_store(cli: &Cli) -> MemResult<Store> {
    let root = store_root(cli);
    let _ = CURRENT_STORE.set(root.clone());
    Store::open(&root, OpenMode::Auto)
}

fn load_types(store: &Store) -> MemResult<TypeSet> {
    let dir = store.abs("types");
    TypeSet::load_dir(&dir).map_err(|e| MemError::Corrupt {
        path: dir.display().to_string(),
        reason: e.to_string(),
    })
}

fn sink_for(store: &Store) -> GitProcess {
    GitProcess::new(store.root())
}

/// B3 接线：读路径的新鲜性判定 + 快路径索引（Indexed 时 Some）。
///
/// 证明判定（红线21）：sidecar 握手（Hello.undigested == 0）∧ stat 检查点一致
/// → 快路径查 index.sqlite；任一不成立 → 直读真相层（标注 reason）。索引打开
/// 失败 = 可丢弃缓存损坏 → 直读兜底（索引永不 gate 正确性，红线8）。
fn fastpath(store: &Store) -> (mem_core::freshness::ReadMode, Option<mem_index::Index>) {
    let checkpoint = mem_core::freshness::load_checkpoint(store);
    let hello = mem_core::sidecar::probe(store);
    let mode = mem_core::freshness::consult_with_hello(store, checkpoint.as_ref(), hello.as_ref());
    if mode.is_direct() {
        return (mode, None);
    }
    // r4 B2：读路径**只读**打开已存在索引——不建库/不建表（此前 Connection::open
    // 会自建空库并把空结果当真相应答）；缺失/schema 不符 → 直读（红线8/18）。
    let index = match mem_index::Index::open_store_readonly(store) {
        Ok(i) => i,
        Err(_) => {
            return (
                mem_core::freshness::ReadMode::Direct {
                    reason: "索引缺失或不完整（读路径不建库——直读兜底，红线8/18）".into(),
                },
                None,
            )
        }
    };
    // r4 B2 代际核对：检查点必须与索引同物化代——索引被删重建（epoch 重置）
    // 而检查点记旧代 → 直读，空/旧索引不被误信。
    let epoch_ok = checkpoint
        .map(|cp| index.epoch().map(|e| e == cp.index_epoch).unwrap_or(false))
        .unwrap_or(false);
    if !epoch_ok {
        return (
            mem_core::freshness::ReadMode::Direct {
                reason: "检查点与索引代际不匹配（物化进行中或索引被重建）——直读".into(),
            },
            None,
        );
    }
    (mode, Some(index))
}

fn index_row_to_json(r: &mem_index::IndexRow) -> serde_json::Value {
    serde_json::json!({
        "id": r.id, "type": r.type_name, "rel": r.rel,
        "modified": r.modified, "version": r.version,
        "token": format!("v{}@{}", r.version, r.digest),
    })
}

fn resolve_principal(store: &Store, id_or_slug: &str) -> MemResult<String> {
    let hit = ops::get(store, id_or_slug)?;
    if hit.record.envelope.r#type != "principal" {
        return Err(MemError::BadArgument(format!("{id_or_slug} 不是 principal 记录")));
    }
    Ok(hit.record.envelope.id)
}

fn out_json(v: &serde_json::Value) {
    println!("{}", serde_json::to_string_pretty(v).expect("JSON 输出不会失败"));
}

fn read_body(spec: Option<&str>) -> MemResult<Vec<u8>> {
    match spec {
        None => Ok(Vec::new()),
        Some("-") => {
            let mut buf = Vec::new();
            std::io::stdin()
                .read_to_end(&mut buf)
                .map_err(|e| MemError::BadArgument(format!("stdin 读取失败：{e}")))?;
            Ok(buf)
        }
        Some(p) => std::fs::read(p)
            .map_err(|e| MemError::BadArgument(format!("正文文件 {p} 不可读：{e}"))),
    }
}

// ---------- init ----------

fn cmd_init(
    path: &std::path::Path,
    template_dir: Option<&str>,
    principal_label: &str,
) -> MemResult<()> {
    Store::create(path)?;
    // BD5 ①：行尾规则随 init 播种（幂等；git add -A 暂存后随首次受管提交入树）。
    mem_core::gitattrs::seed_at_root(path)?;
    // 播种类型两件套（混合分发：外部优先、内嵌兜底，开放问题40）。
    // r14 发布准备：查找逻辑单源下沉 mem_assets::resolve_seed_files——
    // 模板覆盖 > exe 旁 builtin-types/ > Homebrew share 布局 > 内嵌字节
    //（packaging.md §2 清单第 4/3 项；share 布局覆盖 Homebrew 安装根）。
    let exe_dir =
        std::env::current_exe().ok().and_then(|exe| exe.parent().map(|d| d.to_path_buf()));
    let seed = mem_assets::resolve_seed_files(
        template_dir.map(std::path::Path::new),
        exe_dir.as_deref(),
    );
    let (asset_source, template_dir_used): (&str, Option<String>) = match &seed.origin {
        mem_assets::SeedOrigin::External(d) => ("external", Some(d.display().to_string())),
        mem_assets::SeedOrigin::Embedded => ("embedded", None),
    };
    let files = seed.files;
    for (rel, bytes) in &files {
        let abs = path.join(rel);
        std::fs::create_dir_all(abs.parent().unwrap())
            .map_err(|e| MemError::io(abs.display().to_string(), e))?;
        std::fs::write(&abs, bytes).map_err(|e| MemError::io(abs.display().to_string(), e))?;
    }
    // M2：git 基座随店建立（best effort；git 缺席 = 无历史形态，合法降级红线10，
    // 但 fix 的历史通道与 envelope.commit 默认可用）。
    let git_ok = std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["init", "-q"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if git_ok {
        let _ = std::process::Command::new("git")
            .arg("-C")
            .arg(path)
            .args(["-c", "user.name=mem", "-c", "user.email=mem@local", "add", "-A"])
            .output();
    }
    // 自举首条 principal（经类型声明引入，红线12/13）。
    let store = Store::open(path, OpenMode::Auto)?;
    let types = load_types(&store)?;
    let ctx = WriteContext {
        store: &store,
        types: &types,
        by: "mem init".into(),
        committer: None,
        session_id: None,
    };
    // 自举载荷来自播种数据（红线1：类型字段含义在数据，不在代码）；
    // 机制只覆写唯一键推导必须知道的两个名字字段（slug/label）。
    let mut payload: serde_json::Value = serde_json::from_slice(
        &std::fs::read(path.join("bootstrap/principal.payload.json")).map_err(|e| {
            MemError::Corrupt {
                path: "bootstrap/principal.payload.json".into(),
                reason: format!("播种资产缺失 principal 模板：{e}"),
            }
        })?,
    )
    .map_err(|e| MemError::Corrupt {
        path: "bootstrap/principal.payload.json".into(),
        reason: e.to_string(),
    })?;
    payload["slug"] = serde_json::Value::String(principal_label.to_string());
    payload["label"] = serde_json::Value::String(principal_label.to_string());
    if payload.get("host").map(|v| v.is_null()).unwrap_or(true) {
        payload["host"] = serde_json::json!(std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).ok());
    }
    let _ = ops::add(
        &ctx,
        ops::AddArgs { type_name: "principal".into(), payload, body: Vec::new(), principal: None },
    )?;
    out_json(&serde_json::json!({
        "ok": true,
        "store": path.display().to_string(),
        "seeded_files": files.len(),
        "principal": principal_label,
        "asset_source": asset_source,
        "template_dir": template_dir_used,
        "git_initialized": git_ok,
    }));
    Ok(())
}

// ---------- brief ----------

fn cmd_brief(
    store: &Store,
    view: Option<&str>,
    principal: Option<&str>,
    since: Option<u64>,
    offset: Option<usize>,
) -> MemResult<()> {
    // 证明式新鲜性判定（读路径零写盘；B3：订阅状态经 sidecar 握手远端取得）。
    let (mode, index) = fastpath(store);
    // 台账：只读查找（红线25：brief 永不创建/修改台账）。
    let (ledgers, _) = session::scan_ledgers(store)?;
    let identity = ledgers
        .iter()
        .filter(|l| principal.map(|p| l.principal == p || l.principal.contains(p)).unwrap_or(true))
        .max_by(|a, b| a.last_write.cmp(&b.last_write))
        .map(|l| {
            serde_json::json!({
                "session": l.id, "status": l.status, "watermark": l.watermark,
                "handoff_from": l.handoff_from, "view": l.view, "task_ref": l.task_ref,
            })
        });
    // 视图：显式参数 > mem.json 缺省视图指针（显式声明，红线11） > 全库最近。
    let cfg = store.config();
    let view_ref = view
        .map(String::from)
        .or_else(|| cfg.extra.get("default_view").and_then(|v| v.as_str()).map(String::from));
    let (mut items, mut truncated) = match view_ref {
        Some(v) => {
            let r = mem_core::view::execute_from(store, &v, cfg.budgets.brief_items as usize, offset.unwrap_or(0))?;
            let t = r.truncated;
            (brief_items(r), t)
        }
        None => {
            // 快路径：无视图参数 = 全库最近 N 条 → 索引 query_all；不可用则直读。
            let budget = cfg.budgets.brief_items as usize;
            let fast = index.as_ref().and_then(|idx| idx.query_all(budget, offset.unwrap_or(0)).ok());
            match fast {
                Some(rows) => (
                    rows.iter()
                        .map(|r| {
                            serde_json::json!({
                                "id": r.id, "type": r.type_name,
                                "modified": r.modified,
                                "token": format!("v{}@{}", r.version, r.digest),
                            })
                        })
                        .collect::<Vec<_>>(),
                    rows.len() == budget,
                ),
                None => {
                    let filter = mem_core::view::ViewFilter::default();
                    let r = mem_core::view::execute_filter(store, &filter, budget, offset.unwrap_or(0))?;
                    let t = r.truncated;
                    (brief_items(r), t)
                }
            }
        }
    };
    // r4 B7/r9：brief_bytes 硬预算（R4.2）——条目完整 JSON 超预算 → **跳过**
    //（不降级——任何降级形式仍可能超预算），skipped 清单可识别（R6.1）；
    // items 数组严格 ≤ 预算；resume.offset 指向下一未处理项（可执行续读——
    // 跳过的条目用 mem show <id> 单条读取，天然绕开分页预算）。
    // r12 B7-3：预算下限校验与条目字节预算下沉 mem_core::budget 单源（CLI/MCP
    // 共用——此前 MCP 前端缺字节预算，R4.2/红线9 不因前端豁免）。
    let byte_budget = mem_core::budget::brief_bytes_budget(store)?;
    let (kept, skipped, byte_truncated) =
        mem_core::budget::apply_byte_budget(items.drain(..).collect(), byte_budget);
    truncated = truncated || byte_truncated;
    items = kept;
    // r8：bytes_used = 最终 items 数组的实际序列化长度（与输出逐字节一致——
    // 此前逐项估算 +1 逗号导致 47 字节目的 items 被记为 48/2）。
    let used = serde_json::to_string(&items)
        .map(|s| s.len())
        .unwrap_or(byte_budget);
    let since_info = since.map(|n| {
        let log = EventLog::new(store).read_log().unwrap_or_default();
        serde_json::json!({
            "changes_since": log.events.iter().filter(|e| e.seq > n).count(),
            "max_seq": log.events.last().map(|e| e.seq),
        })
    });
    out_json(&serde_json::json!({
        "read_mode": mode,
        "identity": identity,
        "identity_note": if identity.is_none() {
            "无台账——mem session start 显式建立（brief 纯读，红线25）"
        } else { "" },
        "conventions": store.abs("conventions/CONVENTIONS.md").display().to_string(),
        "conventions_version": conventions_version(store),
        "items": items,
        "items_truncated": truncated,
        // r9 B7：resume = 跳过条目后的下一未处理项（跳过项不阻塞分页；
        // 被跳过的条目经 skipped_over_budget 清单可见，单条读取用 mem show）。
        "resume": if truncated { serde_json::json!({"offset": offset.unwrap_or(0) + items.len() + skipped.len()}) } else { serde_json::Value::Null },
        "bytes_budget": byte_budget,
        "bytes_used": used,
        "skipped_over_budget": skipped,
        "since": since_info,
        "next": "mem session start / mem diff --since <watermark> / mem show <id>",
    }));
    Ok(())
}

fn brief_items(r: mem_core::view::ViewResult) -> Vec<serde_json::Value> {
    r.items
        .into_iter()
        .map(|i| {
            serde_json::json!({
                "id": i.envelope.id, "type": i.envelope.r#type,
                "modified": i.envelope.modified, "token": i.token.to_string(),
            })
        })
        .collect()
}

fn conventions_version(store: &Store) -> u64 {
    std::fs::read_to_string(store.abs("conventions/CONVENTIONS.md"))
        .ok()
        .and_then(|t| {
            t.lines()
                .find_map(|l| l.trim().strip_prefix("- version:"))
                .and_then(|v| v.trim().parse().ok())
        })
        .unwrap_or(1)
}

// ---------- session ----------

fn cmd_session_start(
    store: &Store,
    _types: &TypeSet,
    view: Option<&str>,
    principal: &str,
    task_ref: Option<&str>,
    handoff: Option<&str>,
) -> MemResult<()> {
    let principal_id = resolve_principal(store, principal)?;
    let handoff_spec = match handoff {
        None => session::HandoffSpec::None,
        Some("auto") => session::HandoffSpec::Auto,
        Some(id) => session::HandoffSpec::Explicit(id.to_string()),
    };
    let outcome = session::start(
        store,
        session::StartArgs {
            principal: principal_id,
            view: view.map(String::from),
            task_ref: task_ref.map(String::from),
            handoff: handoff_spec,
            idle_secs: store.config().session_idle_secs,
        },
    )?;
    let l = &outcome.ledger;
    out_json(&serde_json::json!({
        "session": l.id,
        "principal": l.principal,
        "view": l.view,
        "task_ref": l.task_ref,
        "status": l.status,
        "watermark": l.watermark,
        "handoff_from": l.handoff_from,
        "idempotent": outcome.idempotent,
        "peers": outcome.peers.iter().map(|p| serde_json::json!({"session": p.id, "last_write": p.last_write})).collect::<Vec<_>>(),
        "conventions": store.abs("conventions/CONVENTIONS.md").display().to_string(),
        "next": "mem brief / mem add / mem update --expect <token>",
    }));
    Ok(())
}

fn cmd_session_flush(store: &Store, session_id: &str, trail: Vec<String>) -> MemResult<()> {
    let sink = sink_for(store);
    let ledger = session::flush(store, session_id, trail, Some(&sink))?;
    out_json(&serde_json::json!({
        "session": ledger.id,
        "status": ledger.status,
        "watermark": ledger.watermark,
        "read_trail": ledger.read_trail,
    }));
    Ok(())
}

// ---------- reconcile ----------

fn cmd_reconcile(store: &Store, deep: bool) -> MemResult<()> {
    let report = mem_core::event::reconcile_backfill(store, "mem", None)?;
    // commit-pending 重试（可重试资源；不分配序号）。
    let mut retried = 0usize;
    let pending = ops::list_pending_commits(store)?;
    if !pending.is_empty() {
        let sink = sink_for(store);
        for (path, pc) in &pending {
            if sink.commit(&pc.paths).is_ok() {
                let _ = std::fs::remove_file(path);
                retried += 1;
            }
        }
    }
    // r4 H2/B3：索引刷新（rebuild + FTS）但**不发布检查点**——reconcile 进程
    // 无订阅证据可查，无收敛证据不得签发证明（旧实现无条件推进检查点，会把旧
    // 索引"提前确认"）。检查点仅由 sidecar 的统一收敛原语发布。
    let mut index_rebuilt = false;
    if store.writable() {
        if let Ok(mut idx) = mem_index::Index::open_store(store) {
            // r11 优化：FTS 虚表移除——find 子串检索由两执行器共用的 LIKE 承担。
            if idx.rebuild_with_entries(store).is_ok() && idx.set_body_texts(store).is_ok() {
                index_rebuilt = true;
            }
        }
    }
    // --deep：全量内容摘要核对（维护审计）。
    let mut deep_report = serde_json::Value::Null;
    if deep {
        // H3：--deep = 磁盘 vs 事件日志（回填完整性复核），非自比自。
        let audit = mem_core::ops::deep_digest_audit(store)?;
        deep_report = serde_json::json!({
            "disk_vs_event_log": audit,
        });
    }
    out_json(&serde_json::json!({
        "backfill_appended": report.appended.len(),
        "backfill_events": report.appended,
        "skipped_stable": report.skipped_stable,
        "warnings": report.warnings,
        "pending_commits_retried": retried,
        "index_rebuilt": index_rebuilt,
        "checkpoint_saved": false,
        "checkpoint_note": "检查点仅由 sidecar 收敛原语发布（r4 H2/B3）",
        "deep": deep_report,
    }));
    Ok(())
}

// ---------- list / find / show / trace ----------

fn list_items(
    store: &Store,
    view: Option<&str>,
    type_filter: Option<&str>,
    limit: Option<usize>,
    offset: Option<usize>,
) -> MemResult<mem_core::view::ViewResult> {
    let limit = mem_core::budget::effective_limit(store, limit);
    let offset = offset.unwrap_or(0);
    if let Some(v) = view {
        return mem_core::view::execute_from(store, v, limit, offset);
    }
    let filter = mem_core::view::ViewFilter {
        types: type_filter.map(|t| vec![t.to_string()]).unwrap_or_default(),
        ..Default::default()
    };
    mem_core::view::execute_filter(store, &filter, limit, offset)
}

fn cmd_list(
    store: &Store,
    view: Option<&str>,
    type_filter: Option<&str>,
    limit: Option<usize>,
    offset: Option<usize>,
) -> MemResult<()> {
    let limit = mem_core::budget::effective_limit(store, limit);
    let offset = offset.unwrap_or(0);
    let (mode, index) = fastpath(store);
    // 快路径：类型过滤（或全库）走索引；视图引用过滤仍直读（v1 索引仅信封标量域）。
    let fast = match (&index, view.is_none()) {
        (Some(idx), true) => match type_filter {
            Some(t) => idx.query_by_type(t, limit, offset).ok(),
            None => idx.query_all(limit, offset).ok(),
        },
        _ => None,
    };
    match fast {
        Some(rows) => {
            out_json(&serde_json::json!({
                "read_mode": mode,
                "items": rows.iter().map(index_row_to_json).collect::<Vec<_>>(),
                "truncated": rows.len() == limit,
                "resume": if rows.len() == limit { serde_json::json!({"offset": offset + rows.len()}) } else { serde_json::Value::Null },
            }));
            Ok(())
        }
        None => {
            let r = list_items(store, view, type_filter, Some(limit), Some(offset))?;
            out_json(&serde_json::json!({
                "read_mode": mode,
                "items": r.items.iter().map(|i| serde_json::json!({
                    "id": i.envelope.id, "type": i.envelope.r#type, "rel": i.rel,
                    "modified": i.envelope.modified, "version": i.envelope.version,
                    "token": i.token.to_string(),
                })).collect::<Vec<_>>(),
                "truncated": r.truncated,
                "resume": r.resume_hint.map(|h| serde_json::json!({"offset": h.offset})),
            }));
            Ok(())
        }
    }
}

fn cmd_find(store: &Store, query: &str, limit: Option<usize>, offset: Option<usize>) -> MemResult<()> {
    let limit = mem_core::budget::effective_limit(store, limit);
    let offset = offset.unwrap_or(0);
    let (mode, index) = fastpath(store);
    // 快路径：折叠列 LIKE 粗筛（仅快路径可用，D5）。
    if let Some(idx) = &index {
        // r12 B7-1：LIKE 折叠列只是**粗筛**（保守超集——跨值假命中可能、永不漏），
        // 命中语义由与 direct 同源的 search::record_matches 精筛裁定；分页必须在
        // 精筛后按命中序号进行（SQL 层预分页会把页边界画在未精筛的行上）。
        // r5 B7：查询失败不得伪装为空命中——Err → 直读兜底（reason 携带真实错误）。
        let candidates = match idx.search(query, usize::MAX, 0) {
            Ok(rows) => rows,
            Err(e) => {
                return cmd_find_direct(store, query, limit, Some(offset),
                    &mem_core::freshness::ReadMode::Direct {
                        reason: format!("索引查询失败——直读兜底（红线8）：{e}"),
                    });
            }
        };
        let q = mem_core::paths::fold(query);
        // 索引行可能领先/落后磁盘（红线8：索引不 gate 正确性）——精筛按磁盘真相
        // 逐条裁定；文件已消失的行（删除竞态残影）跳过。
        let mut hits = Vec::new();
        for row in &candidates {
            let Ok(rec) = ops::get(store, &row.id) else { continue };
            let env = &rec.record.envelope;
            if mem_core::search::record_matches(
                &env.id, &env.aliases, &rec.record.payload, &rec.record.body, &q,
            ) {
                hits.push(rec);
            }
        }
        // hits 保持索引序（SQL：modified DESC, id DESC）——与 direct 排序规则一致。
        let truncated = hits.len() > offset + limit;
        let page = &hits[offset.min(hits.len())..hits.len().min(offset + limit)];
        out_json(&serde_json::json!({
            "read_mode": mode,
            "hits": page.iter().map(|r| serde_json::json!({
                "id": r.record.envelope.id, "type": r.record.envelope.r#type, "rel": r.rel,
                "token": r.token.to_string(),
            })).collect::<Vec<_>>(),
            "limit": limit,
            "truncated": truncated,
            "resume": if truncated { serde_json::json!({"offset": offset + limit}) } else { serde_json::Value::Null },
            "note": if truncated { "命中截断（红线9）——用 --offset <resume.offset> 续读" } else { "" },
        }));
        return Ok(());
    }
    cmd_find_direct(store, query, limit, Some(offset), &mode)
}

fn cmd_find_direct(
    store: &Store,
    query: &str,
    limit: usize,
    offset: Option<usize>,
    mode: &mem_core::freshness::ReadMode,
) -> MemResult<()> {
    let offset = offset.unwrap_or(0);
    let mut scan = ops::scan_all(store)?;
    // r7：分页顺序与 indexed 模式对齐（modified DESC, id DESC）——否则两页顺序
    // 相反，逐 ID 对比与续读语义双双失效。
    scan.records.sort_by(|a, b| {
        b.record
            .envelope
            .modified
            .cmp(&a.record.envelope.modified)
            .then(b.record.envelope.id.cmp(&a.record.envelope.id))
    });
    let q = mem_core::paths::fold(query);
    // r11：续读语义统一为**命中序号**（与 indexed 的 SQL OFFSET 同语义——跳过前
    // N 个命中行）。此前 offset 是扫描记录偏移：direct 第 0 页 resume=扫描位置，
    // 拿到 indexed 当命中偏移 → 漏命中；反向 direct 重复第一页。
    // r12 B7-1：命中判定收敛到 search::record_matches 单源（与 indexed 精筛同一
    // 函数——两执行器命中面结构性等价，不再各自维护匹配逻辑）。
    let mut hits = Vec::new();
    let mut hit_seen = 0usize;
    let mut remaining_after_stop = false;
    for r in &scan.records {
        let env = &r.record.envelope;
        if mem_core::search::record_matches(
            &env.id, &env.aliases, &r.record.payload, &r.record.body, &q,
        ) {
            if hit_seen >= offset {
                if hits.len() < limit {
                    hits.push(r);
                } else {
                    // 命中达到 limit 后还有命中 → 截断标注 + 续读路径（红线9/H5）。
                    remaining_after_stop = true;
                    break;
                }
            }
            hit_seen += 1;
        }
    }
    let truncated = remaining_after_stop;
    out_json(&serde_json::json!({
        "read_mode": mode,
        "hits": hits.iter().map(|r| serde_json::json!({
            "id": r.record.envelope.id, "type": r.record.envelope.r#type, "rel": r.rel,
            "token": r.token.to_string(),
        })).collect::<Vec<_>>(),
        "limit": limit,
        "truncated": truncated,
        "resume": if truncated { serde_json::json!({"offset": offset + hits.len()}) } else { serde_json::Value::Null },
        "note": if truncated { "命中截断（红线9）——用 --offset <resume.offset> 续读" } else { "" },
    }));
    Ok(())
}

fn cmd_show(
    store: &Store,
    id: &str,
    with_body: bool,
    body_offset: Option<usize>,
    body_out: Option<&str>,
) -> MemResult<()> {
    // r12 B7-3：预算是配置错误——先于一切工作响亮拒收（不因 id 无效而先报别的错）。
    let budget = mem_core::budget::brief_bytes_budget(store)?;
    let hit = ops::get(store, id)?;
    let bytes = std::fs::read(store.abs(&hit.rel)).unwrap_or_default();
    // R11.2（G1/G2）：分页双通道——合法 UTF-8 正文走文本通道（切点码点对齐，
    // 数学上字节保真）；非合法正文绝不经有损解码，预览降级为占位描述符（BD3），
    // 字节经 --body-out 取回（BD1）。body_utf8 恒有机判（BD3）。
    let body_utf8 = mem_core::body::is_valid_utf8(&hit.record.body);
    // BD1：--body-out 全量落文件，stdout 只回元信息（路径/字节数/正文 SHA-256）。
    let body_out_meta = match body_out {
        Some(spec) => Some(write_body_out(store, spec, &hit.record.body)?),
        None => None,
    };
    let (body_value, body_truncated, resume) = if body_out.is_some() || !with_body {
        // 字节经文件交付 / --no-body：stdout 不重复正文。
        (serde_json::Value::Null, false, serde_json::Value::Null)
    } else if body_utf8 {
        let (text, truncated, next_off) = mem_core::body::body_page_text(
            &hit.record.body,
            body_offset.unwrap_or(0),
            budget,
        )
        .expect("合法 UTF-8 正文必有文本通道");
        let resume = if truncated {
            serde_json::json!({ "body_offset": next_off })
        } else {
            serde_json::Value::Null
        };
        (serde_json::json!(text), truncated, resume)
    } else {
        // BD3 占位描述符：不分页（描述符后 resume=null）。
        (
            serde_json::json!(mem_core::body::non_text_descriptor(
                &hit.record.envelope.id,
                &hit.record.body
            )),
            false,
            serde_json::Value::Null,
        )
    };
    out_json(&serde_json::json!({
        "rel": hit.rel,
        "envelope": hit.record.envelope.to_json_value(),
        "payload": hit.record.payload,
        "body": body_value,
        "body_utf8": body_utf8,
        "body_out": body_out_meta,
        "body_bytes_total": hit.record.body.len(),
        "body_truncated": body_truncated,
        "resume": resume,
        "token": hit.token.to_string(),
        "bytes": bytes.len(),
    }));
    Ok(())
}

/// BD1：`--body-out` 落文件（读路径唯一的"写盘"——写的是调用方指定的工作区路径，
/// 不是店内任何真相/缓存/锁/事件；G3：无机制副作用，只读存储上同样可用）。
/// 约束两条：目标已存在 → 响亮拒绝；目标位于店目录内 → 响亮拒绝。
///
/// B2（实施评审）："不覆盖"由**原子独占创建**承担（`platform::create_new`——完整
/// 字节写同目录临时文件并同步，再以不覆盖的原子发布交付；目标已存在 → 发布失败
/// 且目标分毫不动）。此前的 `path.exists()` 检查与 `fs::write` 分离存在 TOCTOU
/// 竞态：目标在检查后被并发创建会被截断覆盖——检查与发布合一后窗口不复存在，
/// 并发正确性不依赖事后检测。
fn write_body_out(store: &Store, spec: &str, body: &[u8]) -> MemResult<serde_json::Value> {
    let path = std::path::Path::new(spec);
    if mem_core::paths::path_is_inside(store.root(), path) {
        return Err(MemError::BadArgument(format!(
            "--body-out 目标位于店目录内（防止经读通道在店内产生信封管辖外的旁挂文件）：{spec}"
        )));
    }
    match mem_core::platform::create_new(path, body) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(MemError::BadArgument(format!(
                "--body-out 目标已存在（读命令不做覆盖写；既有文件原字节保持）：{spec}"
            )));
        }
        Err(e) => return Err(MemError::io(spec, e)),
    }
    Ok(serde_json::json!({
        "path": path.display().to_string(),
        "bytes": body.len(),
        "sha256": mem_core::token::sha256_hex(body),
    }))
}

fn cmd_trace(
    store: &Store,
    id: &str,
    dir: &str,
    depth: Option<usize>,
    limit: Option<usize>,
    skip: Option<usize>,
) -> MemResult<()> {
    let (max_depth, max_nodes) = mem_core::budget::trace_budget(store);
    let depth = depth.unwrap_or(max_depth).min(max_depth);
    let nodes = limit.unwrap_or(max_nodes).min(max_nodes);
    let direction = match dir {
        "up" => mem_core::refgraph::TraceDirection::Up,
        "down" => mem_core::refgraph::TraceDirection::Down,
        other => {
            return Err(MemError::BadArgument(format!("--dir 只接受 up|down，实际 {other}")))
        }
    };
    let skip_n = skip.unwrap_or(0);
    let outcome = mem_core::refgraph::trace_from(store, id, direction, depth, nodes, skip_n)?;
    let nodes_out = &outcome.nodes;
    let truncated = outcome.truncated;
    let frontier = &outcome.frontier;
    // r6/r8/r9：truncated 可能来自节点预算或深度预算——resume 按原因给出**可执行**续读。
    // r8：depth 已达预算上限（trace_depth）时，--depth+1 会被钳回——如实标注
    // "预算已用尽，需调大 mem.json budgets.trace_depth"。
    // r9：深度截断必须带 **frontier**（被深度挡住的未访问邻居）——对每个
    // frontier 节点 `trace <id> --dir <同向> --depth <预算>` 即可继续深入
    //（可执行、各子树独立不重复），替代"调大全局预算重跑"的粗粒度指引。
    let depth_limited = truncated && nodes_out.len() < nodes;
    out_json(&serde_json::json!({
        "start": id,
        "direction": dir,
        "nodes": nodes_out.iter().map(|n| serde_json::json!({"id": n.id, "rel": n.rel, "depth": n.depth})).collect::<Vec<_>>(),
        "nodes_seen": nodes_out.len(),
        "truncated": truncated,
        "resume": if truncated {
            if depth_limited {
                if depth >= max_depth {
                    // r9：可执行续读——frontier 逐节点继续 trace（各子树独立不重复）。
                    serde_json::json!({
                        "reason": "深度预算已用尽",
                        "budget_depth": max_depth,
                        "frontier": frontier.iter().map(|f| serde_json::json!({
                            "id": f,
                            "next": format!("mem trace {f} --dir {dir} --depth {}（继续深入）", max_depth),
                        })).collect::<Vec<_>>(),
                        "note": format!("深度预算 trace_depth={} 已到底——按 frontier 逐节点继续 trace（无需调大预算）", max_depth),
                    })
                } else {
                    serde_json::json!({
                        "reason": "深度预算截断",
                        "suggest": format!("mem trace {id} --dir {dir} --depth {}（提高深度重跑）", depth + 1),
                    })
                }
            } else {
                serde_json::json!({
                    "reason": "节点预算截断",
                    "skip": skip_n + nodes_out.len(),
                    "limit": nodes + 1,
                    "depth": depth,
                    "suggest": format!("mem trace {id} --dir {dir} --skip {} --limit {}（续读）", skip_n + nodes_out.len(), nodes + 1),
                })
            }
        } else { serde_json::Value::Null },
    }));
    Ok(())
}

// ---------- 写命令 ----------

fn write_ctx<'a>(
    store: &'a Store,
    types: &'a TypeSet,
    session_id: Option<&'a str>,
) -> WriteContext<'a> {
    // L2 清理：提交汇经零 sized 静态（LeakSink → sink_for_current），无泄漏脚手架。
    let principal_label = session::get(store, session_id.unwrap_or(""))
        .ok()
        .flatten()
        .map(|l| l.principal)
        .unwrap_or_default();
    WriteContext {
        store,
        types,
        by: format!("{principal_label}(cli)"),
        // 有 .git 才挂提交汇（无 git 形态合法，红线10）。
        committer: if store.root().join(".git").exists() { Some(&CliSink) } else { None },
        session_id,
    }
}

/// CLI 写命令的提交汇（经 open_store 时登记的 CURRENT_STORE 定位店根）。
struct CliSink;
impl CommitSink for CliSink {
    fn commit(&self, paths: &[String]) -> MemResult<Option<String>> {
        sink_for_current().commit(paths)
    }
}



static CURRENT_STORE: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
fn sink_for_current() -> GitProcess {
    let root = CURRENT_STORE.get().cloned().unwrap_or_default();
    GitProcess::new(root)
}

fn parse_token(s: &str) -> MemResult<Token> {
    s.parse::<Token>().map_err(MemError::BadArgument)
}

fn cmd_add(
    store: &Store,
    types: &TypeSet,
    type_name: &str,
    payload: &str,
    body: Option<&str>,
    principal: Option<&str>,
    session_id: Option<&str>,
) -> MemResult<()> {
    let payload_v: serde_json::Value = serde_json::from_str(payload)
        .map_err(|e| MemError::BadArgument(format!("--payload 不是合法 JSON：{e}")))?;
    let principal_id = principal
        .map(|p| resolve_principal(store, p))
        .transpose()?;
    let ctx = write_ctx(store, types, session_id);
    let out = ops::add(
        &ctx,
        ops::AddArgs {
            type_name: type_name.to_string(),
            payload: payload_v,
            body: read_body(body)?,
            principal: principal_id,
        },
    )?;
    out_json(&serde_json::json!({
        "created": out.created,
        "id": out.record.envelope.id,
        "rel": out.rel,
        "token": out.token.to_string(),
        "warnings": out.warnings,
    }));
    Ok(())
}

fn cmd_update(
    store: &Store,
    types: &TypeSet,
    id: &str,
    expect: &str,
    payload: &str,
    body_file: Option<&str>,
    session_id: Option<&str>,
) -> MemResult<()> {
    let payload_v: serde_json::Value = serde_json::from_str(payload)
        .map_err(|e| MemError::BadArgument(format!("--payload 不是合法 JSON：{e}")))?;
    let expect_t = parse_token(expect)?;
    let body = if body_file.is_some() { Some(read_body(body_file)?) } else { None };
    let ctx = write_ctx(store, types, session_id);
    let out = ops::update(&ctx, id, &expect_t, payload_v, body)?;
    out_json(&serde_json::json!({
        "id": out.record.envelope.id,
        "rel": out.rel,
        "version": out.record.envelope.version,
        "token": out.token.to_string(),
        "warnings": out.warnings,
    }));
    Ok(())
}

fn cmd_link(
    store: &Store,
    types: &TypeSet,
    a: &str,
    b: &str,
    rel: &str,
    expect_a: &str,
    expect_b: &str,
) -> MemResult<()> {
    let ctx = write_ctx(store, types, None);
    let (ta, tb) = ops::link(&ctx, a, b, rel, &parse_token(expect_a)?, &parse_token(expect_b)?)?;
    out_json(&serde_json::json!({ "a_token": ta.to_string(), "b_token": tb.to_string() }));
    Ok(())
}

fn cmd_merge(
    store: &Store,
    types: &TypeSet,
    dup: &str,
    canonical: &str,
    expect_dup: &str,
    expect_canonical: &str,
) -> MemResult<()> {
    let ctx = write_ctx(store, types, None);
    let m = ops::merge(
        &ctx,
        dup,
        canonical,
        &parse_token(expect_dup)?,
        &parse_token(expect_canonical)?,
    )?;
    out_json(&serde_json::json!({
        "canonical": m.canonical, "dup": m.dup, "repointed": m.repointed,
        "canonical_token": m.canonical_token.to_string(),
    }));
    Ok(())
}

fn cmd_delete(store: &Store, types: &TypeSet, id: &str, expect: &str, force: bool) -> MemResult<()> {
    let ctx = write_ctx(store, types, None);
    let d = ops::delete_record(&ctx, id, &parse_token(expect)?, force)?;
    out_json(&serde_json::json!({ "tombstone": d.rel, "warnings": d.warnings }));
    Ok(())
}

fn cmd_restore(store: &Store, types: &TypeSet, id: &str) -> MemResult<()> {
    let ctx = write_ctx(store, types, None);
    let r = ops::restore_record(&ctx, id, None)?;
    out_json(&serde_json::json!({ "rel": r.rel, "token": r.token.to_string() }));
    Ok(())
}

// ---------- diff / bind / verify ----------

fn cmd_diff(store: &Store, since: Option<u64>, offset: Option<usize>) -> MemResult<()> {
    let cfg = store.config().budgets;
    // r4 B7：changes_since 支持组级 offset 续读（红线9：截断必须附可执行续读方式）；
    // 全量取组数再分页，保证精确截断判定。
    let all = mem_core::drift::diff_since(
        store,
        since.unwrap_or(0),
        usize::MAX,
        cfg.current_drift_limit as usize,
    )?;
    let total = all.changes_since.len();
    let page: Vec<_> = all
        .changes_since
        .into_iter()
        .skip(offset.unwrap_or(0))
        .take(cfg.diff_since_limit as usize)
        .collect();
    let consumed = offset.unwrap_or(0) + page.len();
    let truncated = consumed < total;
    out_json(&serde_json::json!({
        "since": since,
        "changes_since": page.iter().map(|g| serde_json::json!({
            "id": g.id,
            "last_event": g.last_event,
            "history_len": g.history.len(),
        })).collect::<Vec<_>>(),
        "changes_truncated": truncated,
        "resume": if truncated { serde_json::json!({"offset": consumed}) } else { serde_json::Value::Null },
        "total_groups": total,
        "current_drift": all.current_drift.items.iter().map(|i| serde_json::json!({
            "id": i.id, "last_path": i.last_path, "cur_path": i.cur_path,
            "note": i.note, "token": i.token.as_ref().map(|t| t.to_string()),
            "mtime": i.mtime, "readonly_note": i.readonly_note,
        })).collect::<Vec<_>>(),
        "drift_truncated": all.current_drift.truncated,
        "next": "current_drift 项 → mem reconcile 回填后转正（drift 非续读流——回填后转入 changes_since）",
    }));
    Ok(())
}

fn cmd_bind(store: &Store, view: &str) -> MemResult<()> {
    store.check_writable()?;
    // 视图存在性（显式声明；只选择已声明视图，红线11）。
    let v = ops::get(store, view)?;
    if v.record.envelope.r#type != "view" {
        return Err(MemError::BadArgument(format!("{view} 不是 view 记录")));
    }
    let mem_json = store.abs("mem.json");
    let mut cfg_v: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&mem_json).map_err(|e| MemError::io(mem_json.display().to_string(), e))?)
            .map_err(|e| MemError::Corrupt { path: "mem.json".into(), reason: e.to_string() })?;
    cfg_v["default_view"] = serde_json::Value::String(v.record.envelope.id.clone());
    let bytes = serde_json::to_vec_pretty(&cfg_v).unwrap();
    mem_core::platform::atomic_write(&mem_json, &bytes)
        .map_err(|e| MemError::io(mem_json.display().to_string(), e))?;
    out_json(&serde_json::json!({ "default_view": v.record.envelope.id }));
    Ok(())
}

fn cmd_agenda(store: &Store, types: &TypeSet, scan: bool) -> MemResult<()> {
    if scan {
        store.check_writable()?;
        let proposals = mem_core::agenda::scan_agenda(store, types)?;
        let n = mem_core::agenda::write_proposals(store, &proposals)?;
        let _ = n;
    }
    let plans = mem_core::agenda::list_plans(store)?;
    out_json(&serde_json::json!({
        "plans": plans.iter().map(|p| serde_json::json!({
            "id": p.id, "kind": p.kind, "status": p.status, "steps": p.steps.len(),
        })).collect::<Vec<_>>(),
    }));
    Ok(())
}

fn cmd_apply(store: &Store, types: &TypeSet, plan_id: &str) -> MemResult<()> {
    let sink = sink_for(store);
    let committer: Option<&dyn mem_core::ops::CommitSink> =
        if store.root().join(".git").exists() { Some(&sink) } else { None };
    let r = mem_core::agenda::apply(store, types, plan_id, committer)?;
    out_json(&serde_json::json!({
        "plan": plan_id,
        "executed": r.executed,
        "warnings": r.warnings,
    }));
    Ok(())
}

fn cmd_fix(
    store: &Store,
    types: &TypeSet,
    id: &str,
    envelope: Option<&str>,
    payload: Option<&str>,
    expect: Option<&str>,
) -> MemResult<()> {
    store.check_writable()?;
    let git = mem_git::GitProcess::new(store.root());
    let provided = match (envelope, payload) {
        (None, None) => None,
        _ => {
            let env_v: serde_json::Value = serde_json::from_str(
                envelope.ok_or_else(|| {
                    MemError::BadArgument("fix 需要 --envelope 与 --payload 成对提供".into())
                })?,
            )
            .map_err(|e| MemError::BadArgument(format!("--envelope 不是合法 JSON：{e}")))?;
            let env = mem_core::envelope::Envelope::from_json(&env_v)
                .map_err(|reason| MemError::EnvelopeInvalid { path: id.into(), reason })?;
            let payload_v: serde_json::Value = serde_json::from_str(
                payload.ok_or_else(|| {
                    MemError::BadArgument("fix 需要 --envelope 与 --payload 成对提供".into())
                })?,
            )
            .map_err(|e| MemError::BadArgument(format!("--payload 不是合法 JSON：{e}")))?;
            Some((env, payload_v))
        }
    };
    let expect_t = expect.map(parse_token).transpose()?;
    // B1：fix 修复版本与事件同批入史（有 .git 才挂提交汇——无 git 形态合法，红线10）。
    let committer: Option<&dyn mem_core::ops::CommitSink> =
        if store.root().join(".git").exists() { Some(&git) } else { None };
    let out =
        mem_core::ops::fix_record(store, types, id, Some(&git), provided, expect_t.as_ref(), committer)?;
    out_json(&serde_json::json!({
        "rel": out.rel,
        "id": out.record.envelope.id,
        "token": out.token.to_string(),
    }));
    Ok(())
}

/// BD4：恢复正文（经确认的内容写；来源三形解析 + 出处回显）。
fn cmd_restore_body(
    store: &Store,
    types: &TypeSet,
    id: &str,
    expect: &str,
    source: &str,
) -> MemResult<()> {
    store.check_writable()?;
    let expect_t = parse_token(expect)?;
    let source_spec = parse_restore_source(source)?;
    let history = mem_git::GitProcess::new(store.root());
    let ctx = write_ctx(store, types, None);
    let out = ops::restore_body(&ctx, id, &expect_t, &source_spec, Some(&history))?;
    out_json(&serde_json::json!({
        "id": out.record.envelope.id,
        "rel": out.rel,
        "version": out.record.envelope.version,
        "token": out.token.to_string(),
        "source": out.source_echo,
    }));
    Ok(())
}

/// 来源三形解析：`git:<版本>` | `file:<店外路径>` | `history`（§3.6）。
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
        "--source 必须为 git:<版本> | file:<店外路径> | history，实际 {spec:?}"
    )))
}

fn cmd_verify(store: &Store, types: &TypeSet, deep: bool) -> MemResult<()> {
    let scan = ops::scan_all(store)?;
    let log = EventLog::new(store).read_log()?;
    let holes = mem_core::event::chain_audit(&log.events);
    // 断链注记（悬空引用：R3.4 显式报告语义；引用按 ID，D11）。
    let mut dangling = Vec::new();
    let ids: std::collections::HashSet<_> =
        scan.records.iter().map(|r| r.record.envelope.id.clone()).collect();
    for r in &scan.records {
        for e in &r.record.envelope.refs {
            if !ids.contains(&e.target) {
                dangling.push(serde_json::json!({
                    "from": r.record.envelope.id, "rel": e.rel, "target": e.target,
                }));
            }
        }
    }
    // H3：--deep 对照事件日志（此前自比自检不出任何东西）。
    let mut digest_mismatches = Vec::new();
    if deep {
        digest_mismatches = mem_core::ops::deep_digest_audit(store)?;
    }
    // r5 B4：镜像一致性由 agenda::verify_report 内部检查（payload x-ref vs
    // envelope.refs + payload 悬空）。
    let mirror_report = mem_core::ops::mirror_consistency_audit(store, types)?;
    let plan_report = mem_core::agenda::verify_report(store, types, deep)?;
    // BD5 ④：.gitattributes 提交树核验（有 .git 的店；完成标记 = 提交树核验，
    // 非工作区文件存在）。
    let has_git = store.root().join(".git").exists();
    let head_attr = if has_git {
        sink_for(store)
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
    out_json(&serde_json::json!({
        "ok": ok,
        "scanned": scan.records.len(),
        "warnings": scan.warnings,
        "history_holes": holes,
        "dangling_refs": dangling,
        "payload_mirror_inconsistencies": mirror_report,
        "digest_mismatches": digest_mismatches,
        "gitattributes_audit": gitattrs_audit,
        "plans": plan_report,
    }));
    Ok(())
}
