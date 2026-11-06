//! 记录文件：一条记录 = 一个 `.md` 文件。
//!
//! 第一行恰为 `---`，至下一个 `---` 为 frontmatter（必须是合法 JSON，UTF-8；
//! envelope/payload 两块使所有权字面可见，R2 审计），其余为 Markdown 正文 = 载荷自由文本（D18）。
//!
//! 机制再生成纪律（红线27）：机制写记录时只重生成 frontmatter（确定性序列化），
//! **正文按字节保持不变**——正文是人/Agent 的字节领土；正文字节变化必然改变修订
//! 令牌摘要，由 CAS 收敛。
//!
//! fix 流（R6.3）：frontmatter 解析失败 → 从 git 历史取上一版 frontmatter + 保留当前
//! 正文，走 `mem fix` 交互重建。

use crate::envelope::Envelope;
use crate::error::{MemError, MemResult};
use crate::token;

#[derive(Debug, Clone)]
pub struct Record {
    pub envelope: Envelope,
    pub payload: serde_json::Value,
    /// 正文 = 载荷自由文本，字节领土（红线27）：机制永不改动其内容。
    pub body: Vec<u8>,
}

/// 机制再生成整文件字节：确定性 frontmatter + 原样正文字节。
pub fn serialize_new(envelope: &Envelope, payload: &serde_json::Value, body: &[u8]) -> Vec<u8> {
    let mut fm = serde_json::Map::new();
    fm.insert("envelope".into(), envelope.to_json_value());
    fm.insert("payload".into(), payload.clone());
    // serde_json Map 默认 BTreeMap：键递归字母序 → envelope 在前（envelope < payload）。
    let json = serde_json::to_string_pretty(&serde_json::Value::Object(fm))
        .expect("信封+载荷序列化不会失败");
    let mut out = Vec::with_capacity(json.len() + body.len() + 8);
    out.extend_from_slice(b"---\n");
    out.extend_from_slice(json.as_bytes());
    out.extend_from_slice(b"\n---\n");
    out.extend_from_slice(body);
    out
}

impl Record {
    /// 解析记录文件（响亮失败：BOM、缺分隔符、坏 JSON、缺 envelope 块都是错误，D18）。
    pub fn from_bytes(bytes: &[u8]) -> MemResult<Record> {
        let (frontmatter, body) = split_frontmatter(bytes)?;
        let env_v = frontmatter
            .get("envelope")
            .ok_or_else(|| MemError::FrontmatterShape {
                path: "(record)".into(),
                reason: "缺 envelope 块".into(),
            })?;
        let payload =
            frontmatter
                .get("payload")
                .cloned()
                .ok_or_else(|| MemError::FrontmatterShape {
                    path: "(record)".into(),
                    reason: "缺 payload 块".into(),
                })?;
        let envelope = Envelope::from_json(env_v).map_err(|reason| {
            MemError::EnvelopeInvalid { path: "(record)".into(), reason }
        })?;
        Ok(Record { envelope, payload, body: body.to_vec() })
    }

    /// 机制再生成字节（frontmatter 重写 + 正文按字节保持）。
    pub fn regen_bytes(&self, new_envelope: &Envelope) -> Vec<u8> {
        serialize_new(new_envelope, &self.payload, &self.body)
    }

    /// 原文件字节的修订令牌（摘要覆盖磁盘字节，不是再生成字节）。
    pub fn token_of_bytes(bytes: &[u8], version: u64) -> token::Token {
        token::for_bytes(version, bytes)
    }
}

/// 从（可能损坏的）记录字节中提取正文：**首个**闭合 `---` 行之后的全部字节。
///
/// 与 [`split_frontmatter`] 共用同一边界定义（D18：第一行恰为 `---`，至**下一个**
/// `---` 行为 frontmatter）——供 fix 流在 frontmatter JSON 损坏时提取正文字节
/// （红线27 字节领土），不要求 frontmatter 可解析。无闭合分隔符 → Err。
pub fn find_body_after_first_delimiter(bytes: &[u8]) -> MemResult<Vec<u8>> {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return Err(MemError::FrontmatterShape {
            path: "(record)".into(),
            reason: "文件带 UTF-8 BOM——规范禁止（响亮拒绝，R6.1）".into(),
        });
    }
    let first_line_end =
        find_line_end(bytes, 0).ok_or_else(|| MemError::FrontmatterShape {
            path: "(record)".into(),
            reason: "空文件或无首行".into(),
        })?;
    if trim_cr(&bytes[..first_line_end]) != b"---" {
        return Err(MemError::FrontmatterShape {
            path: "(record)".into(),
            reason: "首行必须是 ---（D18）".into(),
        });
    }
    let mut pos = first_line_end + 1;
    while pos < bytes.len() {
        match find_line_end(bytes, pos) {
            Some(e) => {
                if trim_cr(&bytes[pos..e]) == b"---" {
                    let body_start = e + 1; // 闭合行换行之后 = 正文起点
                    return Ok(bytes[body_start.min(bytes.len())..].to_vec());
                }
                pos = e + 1;
            }
            None => break,
        }
    }
    Err(MemError::FrontmatterShape {
        path: "(record)".into(),
        reason: "frontmatter 无闭合 --- 分隔符（正文提取，B2）".into(),
    })
}

/// 拆分 frontmatter 与正文：返回 (frontmatter JSON, 正文字节)。
pub fn split_frontmatter(bytes: &[u8]) -> MemResult<(serde_json::Value, &[u8])> {
    const PATH: &str = "(record)";
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return Err(MemError::FrontmatterShape {
            path: PATH.into(),
            reason: "文件带 UTF-8 BOM——规范禁止（响亮拒绝，R6.1）".into(),
        });
    }
    let first_line_end = find_line_end(bytes, 0).ok_or_else(|| MemError::FrontmatterShape {
        path: PATH.into(),
        reason: "空文件或无首行".into(),
    })?;
    let first = trim_cr(&bytes[..first_line_end]);
    if first != b"---" {
        return Err(MemError::FrontmatterShape {
            path: PATH.into(),
            reason: "首行必须是 ---（D18）".into(),
        });
    }
    // 找闭合 `---` 行。
    let fm_start = first_line_end + 1;
    let mut pos = fm_start;
    let mut close = None; // (行起点, 行尾=换行符位置)
    while pos < bytes.len() {
        match find_line_end(bytes, pos) {
            Some(e) => {
                if trim_cr(&bytes[pos..e]) == b"---" {
                    close = Some((pos, e));
                    break;
                }
                pos = e + 1;
            }
            None => break,
        }
    }
    let (fm_close_line_start, fm_close_line_end) =
        close.ok_or_else(|| MemError::FrontmatterShape {
            path: PATH.into(),
            reason: "frontmatter 无闭合 --- 分隔符".into(),
        })?;
    let fm_json = &bytes[fm_start..fm_close_line_start];
    // 正文从闭合行的换行之后开始（字节领土，逐字节保持）。
    let body_start = fm_close_line_end + 1;
    let body = if body_start <= bytes.len() { &bytes[body_start..] } else { &[][..] };

    let value: serde_json::Value = serde_json::from_slice(fm_json).map_err(|e| {
        MemError::FrontmatterJson { path: PATH.into(), reason: e.to_string() }
    })?;
    let obj = value.as_object().ok_or_else(|| MemError::FrontmatterShape {
        path: PATH.into(),
        reason: "frontmatter 顶层必须是 JSON object（恰含 envelope/payload 两键）".into(),
    })?;
    if obj.len() != 2 || !obj.contains_key("envelope") || !obj.contains_key("payload") {
        return Err(MemError::FrontmatterShape {
            path: PATH.into(),
            reason: format!(
                "frontmatter 顶层必须恰含 envelope/payload 两键，实际键 {keys:?}",
                keys = obj.keys().collect::<Vec<_>>()
            ),
        });
    }
    Ok((value, body))
}

/// 在 [from, len) 内找行尾（返回换行符位置；无换行则返回 len-1 视作最后一行末尾）。
fn find_line_end(bytes: &[u8], from: usize) -> Option<usize> {
    bytes[from..]
        .iter()
        .position(|&b| b == b'\n')
        .map(|i| from + i)
        .or(if from < bytes.len() { Some(bytes.len()) } else { None })
}

/// 去掉行尾 \r（规范允许分隔符行带 \r）。
fn trim_cr(line: &[u8]) -> &[u8] {
    match line.last() {
        Some(b'\r') => &line[..line.len() - 1],
        _ => line,
    }
}
