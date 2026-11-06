//! find 命中谓词单源（r12 B7-1）。
//!
//! B7 反复出问题的根因是"两套匹配实现必然漂移"（r7–r11 逐轮复现：LF 假命中、
//! NFC/NFD 不一致、大小写 ID 前缀……）。本模块把命中语义收敛为**一个函数**：
//!
//! - direct 执行器直接调用 [`record_matches`] 裁定命中；
//! - indexed 执行器用折叠列 LIKE 做保守**粗筛**（超集：跨值假命中可能、永不漏——
//!   值内命中 ⊆ 拼接命中；LF 分隔符使查询含 LF 时只多报不漏报），再对粗筛候选
//!   调用同一函数**精筛**后分页。
//!
//! 两执行器的命中面从此结构性等价——拼接折叠列不再定义语义，只提供过滤。

use crate::paths::fold;
use serde_json::Value;

/// payload 值匹配：逐值独立 fold contains（键名不参与——键名查询两模式都必须不
/// 命中，r8 裁决）。
pub fn payload_contains(payload: &Value, q_fold: &str) -> bool {
    fn walk(v: &Value, q_fold: &str) -> bool {
        match v {
            Value::String(s) => fold(s).contains(q_fold),
            Value::Array(a) => a.iter().any(|x| walk(x, q_fold)),
            Value::Object(o) => o.values().any(|x| walk(x, q_fold)),
            _ => false,
        }
    }
    walk(payload, q_fold)
}

/// 记录命中谓词（direct 执行器与 indexed 精筛共用，结构性等价）：
/// id 前缀 ∪ 逐别名 ∪ 逐 payload 值 ∪ 正文——全部 fold（NFC+小写）子串语义。
/// `q_fold` 必须是 `fold(query)`（调用方折一次，逐记录循环内不重复折叠）。
pub fn record_matches(
    id: &str,
    aliases: &[String],
    payload: &Value,
    body: &[u8],
    q_fold: &str,
) -> bool {
    if fold(id).starts_with(q_fold) {
        return true;
    }
    if aliases.iter().any(|a| fold(a).contains(q_fold)) {
        return true;
    }
    if payload_contains(payload, q_fold) {
        return true;
    }
    fold(&String::from_utf8_lossy(body)).contains(q_fold)
}
