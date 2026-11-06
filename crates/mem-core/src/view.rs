//! 视图：角色的信息可见性是声明式数据（D10）。
//!
//! 视图 = 声明式查询模板（类型 × 引用 × principal × 时间），引用一律填 uuid v7；
//! **过滤只依赖信封**（R8.4）——x-ref 字段由写路径镜像进 envelope.refs（ops.rs 镜像
//! 纪律）。隔离不靠物理分区（R8.11）。执行 = 直读扫描 + 过滤 + 排序 + 预算分页
//! （读路径零写盘，红线18）。

use crate::envelope::Envelope;
use crate::error::{MemError, MemResult};
use crate::ops::{self, Scanned};
use crate::store::Store;
use crate::token::Token;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ViewFilter {
    /// 限定类型标识（空 = 不限）。
    pub types: Vec<String>,
    /// 要求记录携带的信封引用（rel, target）。
    pub refs: Vec<(String, String)>,
    /// 限定归属 principal（空 = 不限）。
    pub principals: Vec<String>,
    /// 只看此后 modified 的记录（RFC 3339 字典序比较）。
    pub since: Option<String>,
}

impl ViewFilter {
    /// 从 view 记录的 payload.filter 解析（声明式数据，机制零类型知识——filter
    /// 形状属于 view 类型声明，本函数按 D10 的通用模板形状机械读取）。
    pub fn from_payload(payload: &serde_json::Value) -> MemResult<ViewFilter> {
        let f = payload.get("filter").ok_or_else(|| {
            MemError::BadArgument("view payload 缺 filter（D10 查询模板）".into())
        })?;
        let types = f
            .get("types")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default();
        let refs = f
            .get("refs")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| {
                        let rel = x.get("rel")?.as_str()?.to_string();
                        let target = x.get("target")?.as_str()?.to_string();
                        Some((rel, target))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let principals = f
            .get("principals")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default();
        let since = f.get("since").and_then(|v| v.as_str()).map(String::from);
        Ok(ViewFilter { types, refs, principals, since })
    }

    pub fn matches(&self, env: &Envelope) -> bool {
        if !self.types.is_empty() && !self.types.contains(&env.r#type) {
            return false;
        }
        if !self.principals.is_empty() {
            match &env.principal {
                None => return false,
                Some(p) if !self.principals.contains(p) => return false,
                _ => {}
            }
        }
        for (rel, target) in &self.refs {
            if !env.refs.iter().any(|e| &e.rel == rel && &e.target == target) {
                return false;
            }
        }
        if let Some(since) = &self.since {
            if env.modified.as_str() < since.as_str() {
                return false;
            }
        }
        true
    }
}

#[derive(Debug, Clone)]
pub struct ListItem {
    pub rel: String,
    pub envelope: Envelope,
    pub token: Token,
}

#[derive(Debug)]
pub struct ViewResult {
    pub items: Vec<ListItem>,
    pub truncated: bool,
    /// 续读路径（红线9）：`execute_from` 的 offset。
    pub resume_hint: Option<ResumeHint>,
}

#[derive(Debug, Clone)]
pub struct ResumeHint {
    pub offset: usize,
}

/// 执行视图（分页第一页）。
pub fn execute(store: &Store, view_id: &str, limit: usize) -> MemResult<ViewResult> {
    execute_from(store, view_id, limit, 0)
}

/// 执行视图（自 offset 续读）。
pub fn execute_from(store: &Store, view_id: &str, limit: usize, offset: usize) -> MemResult<ViewResult> {
    let v = ops::get(store, view_id)?;
    if v.record.envelope.r#type != "view" {
        return Err(MemError::BadArgument(format!("{view_id} 不是 view 记录")));
    }
    let filter = ViewFilter::from_payload(&v.record.payload)?;
    execute_filter(store, &filter, limit, offset)
}

/// 按过滤器执行（brief/list 的底层入口）。
pub fn execute_filter(store: &Store, filter: &ViewFilter, limit: usize, offset: usize) -> MemResult<ViewResult> {
    let scan = ops::scan_all(store)?;
    let mut matched: Vec<Scanned> = scan
        .records
        .into_iter()
        .filter(|s| filter.matches(&s.record.envelope))
        .collect();
    // 排序：modified 降序、id 兜底（稳定分页序）。
    matched.sort_by(|a, b| {
        b.record
            .envelope
            .modified
            .cmp(&a.record.envelope.modified)
            .then(b.record.envelope.id.cmp(&a.record.envelope.id))
    });
    let total = matched.len();
    let page: Vec<ListItem> = matched
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|s| ListItem { rel: s.rel, envelope: s.record.envelope, token: s.token })
        .collect();
    let remaining = total.saturating_sub(offset + page.len());
    Ok(ViewResult {
        truncated: remaining > 0,
        resume_hint: if remaining > 0 { Some(ResumeHint { offset: offset + page.len() }) } else { None },
        items: page,
    })
}
