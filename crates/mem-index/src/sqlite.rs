//! SQLite 索引库：表结构、写例程物化、查询（LIMIT + 续读游标）。
//!
//! 边表支撑 `mem trace` 多跳遍历（D16）；过滤/排序/分页只依赖信封字段（R8.4）。
//! 索引约束（主键）仅作对账诊断——物化以真相层为准（红线8）。

use std::path::Path;

use rusqlite::Connection;

use mem_core::error::{MemError, MemResult};
use mem_core::ops::{self, Scanned};
use mem_core::store::Store;

#[derive(Debug, Clone, PartialEq)]
pub struct IndexRow {
    pub id: String,
    pub type_name: String,
    pub rel: String,
    pub version: u64,
    /// 全文件 SHA-256（令牌组成部分；快路径响应可附带完整令牌，红线19）。
    pub digest: String,
    pub modified: String,
    pub principal: Option<String>,
    pub location: String, // records | attic
    /// 直读扫描时的 payload JSON（lenient 索引；索引永不解释语义，R8.2——只存字节）。
    pub payload_json: String,
    /// r11：折叠列与正文/别名列——由物化器（rebuild_with_entries）一次性写入，
    /// sync_fts **不再覆盖**（r11 复现：sync_fts 用旧空格拼接覆盖 set_body_texts
    /// 写入的正确折叠列，跨值查询假命中）。
    pub payload_folded: String,
    pub body_folded: String,
    pub aliases_folded: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RefRow {
    pub from_id: String,
    pub rel: String,
    pub target: String,
}

pub struct Index {
    pub(crate) conn: Connection,
}

/// r10 B7-2：payload 折叠列内容——**逐值独立 fold** 后以 LF 连接。
/// LF 分隔符保证 LIKE 子串匹配不跨值（direct 的 payload_contains 是逐值
/// contains，两执行器命中面等价）；r7 的空格连接版本会跨值假命中。
pub fn payload_values_folded(payload: &serde_json::Value) -> String {
    fn walk(v: &serde_json::Value, out: &mut Vec<String>) {
        match v {
            serde_json::Value::String(s) => out.push(mem_core::paths::fold(s)),
            serde_json::Value::Number(n) => out.push(n.to_string()),
            serde_json::Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
            serde_json::Value::Object(o) => o.values().for_each(|x| walk(x, out)),
            _ => {}
        }
    }
    let mut parts = Vec::new();
    walk(payload, &mut parts);
    parts.join("\n")
}

/// FTS 索引文本：机械递归提取 payload 的**字符串值**（键名不进索引——否则
/// title/type 等公共键名让任何前缀查询命中全部记录）。
/// 语义零知识：只按 JSON 结构收集值，不解释字段含义（R8.2/红线1 同族纪律）。
pub fn values_text(payload: &serde_json::Value) -> String {
    fn walk(v: &serde_json::Value, out: &mut String) {
        match v {
            serde_json::Value::String(s) => {
                out.push_str(s);
                out.push(' ');
            }
            serde_json::Value::Number(n) => {
                out.push_str(&n.to_string());
                out.push(' ');
            }
            serde_json::Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
            serde_json::Value::Object(o) => o.values().for_each(|x| walk(x, out)),
            _ => {}
        }
    }
    let mut out = String::new();
    walk(payload, &mut out);
    out
}

impl Index {
    /// 打开/建库（index/index.sqlite；可丢弃——损坏即删除重建）。
    /// **仅供维护路径使用**（sidecar/reconcile 物化）；读路径必须用
    /// [`Self::open_readonly`]——r4 B2：读路径建库会把空索引当作真相应答。
    /// r11 优化：records_fts 虚表已移除——find 子串检索由两执行器共用的
    /// LIKE 语义承担（r7/r10 对齐后命中契约一致），FTS5 token 检索属
    /// 规模增长后的升级坡道（判断 3）。
    pub fn open(path: &Path) -> MemResult<Index> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| MemError::io(dir.display().to_string(), e))?;
        }
        let conn = Connection::open(path)
            .map_err(|e| MemError::Corrupt { path: path.display().to_string(), reason: e.to_string() })?;
        Self::init_schema(&conn)?;
        Ok(Index { conn })
    }

    /// r4 B2：**只读**打开已存在的索引——文件缺失/schema 不符 → Err（调用方直读）。
    /// 不建库、不建表、不切换 journal mode（读路径零写盘，红线18）。
    pub fn open_readonly(path: &Path) -> MemResult<Index> {
        if !path.is_file() {
            return Err(MemError::MissingRecord { id_or_path: path.display().to_string() });
        }
        let conn = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .map_err(|e| MemError::Corrupt { path: path.display().to_string(), reason: e.to_string() })?;
        // schema 存在性核对（只读查询；缺表/缺列 = 物化未完成/旧代索引 → Err 直读）。
        let ok: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN ('records','refs','meta')",
                [],
                |r| r.get(0),
            )
            .map_err(|e| MemError::Corrupt { path: path.display().to_string(), reason: e.to_string() })?;
        let cols: i64 = conn
            .query_row(
                "SELECT count(*) FROM pragma_table_info('records') WHERE name IN ('body_text','aliases_text','payload_folded','body_folded','aliases_folded')",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if ok < 3 || cols < 5 {
            return Err(MemError::Corrupt {
                path: path.display().to_string(),
                reason: "索引 schema 不完整或为旧代（物化未完成/异物文件）".into(),
            });
        }
        Ok(Index { conn })
    }

    fn init_schema(conn: &Connection) -> MemResult<()> {
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            CREATE TABLE IF NOT EXISTS records (
                id TEXT PRIMARY KEY,
                type TEXT NOT NULL,
                rel TEXT NOT NULL,
                version INTEGER NOT NULL,
                digest TEXT NOT NULL,
                modified TEXT NOT NULL,
                principal TEXT,
                location TEXT NOT NULL,
                payload_json TEXT NOT NULL,
                body_text TEXT NOT NULL DEFAULT '',
                aliases_text TEXT NOT NULL DEFAULT '',
                payload_folded TEXT NOT NULL DEFAULT '',
                body_folded TEXT NOT NULL DEFAULT '',
                aliases_folded TEXT NOT NULL DEFAULT ''
            );
            CREATE INDEX IF NOT EXISTS idx_records_type ON records(type, modified DESC);
            CREATE INDEX IF NOT EXISTS idx_records_rel ON records(rel);
            CREATE TABLE IF NOT EXISTS refs (
                from_id TEXT NOT NULL,
                rel TEXT NOT NULL,
                target TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_refs_target ON refs(target);
            CREATE INDEX IF NOT EXISTS idx_refs_from ON refs(from_id);
            CREATE TABLE IF NOT EXISTS meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            "#,
        )
        .map_err(|e| MemError::Corrupt { path: "index".into(), reason: e.to_string() })?;
        // r5/r7 列迁移（旧库幂等补列；新库 CREATE 已含，报错忽略）。
        let _ = conn.execute_batch(
            "ALTER TABLE records ADD COLUMN body_text TEXT NOT NULL DEFAULT '';
             ALTER TABLE records ADD COLUMN aliases_text TEXT NOT NULL DEFAULT '';
             ALTER TABLE records ADD COLUMN payload_folded TEXT NOT NULL DEFAULT '';
             ALTER TABLE records ADD COLUMN body_folded TEXT NOT NULL DEFAULT '';
             ALTER TABLE records ADD COLUMN aliases_folded TEXT NOT NULL DEFAULT '';",
        );
        Ok(())
    }

    /// 打开店索引（维护路径，index/index.sqlite）。
    pub fn open_store(store: &Store) -> MemResult<Index> {
        Self::open(&store.abs("index/index.sqlite"))
    }

    /// 打开店索引（读路径，只读；缺失/不完整 → Err → 调用方直读）。
    pub fn open_store_readonly(store: &Store) -> MemResult<Index> {
        Self::open_readonly(&store.abs("index/index.sqlite"))
    }

    /// 物化代际（r4 B2）：每次 rebuild 递增；检查点发布时记录该值，读路径核对
    /// 一致才认定"检查点与索引同代"——索引被删除重建（epoch 归 1）而检查点仍
    /// 记旧代 → 读路径直读，不会被空/旧索引误信。
    pub fn epoch(&self) -> MemResult<u64> {
        self.conn
            .query_row("SELECT value FROM meta WHERE key = 'epoch'", [], |r| {
                r.get::<_, String>(0)
            })
            .ok()
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| MemError::Corrupt {
                path: "index".into(),
                reason: "索引缺 epoch（物化未完成）".into(),
            })
    }

    /// 物化（写例程/reconcile/sidecar 维护调用；读路径永不调用——红线18）。
    ///
    /// 全量替换式物化（v1：中小规模，简单压倒增量；索引可丢弃，重建成本低）。
    /// 返回 (记录数, 边数)。
    pub fn rebuild(&mut self, store: &Store) -> MemResult<(usize, usize)> {
        let (n, r, _) = self.rebuild_with_entries(store)?;
        Ok((n, r))
    }

    /// 物化并返回**与索引同一次扫描**的文件 stat 集（r5 B3）。
    ///
    /// 检查点的 entries 必须采自这份集合——它正是索引物化时看到的文件状态。
    /// 若在发布时重新枚举磁盘，"最后 fresh 查询之后、快照之前"的写入会被检查点
    /// 提前确认（stat 一致但索引是旧内容——红线21 立法场景）。同代绑定后：
    /// 扫描之后的写入要么不在 entries（新文件 → 实时枚举失配 → 直读），
    /// 要么触发订阅事件（改既有文件 → fresh > 0 → 直读/再维护）。
    pub fn rebuild_with_entries(
        &mut self,
        store: &Store,
    ) -> MemResult<(
        usize,
        usize,
        std::collections::BTreeMap<String, (u128, u64)>,
    )> {
        let scan = ops::scan_all(store)?;
        let mut entries = std::collections::BTreeMap::new();
        let mut n_records = 0usize;
        let mut n_refs = 0usize;
        self.conn
            .execute("BEGIN", [])
            .map_err(|e| MemError::Corrupt { path: "index".into(), reason: e.to_string() })?;
        let _ = self.conn.execute("DELETE FROM records", []);
        let _ = self.conn.execute("DELETE FROM refs", []);
        for s in &scan.records {
            // r11：折叠列统一在物化时计算（逐值/逐别名独立 fold 后 LF 连接——
            // LF 分隔符保证 LIKE 子串匹配不跨值/跨别名）。sync_fts 不再覆盖。
            let payload_folded = payload_values_folded(&s.record.payload);
            let body_folded = mem_core::paths::fold(&String::from_utf8_lossy(&s.record.body));
            let aliases_folded = s
                .record
                .envelope
                .aliases
                .iter()
                .map(|a| mem_core::paths::fold(a))
                .collect::<Vec<_>>()
                .join("
");
            self.upsert_row(&IndexRow {
                id: s.record.envelope.id.clone(),
                type_name: s.record.envelope.r#type.clone(),
                rel: s.rel.clone(),
                version: s.record.envelope.version,
                digest: s.token.digest.clone(),
                modified: s.record.envelope.modified.clone(),
                principal: s.record.envelope.principal.clone(),
                location: "records".into(),
                payload_json: serde_json::to_string(&s.record.payload).unwrap_or_default(),
                payload_folded,
                body_folded,
                aliases_folded,
            })?;
            n_records += 1;
            // 同代 stat 集：与索引内容来自同一次扫描（r5 B3）。
            if let Ok(md) = std::fs::metadata(store.abs(&s.rel)) {
                let mtime = md
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos())
                    .unwrap_or(0);
                entries.insert(s.rel.clone(), (mtime, md.len()));
            }
            for e in &s.record.envelope.refs {
                self.insert_ref(&RefRow {
                    from_id: s.record.envelope.id.clone(),
                    rel: e.rel.clone(),
                    target: e.target.clone(),
                })?;
                n_refs += 1;
            }
        }
        self.conn
            .execute("COMMIT", [])
            .map_err(|e| MemError::Corrupt { path: "index".into(), reason: e.to_string() })?;
        // 物化代际递增（r4 B2：检查点据此核对与索引同代）。
        let prev = self
            .conn
            .query_row("SELECT value FROM meta WHERE key = 'epoch'", [], |r| {
                r.get::<_, String>(0)
            })
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        self.conn
            .execute(
                "INSERT INTO meta(key, value) VALUES ('epoch', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [format!("{}", prev + 1)],
            )
            .map_err(|e| MemError::Corrupt { path: "index".into(), reason: e.to_string() })?;
        self.conn
            .execute(
                "INSERT INTO meta(key, value) VALUES ('last_rebuild', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [mem_core::clock::now_rfc3339()],
            )
            .map_err(|e| MemError::Corrupt { path: "index".into(), reason: e.to_string() })?;
        let _ = scan.warnings;
        Ok((n_records, n_refs, entries))
    }

    fn upsert_row(&self, r: &IndexRow) -> MemResult<()> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO records(id, type, rel, version, digest, modified, principal, location, payload_json, payload_folded, body_folded, aliases_folded)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                rusqlite::params![
                    r.id, r.type_name, r.rel, r.version, r.digest, r.modified,
                    r.principal, r.location, r.payload_json,
                    r.payload_folded, r.body_folded, r.aliases_folded
                ],
            )
            .map_err(sq_err)?;
        Ok(())
    }

    /// r5 B7：物化时同步 FTS 文本素材列（正文 + 别名；sync_fts 组装 text 时读取）。
    pub fn set_body_texts(&mut self, store: &Store) -> MemResult<()> {
        // r7：fold 列预折叠（与 mem_core::paths::fold 语义一致——NFC + 小写），
        // 查询用 fold 模式精确 LIKE，非 ASCII 大小写折叠不依赖 SQLite LIKE。
        let scan = ops::scan_all(store)?;
        for s in &scan.records {
            let body = String::from_utf8_lossy(&s.record.body).to_string();
            let aliases = s.record.envelope.aliases.join(" ");
            // r8：payload_folded = **值的 fold**（不含键名、不含 id）——直读的
            // payload 匹配只搜值（payload_contains），键名查询两模式都必须不命中；
            // id 匹配是**前缀**语义，fold 列不得包含 id 全文（否则中段子串在
            // indexed 命中而 direct 不命中——命中面不等价）。
            // r10 B7-2：payload 逐值 fold 后以 LF 连接、别名逐个 fold 后以 LF 连接
            // ——LF 分隔符保证 LIKE 子串匹配不会跨值/跨别名产生假命中
            // （此前空格连接：值 "ab"+"cd" 拼出 "abcd"，查询 "bc" 误命中）。
            let payload_folded = payload_values_folded(&s.record.payload);
            let body_folded = mem_core::paths::fold(&body);
            let aliases_folded = s
                .record
                .envelope
                .aliases
                .iter()
                .map(|a| mem_core::paths::fold(a))
                .collect::<Vec<_>>()
                .join("\n");
            self.conn
                .execute(
                    "UPDATE records SET body_text = ?2, aliases_text = ?3, payload_folded = ?4, body_folded = ?5, aliases_folded = ?6 WHERE id = ?1",
                    rusqlite::params![
                        s.record.envelope.id, body, aliases,
                        payload_folded, body_folded, aliases_folded
                    ],
                )
                .map_err(sq_err)?;
        }
        Ok(())
    }

    fn insert_ref(&self, r: &RefRow) -> MemResult<()> {
        self.conn
            .execute(
                "INSERT INTO refs(from_id, rel, target) VALUES (?1, ?2, ?3)",
                rusqlite::params![r.from_id, r.rel, r.target],
            )
            .map_err(sq_err)?;
        Ok(())
    }

    /// 按类型过滤（modified 降序、LIMIT/OFFSET——快路径查询）。
    pub fn query_by_type(
        &self,
        type_name: &str,
        limit: usize,
        offset: usize,
    ) -> MemResult<Vec<IndexRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, type, rel, version, digest, modified, principal, location, payload_json, payload_folded, body_folded, aliases_folded
                 FROM records WHERE type = ?1
                 ORDER BY modified DESC, id DESC LIMIT ?2 OFFSET ?3",
            )
            .map_err(sq_err)?;
        let rows = stmt
            .query_map(rusqlite::params![type_name, limit as i64, offset as i64], row_to_index)
            .map_err(sq_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sq_err)?;
        Ok(rows)
    }

    /// 全类型查询（brief/list 无类型过滤的快路径；modified 降序）。
    pub fn query_all(&self, limit: usize, offset: usize) -> MemResult<Vec<IndexRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, type, rel, version, digest, modified, principal, location, payload_json, payload_folded, body_folded, aliases_folded
                 FROM records
                 ORDER BY modified DESC, id DESC LIMIT ?1 OFFSET ?2",
            )
            .map_err(sq_err)?;
        let rows = stmt
            .query_map(rusqlite::params![limit as i64, offset as i64], row_to_index)
            .map_err(sq_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sq_err)?;
        Ok(rows)
    }

    /// 多跳边查询（trace 快路径）：一跳邻居。
    pub fn neighbors(&self, id: &str, direction: &'static str) -> MemResult<Vec<RefRow>> {
        let (sql, key) = match direction {
            "up" => ("SELECT from_id, rel, target FROM refs WHERE from_id = ?1", id),
            _ => ("SELECT from_id, rel, target FROM refs WHERE target = ?1", id),
        };
        let mut stmt = self.conn.prepare(sql).map_err(sq_err)?;
        let rows = stmt
            .query_map([key], |r| {
                Ok(RefRow {
                    from_id: r.get(0)?,
                    rel: r.get(1)?,
                    target: r.get(2)?,
                })
            })
            .map_err(sq_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sq_err)?;
        Ok(rows)
    }

    /// 对账诊断（永不拒收——差异仅报告，红线8）：索引 vs 直读。
    pub fn diagnose_against(&self, scan: &[Scanned]) -> MemResult<Vec<String>> {
        let mut out = Vec::new();
        let mut stmt = self
            .conn
            .prepare("SELECT id, rel, version FROM records")
            .map_err(sq_err)?;
        let indexed: std::collections::BTreeMap<String, (String, i64)> = stmt
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, (r.get::<_, String>(1)?, r.get::<_, i64>(2)?)))
            })
            .map_err(sq_err)?
            .filter_map(|x| x.ok())
            .collect();
        let truth: std::collections::BTreeMap<String, (String, u64)> = scan
            .iter()
            .map(|s| (s.record.envelope.id.clone(), (s.rel.clone(), s.record.envelope.version)))
            .collect();
        for (id, (rel, ver)) in &truth {
            match indexed.get(id) {
                None => out.push(format!("索引缺记录 {id}（{rel}）")),
                Some((irel, iver)) => {
                    if irel != rel {
                        out.push(format!("索引路径漂移 {id}: {irel} ≠ {rel}"));
                    }
                    if *iver != *ver as i64 {
                        out.push(format!("索引版本落后 {id}: {iver} ≠ {ver}"));
                    }
                }
            }
        }
        for id in indexed.keys() {
            if !truth.contains_key(id) {
                out.push(format!("索引多余记录 {id}（真相层已不在——可丢弃缓存，仅诊断）"));
            }
        }
        Ok(out)
    }
}

fn row_to_index(r: &rusqlite::Row<'_>) -> rusqlite::Result<IndexRow> {
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
}

fn sq_err(e: rusqlite::Error) -> MemError {
    MemError::Corrupt { path: "index/index.sqlite".into(), reason: e.to_string() }
}
