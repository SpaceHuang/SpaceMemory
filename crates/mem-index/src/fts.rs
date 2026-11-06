//! 全文检索（FTS）：仅快路径可用。
//!
//! 直读模式下不可用，显式标注"全文检索需要 sidecar 在场"（D5）；检索升级坡道
//! （sqlite-vec / tantivy）暂不引入（判断 3：小到中等规模下"结构化摘要即索引"
//! 视为达标）。
//!
//! v1：FTS5 虚表（payload_json + 正文摘录不索引正文——正文是自由文本，索引只
//! 覆盖 payload 信封镜像层；正文检索属直读子串扫描，见 mem find）。

use mem_core::error::{MemError, MemResult};

use crate::sqlite::{Index, IndexRow};

pub const UNAVAILABLE_NOTE: &str = "全文检索需要 sidecar 在场（直读模式不可用，D5）";

impl Index {
    /// FTS 查询（仅快路径；调用方必须先通过新鲜性证明）。
    ///
    /// 分词注记：FTS5 unicode61 不切 CJK——查询串自动加前缀 `*`（`登录` → `登录*`
    /// 命中 token `登录接口设计`）；完整 CJK 短语检索属检索升级坡道（判断 3，暂缓）。
    /// FTS 查询（仅快路径；调用方必须先通过新鲜性证明）。
    /// r4 B7：offset/稳定排序（modified 降序 + id 兜底）——分页不重复、不遗漏，
    /// 与直读模式命中面对齐。
    pub fn search(&self, q: &str, limit: usize, offset: usize) -> MemResult<Vec<IndexRow>> {
        // r6 B7：**子串语义**与直读对齐。FTS5 MATCH 是 token/前缀匹配——查询
        // "hared-al"（shared-alpha 的中段子串）或 "ello"（hello 的后段子串）
        // 时零命中，而直读是 contains 子串命中——同一查询两个执行器给出不同
        // 命中面。改为对 records 表 LIKE 扫描（id/别名/payload_json/正文）：
        // 中小规模下（判断 3：结构化摘要即索引达标）成本可接受，语义与直读一致。
        // FTS 虚表保留（token 精确检索的升级坡道，判断 3 规模增长时启用）。
        // r7：a) LIKE 通配符（%/_）转义——字面 "%" 查询此前变成 "%%" 匹配全部；
        // b) 非ASCII 大小写：SQLite LIKE 对非 ASCII 大小写**敏感**（"ärger" 不匹配
        //    "Ärger"）——fold 匹配改为对**预折叠列**（payload_folded/body_folded）
        //    精确 LIKE，列内容在物化时已 fold，查询模式同为 fold。
        // r12 B7-1：本查询降格为**粗筛**（保守超集：LF 拼接列对含 LF 查询只多报
        // 不漏报）——命中语义由调用方（CLI find）用与 direct 同源的
        // mem_core::search::record_matches 对候选精筛后分页；limit=usize::MAX
        // 表示不限量候选（SQLite LIMIT -1），供精筛分页使用。
        let esc = |s: &str| {
            s.replace("\\", "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        };
        let pat_id_prefix = format!("{}%", esc(q));
        let pat_fold = format!("%{}%", esc(&crate_fold(q)));
        let lim: i64 = if limit == usize::MAX { -1 } else { limit as i64 };
        let mut stmt = self
            .conn
            .prepare(
                "SELECT r.id, r.type, r.rel, r.version, r.digest, r.modified, r.principal, r.location, r.payload_json, r.payload_folded, r.body_folded, r.aliases_folded
                 FROM records r
                 WHERE (r.id LIKE ?1 ESCAPE '\\')
                    OR (r.aliases_folded LIKE ?2 ESCAPE '\\')
                    OR (r.payload_folded LIKE ?2 ESCAPE '\\')
                    OR (r.body_folded LIKE ?2 ESCAPE '\\')
                 ORDER BY r.modified DESC, r.id DESC LIMIT ?3 OFFSET ?4",
            )
            .map_err(|e| MemError::Corrupt { path: "index".into(), reason: e.to_string() })?;
        let rows = stmt
            .query_map(
                rusqlite::params![pat_id_prefix, pat_fold, lim, offset as i64],
                |r| {
                Ok(IndexRow {
                    id: r.get(0)?,
                    type_name: r.get(1)?,
                    rel: r.get(2)?,
                    version: r.get::<_, i64>(3)? as u64,
                    digest: r.get(4)?,
                    modified: r.get(5)?,
                    principal: r.get(6)?,
                    location: r.get(7)?,
                    payload_json: r.get(8)?,
                    payload_folded: r.get(9)?,
                    body_folded: r.get(10)?,
                    aliases_folded: r.get(11)?,
                })
            })
            .map_err(|e| MemError::Corrupt { path: "index".into(), reason: e.to_string() })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| MemError::Corrupt { path: "index".into(), reason: e.to_string() })?;
        Ok(rows)
    }
}

/// r9：查询折叠与列折叠**同源**——直接复用 mem_core::paths::fold（NFC + 小写）。
/// 此前本地实现只有 to_lowercase（无 NFC）：NFD 形式的查询串（如 ärger 的分解
/// 形式）折叠后仍是 NFD，与 NFC 折叠列不匹配——direct（fold 全程同源）命中而
/// indexed 漏命中。
fn crate_fold(s: &str) -> String {
    mem_core::paths::fold(s)
}

