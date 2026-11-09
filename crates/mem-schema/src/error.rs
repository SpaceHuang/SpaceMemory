//! 类型声明加载错误（响亮失败，R6.1）。

use thiserror::Error;

#[derive(Debug, Error)]
pub enum LoadError {
    #[error("schema.json 不可读：{0}")]
    Io(#[from] std::io::Error),
    #[error("schema.json 不是合法 JSON：{0}")]
    Json(#[from] serde_json::Error),
    #[error("schema 编译失败：{0}")]
    SchemaCompile(String),
    #[error("schema.json 顶层必须是 JSON object")]
    NotObject,
    #[error("$schema 必须是 draft 2020-12，实际为 {0:?}")]
    WrongDraft(String),
    #[error("x-grade 取值非法：{0:?}（只允许 \"strict\"/\"lenient\"）")]
    BadGrade(String),
    #[error("x-unique-key 必须是字符串数组")]
    BadUniqueKey,
    #[error("x-ref 必须是 object：{{\"target_type\": …, \"existence\": …}}，字段 {0}")]
    BadRefSpec(String),
    #[error("x-unique-key/x-topics 引用的字段 {0:?} 未在 properties 中定义")]
    UnknownKeyedField(String),
    #[error("x-ref 字段 {0} 未在 properties 中定义")]
    UnknownRefField(String),
    #[error("x-filter 字段 {0} 未在 properties 中定义")]
    UnknownFilterField(String),
}
