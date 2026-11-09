//! 自定义关键字词汇表：x-grade / x-unique-key / x-ref / x-dag / x-topics /
//! x-filter（D19；x-filter 为 R10 决策③新增，词汇表 v2）。
//!
//! 词汇表是基础设施的一部分（机制认识这些关键字并机械执行）；关键字集合的变更
//! 属于基础设施演化，与"类型是纯数据"（R8.2）不冲突——载荷字段的含义仍完全由
//! 类型声明提供。
//!
//! 实现形态：`x-*` 对 draft 校验器是普通注记（JSON Schema 允许未知关键字）；其
//! 强制语义由 [`crate::decl::TypeDecl`] 解析为类型元数据（grade/unique_key/refs/
//! dag/topics），分别在 mem-schema（分档、存在性）与 mem-core（路径独占、DAG、
//! 路径推导）执行。本模块固定**保留前缀**与词汇表常量，供文档与防御性检查使用。

/// 机制保留的 x- 关键字（与声明作者的普通 x- 注记区分：同名键将被机制解释）。
pub const RESERVED_KEYWORDS: &[&str] = &[
    "x-grade",
    "x-unique-key",
    "x-ref",
    "x-dag",
    "x-topics",
    "x-derived-rel",
    "x-filter",
];

/// 词汇表版本（随 mem-schema 演化；类型作者据此判断可用关键字面）。
/// v2 = v1 + x-filter（R10 决策③，keyword-search-design.md §2.4）。
pub const KEYWORD_VOCAB_VERSION: u32 = 2;

/// 防御性检查：根层出现 x-ref 属于错位（x-ref 只在 payload 属性上）——响亮报告。
pub fn misplaced_reserved_keywords(schema: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(obj) = schema.as_object() {
        if obj.contains_key("x-ref") {
            out.push("x-ref".to_string());
        }
    }
    out
}
