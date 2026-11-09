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
pub enum MemError {
    #[error("IO 错误（{path}）：{source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("frontmatter 不是合法 JSON（{path}）：{reason}——解析失败必须响亮（D18），修复通道 mem fix（R6.3）")]
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

    #[error("payload 不合类型声明（{type_name}，strict）：{violations:?}——修复通道 mem fix（R6.3）")]
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

    #[error("git 提交失败（可重试资源，事件已生效）：{0}")]
    Git(String),

    #[error("受管写入被拒（{path}）：.gitattributes 内容与默认不符——建立并核验后才允许受管写入（BD5 ③）；恢复默认内容 `* -text` 即解锁，读路径不受影响")]
    GitAttributesConflict { path: String },

    #[error("计划/文件损坏（{path}）：{reason}")]
    PlanCorrupt { path: String, reason: String },

    #[error("参数非法：{0}")]
    BadArgument(String),
}

pub type MemResult<T> = Result<T, MemError>;

impl MemError {
    pub fn io(path: impl Into<String>, e: std::io::Error) -> Self {
        MemError::Io { path: path.into(), source: e }
    }
}
