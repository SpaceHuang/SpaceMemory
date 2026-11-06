//! 载荷校验与分档执行（R8.5 / R8.6）。
//!
//! - strict（判定型结构，如 task）：坏字段即坏记录——拒收并报告哪一条、为何不合规
//!   （R6.1），提供接口修复通道（R6.3 `mem fix`）；
//! - lenient（模糊型结构，如 memory）：容忍未知字段，降级服务并告警。
//!
//! 校验时机与 D3 写路径第 4 步对齐：在记录锁内、落盘前执行。
//! x-ref 存在性检查在本模块（调用方提供存在性回调）；DAG 检查在 mem-core（需要图）。

use std::collections::BTreeMap;

use crate::decl::{Grade, RefSpec};
use crate::Violation;

/// 分档裁决结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// strict + 有违例 → 拒收（结构化错误）。
    Reject,
    /// lenient + 有违例 → 降级服务 + 告警（不拒收）。
    Warn,
    /// 无违例。
    Accept,
}

pub fn policy_for(grade: Grade, violations: &[Violation]) -> Policy {
    if violations.is_empty() {
        Policy::Accept
    } else if grade == Grade::Strict {
        Policy::Reject
    } else {
        Policy::Warn
    }
}

/// x-ref 存在性检查：字段值非 null 且声明 existence=true 时，必须是
/// `exists(target_type, id) == true` 的目标记录。
pub fn check_ref_existence(
    refs: &BTreeMap<String, RefSpec>,
    payload: &serde_json::Value,
    exists: impl Fn(&str, &str) -> bool,
) -> Vec<Violation> {
    let mut out = Vec::new();
    let Some(obj) = payload.as_object() else {
        return out;
    };
    for (field, spec) in refs {
        if !spec.existence {
            continue;
        }
        match obj.get(field) {
            None | Some(serde_json::Value::Null) => continue,
            Some(serde_json::Value::String(id)) => {
                if !exists(&spec.target_type, id) {
                    out.push(Violation {
                        path: format!("/{field}"),
                        message: format!("引用目标不存在：{field} → {} ({id})", spec.target_type),
                    });
                }
            }
            Some(other) => out.push(Violation {
                path: format!("/{field}"),
                message: format!("引用字段必须是 uuid 字符串或 null，实际 {other}"),
            }),
        }
    }
    out
}
