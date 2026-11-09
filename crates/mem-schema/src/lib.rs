//! 类型声明两件套的加载与校验（D19 / R8.5–R8.8）。
//!
//! 每种内容类型 = `types/<t>/schema.json`（JSON Schema draft 2020-12 + 自定义关键字）
//! + `conventions.md`（约定教学），两件套同源交付（R8.8）：
//! - schema.json 承载机器可判定规则（R7-a）：字段类型、必填性、取值域、引用目标
//!   存在性——由本校验器机械执行，不依赖写入者自律（R8.5）；
//! - conventions.md 承载判断型行为（R7-b），与机制解耦、独立演化。
//!
//! 词汇表属基础设施（机制认识这些关键字并机械执行）；**类型本身仍是纯数据**——新增类型 =
//! 新增两件套，基础设施零改动（R8.1/红线1）：
//!
//! | 关键字 | 语义 |
//! |---|---|
//! | `x-grade` | 校验分档：strict（坏字段拒收 + fix 通道）/ lenient（容忍未知字段 + 告警，R8.6） |
//! | `x-unique-key` | 自然键 → 规范路径原子独占（D11/R3.3） |
//! | `x-ref` | 引用字段：目标类型与存在性校验 |
//! | `x-dag` | 派生类引用强制无环（D16/红线17） |
//! | `x-topics` | 必挂主题面（R7 (a)/(b) 边界） |
//! | `x-filter` | 载荷字段可过滤声明（R10 决策③；词汇表 v2） |
//!
//! 演化容忍（R8.7）：类型声明变更后，既有记录仍可读——降级、标注、不拒绝服务；
//! 迁移是显式例程，不做隐式自动改写。

pub mod decl;
pub mod error;
pub mod evolve;
pub mod keywords;
pub mod validate;

pub use decl::{FilterShape, Grade, RefSpec, TypeDecl, TypeSet};
pub use error::LoadError;
pub use validate::Policy;

/// 载荷校验违例（R6.1：明确报告哪一条、为何不合规）。
#[derive(Debug, Clone, PartialEq)]
pub struct Violation {
    /// 违例位置（JSON pointer 风格，如 `/status`）。
    pub path: String,
    /// 人读描述。
    pub message: String,
}
