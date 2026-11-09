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
///
/// R11.4 正文域参与门槛（G1 单源，需求决策④(b)）：正文区非合法 UTF-8 → 正文域
/// 不参与（恒不命中），**绝不经过有损解码**——binary-content-design §3.4 废除
/// "有损解码后匹配"（编码巧合入命中 = 伪命中，基线 R10.2 注记由 R11 落地收口）。
/// 其余三域（身份前缀/别名/载荷值）照常。
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
    match crate::body::text_or_none(body) {
        Some(s) => fold(s).contains(q_fold),
        // 非文本正文：正文域不参与（G1 门槛）。
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const BIN: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, b'a', b'b', b's', 0x00, 0xFF];

    #[test]
    fn r11_body_gate_non_utf8_never_matches() {
        // (i) 正文区含 "abs" 编巧合、其余域不含 → 不命中（伪命中收口）。
        assert!(
            !record_matches("0198-id", &[], &json!({"slug":"img1"}), BIN, &fold("abs")),
            "非文本正文绝不经有损解码参与匹配"
        );
    }

    #[test]
    fn r11_body_gate_valid_utf8_body_still_matches() {
        // (iii) 合法 UTF-8 正文（SVG 文本）照常命中。
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg">abstract-geometry</svg>"#;
        assert!(record_matches("0198-id", &[], &json!({}), svg, &fold("abstract")));
    }

    #[test]
    fn r11_other_domains_still_hit_on_binary_record() {
        // (ii) 非文本记录的 id 前缀 / 别名 / 载荷值照常参与。
        let bin = BIN;
        assert!(record_matches("img-abs-01", &[], &json!({}), bin, &fold("img")));
        assert!(record_matches("0198-id", &["abs-alias".into()], &json!({}), bin, &fold("abs-ali")));
        assert!(
            record_matches("0198-id", &[], &json!({"filename":"abs.png"}), bin, &fold("abs.png")),
            "锁定『仅正文区排除』边界"
        );
    }

    #[test]
    fn legacy_body_matching_unchanged_for_utf8() {
        // 既有语义回归：合法 UTF-8 正文子串匹配不受门槛影响。
        assert!(record_matches("id", &[], &json!({}), b"hello world", &fold("world")));
        assert!(!record_matches("id", &[], &json!({}), b"hello world", &fold("xyz")));
    }
}
