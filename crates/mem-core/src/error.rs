//! 统一错误与结构化结果。
//!
//! 原则：
//! - 冲突是一等结果（D3）：令牌 CAS 失败返回结构化冲突（含当前令牌），不是异常；
//! - 不合规写入可识别：明确报告是哪一条、为何不合规（R6.1）；strict 拒收并提供
//!   接口修复通道，lenient 降级服务并告警（R8.6）；
//! - 可隔离：局部坏数据不得使整体不可用——读路径跳过坏条目、附告警、继续服务（R6.2）；
//! - 解析失败是响亮错误：frontmatter JSON 解析失败绝不静默（D18）。

use mem_schema::Violation;

use crate::token::Token;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MemError {
    #[error("IO 错误（{path}）：{source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error(
        "frontmatter 不是合法 JSON（{path}）：{reason}——解析失败必须响亮（D18），修复通道 mem fix（R6.3）"
    )]
    FrontmatterJson { path: String, reason: String },

    #[error("frontmatter 结构非法（{path}）：{reason}")]
    FrontmatterShape { path: String, reason: String },

    #[error("信封字段非法（{path}）：{reason}")]
    EnvelopeInvalid { path: String, reason: String },

    #[error("记录损坏（{path}）：{reason}")]
    Corrupt { path: String, reason: String },

    /// 令牌 CAS 冲突（D3/红线19）：含当前令牌，调用方据此重读重放。
    #[error("conflict: 令牌不一致——expect {expected}，当前 {current}；先读当前成果再重放你的变更")]
    Conflict { expected: Token, current: Token },

    #[error("记录不存在（{id_or_path}）")]
    MissingRecord { id_or_path: String },

    #[error("唯一键路径已被占用（{path}）：既有记录 {existing_id}")]
    CreateConflict { path: String, existing_id: String },

    #[error("唯一键字段不得修改（{fields:?}）：移动语义 v1 拒收，转维护例程")]
    UniqueKeyImmut { fields: Vec<String> },

    #[error(
        "payload 不合类型声明（{type_name}，strict）：{violations:?}——修复通道 mem fix（R6.3）"
    )]
    SchemaReject {
        type_name: String,
        violations: Vec<Violation>,
    },

    #[error("未声明的类型 {0}（types/ 缺两件套或加载失败）")]
    UnknownType(String),

    #[error("引用目标不存在：{rel} → {target}")]
    RefTargetMissing { rel: String, target: String },

    #[error("派生关系构成环（x-dag 类型强制无环，D16/红线17）：{0}")]
    CycleDetected(String),

    #[error("锁等待超时（{lock_path}）：持有者 {holder:?}——超时只许等待方放弃（红线22）")]
    LockTimeout {
        lock_path: String,
        holder: Option<String>,
    },

    #[error("存储只读：写类操作拒绝（只读档五读命令全部可用，§3.4）")]
    ReadOnly,

    #[error("不支持的数据格式 {found}；当前支持 {supported:?}")]
    UnsupportedFormat {
        found: String,
        supported: Vec<String>,
    },

    #[error("数据目录须先迁移：当前格式 {current}，目标格式 {target}")]
    RequiresMigration { current: String, target: String },

    #[error("迁移状态不可安全处理：{0}")]
    MigrationState(String),

    #[error("功能特性标识无效：{0}")]
    InvalidFeature(String),

    #[error("受管写入缺少必需机制特性：{0:?}")]
    MissingFeatures(Vec<String>),

    #[error("git 提交失败（可重试资源，事件已生效）：{0}")]
    Git(String),

    #[error(
        "受管写入被拒（{path}）：.gitattributes 内容与默认不符——建立并核验后才允许受管写入（BD5 ③）；恢复默认内容 `* -text` 即解锁，读路径不受影响"
    )]
    GitAttributesConflict { path: String },

    #[error("计划/文件损坏（{path}）：{reason}")]
    PlanCorrupt { path: String, reason: String },

    #[error("参数非法：{0}")]
    BadArgument(String),

    /// 检索/过滤参数的显式拒收（R10 §2.7：4 枚机判 kind，两前端同码——CLI JSON
    /// 错误体与 MCP JSON-RPC error 的 data.kind 携带同一 kind 字符串）。
    #[error("{message}")]
    ParamRejected { kind: String, message: String },
}

pub type MemResult<T> = Result<T, MemError>;

impl MemError {
    pub fn io(path: impl Into<String>, e: std::io::Error) -> Self {
        MemError::Io {
            path: path.into(),
            source: e,
        }
    }

    /// 机判错误 kind（R10 §2.7）：仅 ParamRejected 携带；两前端据此同码透出。
    pub fn param_kind(&self) -> Option<&str> {
        match self {
            MemError::ParamRejected { kind, .. } => Some(kind),
            _ => None,
        }
    }

    /// 稳定的跨前端错误码与已有结构化字段。人读消息仍由 `Display` 提供。
    pub fn machine_info(&self) -> serde_json::Value {
        use serde_json::json;
        let (code, details) = match self {
            MemError::Io { path, .. } => ("io_error", json!({"path": path})),
            MemError::FrontmatterJson { path, reason } => {
                ("frontmatter_json", json!({"path": path, "reason": reason}))
            }
            MemError::FrontmatterShape { path, reason } => {
                ("frontmatter_shape", json!({"path": path, "reason": reason}))
            }
            MemError::EnvelopeInvalid { path, reason } => {
                ("envelope_invalid", json!({"path": path, "reason": reason}))
            }
            MemError::Corrupt { path, reason } => {
                ("corrupt", json!({"path": path, "reason": reason}))
            }
            MemError::Conflict { expected, current } => (
                "conflict",
                json!({"expected": {"version": expected.version, "digest": expected.digest}, "current": {"version": current.version, "digest": current.digest}}),
            ),
            MemError::MissingRecord { id_or_path } => {
                ("missing_record", json!({"id_or_path": id_or_path}))
            }
            MemError::CreateConflict { path, existing_id } => (
                "create_conflict",
                json!({"path": path, "existing_id": existing_id}),
            ),
            MemError::UniqueKeyImmut { fields } => {
                ("unique_key_immutable", json!({"fields": fields}))
            }
            MemError::SchemaReject {
                type_name,
                violations,
            } => (
                "schema_reject",
                json!({"type_name": type_name, "violations": violations.iter().map(|v| json!({"path": v.path, "message": v.message})).collect::<Vec<_>>()}),
            ),
            MemError::UnknownType(name) => ("unknown_type", json!({"type_name": name})),
            MemError::RefTargetMissing { rel, target } => {
                ("ref_target_missing", json!({"rel": rel, "target": target}))
            }
            MemError::CycleDetected(detail) => ("cycle_detected", json!({"detail": detail})),
            MemError::LockTimeout { lock_path, holder } => (
                "lock_timeout",
                json!({"lock_path": lock_path, "holder": holder}),
            ),
            MemError::ReadOnly => ("read_only", json!({})),
            MemError::UnsupportedFormat { found, supported } => (
                "unsupported_format",
                json!({"found": found, "supported": supported}),
            ),
            MemError::RequiresMigration { current, target } => (
                "requires_migration",
                json!({"current": current, "target": target}),
            ),
            MemError::MigrationState(reason) => ("migration_state", json!({"reason": reason})),
            MemError::InvalidFeature(feature) => ("invalid_feature", json!({"feature": feature})),
            MemError::MissingFeatures(features) => {
                ("missing_features", json!({"features": features}))
            }
            MemError::Git(detail) => ("git_error", json!({"detail": detail})),
            MemError::GitAttributesConflict { path } => {
                ("git_attributes_conflict", json!({"path": path}))
            }
            MemError::PlanCorrupt { path, reason } => {
                ("plan_corrupt", json!({"path": path, "reason": reason}))
            }
            MemError::BadArgument(detail) => ("bad_argument", json!({"detail": detail})),
            MemError::ParamRejected { kind, .. } => ("param_rejected", json!({"kind": kind})),
        };
        json!({"code": code, "details": details})
    }
}
