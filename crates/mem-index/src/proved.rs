//! 构造即证明（R10 决策②/红线21——快路径的唯一证明点，keyword-search-design.md §2.1）。
//!
//! `open_proved` 是 [`ProvedIndex`] 的唯一构造路径：stat 咨询（mem-core freshness
//! 既有通道：检查点 ∧ 订阅覆盖）→ 只读打开（不建库不建表，红线18）→ 只读 schema
//! 门禁（records/refs/meta/bad_records 及折叠列完整——旧代索引缺表即不通过）→
//! epoch 代际核对（检查点与索引同代，防旧检查点配到删除重建/未完成物化的新索引）。
//! 全部通过才产出已证明句柄；任一失败 → `index = None`（调用方直读并标注 reason）。
//!
//! 证明逻辑不出现在 [`mem_core::find::FindPrefilter`]（trait 只承载已证明句柄的两个
//! 查询）、不进入执行器（分流依据只有一个："是否拿到已证明句柄"）、不留在前端
//! （CLI 与 MCP 前端循环调用同一 open_proved 组装）。既有 CLI 快路径的全部门禁
//! 随下沉完整收进（评审 B3——非省略）。

use std::sync::Mutex;

use mem_core::error::MemResult;
use mem_core::find::{FindPrefilter, PrefilterRow};
use mem_core::freshness::ReadMode;
use mem_core::sidecar::Hello;
use mem_core::store::Store;

use crate::sqlite::Index;

/// open_proved 的产物：mode 是同一构造的诚实标注（Indexed 或带 reason 的 Direct），
/// index 仅在全部证明成立时为 Some。
pub struct Proved {
    pub mode: ReadMode,
    pub index: Option<ProvedIndex>,
}

/// 已证明索引句柄（唯一构造路径：[`open_proved`]）。rusqlite Connection 非 Sync，
/// 以 Mutex 包装满足 FindPrefilter: Send + Sync。
pub struct ProvedIndex {
    conn: Mutex<Index>,
}

impl ProvedIndex {
    fn new(index: Index) -> Self {
        Self { conn: Mutex::new(index) }
    }

    /// cmd_view 既有快路径的兼容面（R10 检索执行器之外的既有索引消费者——
    /// 类型单维查询，SQL 层 LIMIT/OFFSET，排序 modified DESC, id DESC）。
    pub fn query_by_type(&self, type_name: &str, limit: usize, offset: usize) -> MemResult<Vec<crate::sqlite::IndexRow>> {
        let conn = self.conn.lock().expect("索引连接锁");
        conn.query_by_type(type_name, limit, offset)
    }

    /// cmd_brief 既有快路径的兼容面（全类型最近 N 条）。
    pub fn query_all(&self, limit: usize, offset: usize) -> MemResult<Vec<crate::sqlite::IndexRow>> {
        let conn = self.conn.lock().expect("索引连接锁");
        conn.query_all(limit, offset)
    }
}

/// 唯一证明点（两前端调同一函数）。`hello` = sidecar 握手（None = 嵌入模式，
/// 永远不可证明）。
pub fn open_proved(store: &Store, hello: Option<&Hello>) -> Proved {
    // 1) stat 咨询（freshness 既有：检查点 ∧ 订阅覆盖；只读存储同样在此否决）。
    let checkpoint = mem_core::freshness::load_checkpoint(store);
    let mode = mem_core::freshness::consult_with_hello(store, checkpoint.as_ref(), hello);
    if mode.is_direct() {
        return Proved { mode, index: None };
    }
    // 2) 只读打开 + 3) 只读 schema 门禁（records/refs/meta/bad_records + 折叠列）。
    let index = match Index::open_store_readonly(store) {
        Ok(i) => i,
        Err(e) => {
            return Proved {
                mode: ReadMode::Direct {
                    reason: format!("索引只读打开或 schema 门禁失败（读路径不建库，红线8/18）：{e}"),
                },
                index: None,
            }
        }
    };
    // 4) epoch 代际核对：检查点必须与索引同物化代——索引被删重建（epoch 重置）
    //    而检查点记旧代 → 直读，空/旧索引不被误信（r4 B2，既有门禁随下沉收进）。
    let epoch_ok = checkpoint
        .as_ref()
        .map(|cp| index.epoch().map(|e| e == cp.index_epoch).unwrap_or(false))
        .unwrap_or(false);
    if !epoch_ok {
        return Proved {
            mode: ReadMode::Direct {
                reason: "检查点与索引代际不匹配（物化进行中或索引被重建）——直读".into(),
            },
            index: None,
        };
    }
    Proved { mode, index: Some(ProvedIndex::new(index)) }
}

impl FindPrefilter for ProvedIndex {
    /// 多词项合取粗筛（§2.1 下推规则）：每个**正词项**生成一组 LIKE 条件
    /// （id 前缀 ∨ 三折叠列含匹配），词项间 AND——每词项的 LIKE 都是其谓词命中的
    /// 超集（载荷折叠列是全部值的折叠拼接：值内命中 ⊆ 拼接命中；id 前缀 LIKE 与
    /// 谓词同形），各自超集的交集仍是超集。**负词项与过滤不下推**：LIKE 命中不
    /// 代表记录命中，对近似条件取反会把真命中错误排除——只在精筛施加（红线8）。
    fn candidates(&self, terms_folded: &[String], types: Option<&[String]>) -> MemResult<Vec<PrefilterRow>> {
        let conn = self.conn.lock().expect("索引连接锁");
        let conn = &conn.conn;
        if types.map(|t| t.is_empty()).unwrap_or(false) {
            return Ok(vec![]); // 空类型集：候选为空（正确性无害——精筛亦必排除）
        }
        let esc = |s: &str| {
            s.replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        };
        // 参数序：每词项 4 个（id 前缀 + 三折叠列），类型收窄追加。
        let mut params: Vec<String> = Vec::new();
        let mut conds: Vec<String> = Vec::with_capacity(terms_folded.len() + 1);
        let mut n = 0usize;
        for raw_t in terms_folded {
            let t = super::sqlite::index_safe(raw_t);
            let pat_prefix = format!("{}%", esc(&t));
            let pat_sub = format!("%{}%", esc(&t));
            let (a, b, c, d) = (n + 1, n + 2, n + 3, n + 4);
            n += 4;
            params.push(pat_prefix);
            params.push(pat_sub.clone());
            params.push(pat_sub.clone());
            params.push(pat_sub);
            conds.push(format!(
                "(id LIKE ?{a} ESCAPE '\\' OR aliases_folded LIKE ?{b} ESCAPE '\\' \
                 OR payload_folded LIKE ?{c} ESCAPE '\\' OR body_folded LIKE ?{d} ESCAPE '\\')"
            ));
        }
        if let Some(ts) = types {
            let placeholders: Vec<String> = ts
                .iter()
                .map(|_| {
                    n += 1;
                    format!("?{n}")
                })
                .collect();
            conds.push(format!("type IN ({})", placeholders.join(", ")));
            params.extend(ts.iter().cloned());
        }
        let where_clause = if conds.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", conds.join(" AND "))
        };
        let sql = format!(
            "SELECT id, type, rel, version, digest, modified FROM records{where_clause} \
             ORDER BY modified DESC, id DESC"
        );
        let mut stmt = conn.prepare(&sql).map_err(sq_err)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(params.iter()), |r| {
                Ok(PrefilterRow {
                    id: r.get(0)?,
                    type_name: r.get(1)?,
                    rel: r.get(2)?,
                    version: r.get::<_, i64>(3)? as u64,
                    digest: r.get(4)?,
                    modified: r.get(5)?,
                })
            })
            .map_err(sq_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sq_err)?;
        Ok(rows)
    }

    /// 坏记录清单（决策⑦）：rel 升序最多 cap 条 + 是否还有更多。排序先行保证
    /// 与 direct（扫描枚举序）截断出的子集逐条一致（§2.5）。
    fn bad_records(&self, cap: usize) -> MemResult<(Vec<(String, String)>, bool)> {
        let conn = self.conn.lock().expect("索引连接锁");
        let conn = &conn.conn;
        let mut stmt = conn
            .prepare("SELECT rel, error FROM bad_records ORDER BY rel ASC LIMIT ?1")
            .map_err(sq_err)?;
        let rows = stmt
            .query_map(rusqlite::params![cap as i64 + 1], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(sq_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sq_err)?;
        let more = rows.len() > cap;
        let mut out = rows;
        out.truncate(cap);
        Ok((out, more))
    }
}

fn sq_err(e: rusqlite::Error) -> mem_core::error::MemError {
    mem_core::error::MemError::Corrupt { path: "index/index.sqlite".into(), reason: e.to_string() }
}
