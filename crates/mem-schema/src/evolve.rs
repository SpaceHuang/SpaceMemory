//! 演化容忍与迁移（R8.7）。
//!
//! 类型声明变更后，既有记录仍可读——降级、标注、不拒绝服务；历史数据的迁移是
//! 显式例程（R5.5：有预算、经确认、可审计），不做隐式自动改写。

use crate::Violation;

/// 单条记录对（新）声明的降级判定结果。
#[derive(Debug, Clone, PartialEq)]
pub struct Degraded {
    /// 记录不满足的新声明要求（违例列表的快照，供读取路径标注）。
    pub violations: Vec<Violation>,
}

/// 旧载荷对新声明校验：违例即降级标注（读取路径附告警），不是错误。
pub fn check_degraded(
    validator: &dyn Fn(&serde_json::Value) -> Vec<Violation>,
    payload: &serde_json::Value,
) -> Option<Degraded> {
    let violations = validator(payload);
    if violations.is_empty() {
        None
    } else {
        Some(Degraded { violations })
    }
}
