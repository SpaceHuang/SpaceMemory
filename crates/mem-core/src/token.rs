//! 修订令牌与 CAS。
//!
//! 修订令牌 = `v<版本>@<完整 SHA-256 十六进制>`，覆盖整个文件字节；机制在读响应
//! 附带，不存储。一切读改写在锁内比较版本**与**摘要，不一致 → 结构化冲突（含当前
//! 令牌），不得落盘（D3 / 红线19）。全长度摘要在密码学意义上支撑"必被检出"——
//! 保版本号的人手修改必被检出。
//!
//! 操作版本语义：create 无令牌（唯一键路径独占即判定）；update/link 强制令牌；
//! merge/apply 独占门闩内逐记录令牌 CAS。

use std::fmt;
use std::str::FromStr;

use sha2::{Digest, Sha256};

use crate::error::{MemError, MemResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub version: u64,
    /// 64 个小写十六进制字符（全 SHA-256；红线19 禁截断摘要）。
    pub digest: String,
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v{}@{}", self.version, self.digest)
    }
}

impl FromStr for Token {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let rest = s
            .strip_prefix('v')
            .ok_or_else(|| format!("令牌必须形如 v<版本>@<64hex>：{s}"))?;
        let (ver, digest) = rest
            .split_once('@')
            .ok_or_else(|| format!("令牌必须形如 v<版本>@<64hex>：{s}"))?;
        let version: u64 = ver
            .parse()
            .map_err(|_| format!("令牌版本非法：{s}"))?;
        if version == 0 {
            return Err(format!("令牌版本必须 ≥ 1：{s}"));
        }
        if digest.len() != 64 || !digest.chars().all(|c| c.is_ascii_hexdigit()) {
            // 禁截断摘要（红线19）：64 字符全摘要才在密码学意义上支撑"必被检出"。
            return Err(format!("令牌摘要必须为 64 位十六进制（全 SHA-256）：{s}"));
        }
        Ok(Token { version, digest: digest.to_ascii_lowercase() })
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    let out = h.finalize();
    out.iter().map(|b| format!("{b:02x}")).collect()
}

/// 文件字节的修订令牌。
pub fn for_bytes(version: u64, bytes: &[u8]) -> Token {
    Token { version, digest: sha256_hex(bytes) }
}

/// 锁内 CAS 比较：版本**与**全摘要都必须一致（红线19）。
pub fn cas_check(expected: &Token, current: &Token) -> MemResult<()> {
    if expected.version == current.version && expected.digest == current.digest {
        Ok(())
    } else {
        Err(MemError::Conflict {
            expected: expected.clone(),
            current: current.clone(),
        })
    }
}
