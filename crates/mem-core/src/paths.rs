//! 规范路径与键字符集规则（D19 / D11）。
//!
//! - 规范化 NFC；路径推导用 casefold（Windows 大小写不敏感：两键一径 → EEXIST → 同一记录）；
//! - slug 允许 Unicode 字母/数字/连字符/下划线（XID 语义的宽实现：`char::is_alphanumeric`
//!   ∪ `_` ∪ `-`）；黑名单 = 路径敌对字符（`<>:"/\|?*`）、控制符、首尾点/空格；
//!   保留名禁用（店级目录/文件名 + Windows 设备名）；长度上限 128 字节；
//! - 唯一键 → 规范路径原子独占（R3.3）：并发创建的唯一性由基础设施强制，不依赖
//!   写入者"先查后建"；输者返回既有记录及令牌（D11）。
//!
//! 身份是地址不是名字：引用按 ID 不按名（存在性强制）；slug 只是键成分；别名只
//! 作用于查找解析层（D11 / 红线11）。

use crate::error::{MemError, MemResult};

/// 店级保留名（envelope-format.md §1.2；大小写折叠后精确匹配）。
const RESERVED: &[&str] = &[
    "conventions", "types", "records", "attic", "sessions", "agenda", "index",
    "events", "tmp", ".git", "mem.json", "events.jsonl", "barrier.lock",
    "events.lock",
];

/// Windows 保留设备名（stem 匹配，大小写折叠；con.txt 同样拒收）。
const WIN_DEVICES: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6",
    "com7", "com8", "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6",
    "lpt7", "lpt8", "lpt9",
];

/// 单个键成分的长度上限（UTF-8 字节；envelope-format.md §2.4）。
pub const MAX_COMPONENT_BYTES: usize = 128;

/// NFC 规范化。
pub fn nfc(s: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    s.nfc().collect()
}

/// 路径推导折叠：NFC + Unicode 小写映射（casefold 的实现近似，见
/// type-declaration.md §5——唯一性从不依赖折叠正确性，由路径原子独占保证）。
pub fn fold(s: &str) -> String {
    nfc(s).to_lowercase()
}

/// 校验一个键成分（slug / 唯一键标量字段值 / 别名 / 类型名）。
///
/// 合法：Unicode 字母/数字（`is_alphanumeric`，含 CJK）、`_`、`-`；
/// 非法：路径敌对字符 `<>:"/\|?*`、控制符、首尾点/空格、保留名、Windows 设备名、
/// 空、超 128 字节。
pub fn validate_component(s: &str) -> MemResult<()> {
    let n = nfc(s);
    if n.is_empty() {
        return Err(MemError::BadArgument("键成分不能为空".into()));
    }
    if n.len() > MAX_COMPONENT_BYTES {
        return Err(MemError::BadArgument(format!(
            "键成分超长（{n} 字节 > {MAX_COMPONENT_BYTES}）：{s:?}"
        )));
    }
    if n.starts_with('.') || n.starts_with(' ') || n.ends_with('.') || n.ends_with(' ') {
        return Err(MemError::BadArgument(format!(
            "键成分不得以点/空格开头或结尾：{s:?}"
        )));
    }
    for c in n.chars() {
        if (c as u32) < 0x20 || c as u32 == 0x7f {
            return Err(MemError::BadArgument(format!("键成分含控制符：{s:?}")));
        }
        if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
            return Err(MemError::BadArgument(format!(
                "键成分含路径敌对字符 {c:?}：{s:?}"
            )));
        }
        if !(c.is_alphanumeric() || c == '_' || c == '-') {
            return Err(MemError::BadArgument(format!(
                "键成分含非法字符 {c:?}（允许 Unicode 字母/数字/下划线/连字符）：{s:?}"
            )));
        }
    }
    let f = fold(&n);
    if RESERVED.contains(&f.as_str()) {
        return Err(MemError::BadArgument(format!("键成分命中店级保留名：{s:?}")));
    }
    let stem = f.split('.').next().unwrap_or(&f);
    if WIN_DEVICES.contains(&stem) {
        return Err(MemError::BadArgument(format!("键成分命中 Windows 设备名：{s:?}")));
    }
    Ok(())
}

/// 唯一键 → 记录规范路径（records/ 相对路径，`/` 分隔）。
///
/// - 有键：`records/<type>/<k1折叠>/<…>/<kn折叠>.md`（键序 = x-unique-key 序；
///   引用字段的成分即目标 ID，标量字段取折叠值）；
/// - 无键：`records/<type>/<id>.md`（身份即路径成分）。
pub fn derive_record_rel(
    type_name: &str,
    key_components: &[String],
    id: &str,
) -> MemResult<String> {
    validate_component(type_name).map_err(|_| {
        MemError::BadArgument(format!("类型名非法：{type_name:?}"))
    })?;
    if key_components.is_empty() {
        return Ok(format!("records/{type_name}/{id}.md"));
    }
    let mut parts = Vec::with_capacity(key_components.len());
    for k in key_components {
        let folded = fold(k);
        // 折叠后校验（拒绝注入敌对字符）。
        validate_component(&folded).map_err(|e| {
            MemError::BadArgument(format!("唯一键成分非法 {k:?}：{e}"))
        })?;
        parts.push(folded);
    }
    let last = parts.len() - 1;
    let mut rel = format!("records/{type_name}");
    for (i, p) in parts.iter().enumerate() {
        rel.push('/');
        rel.push_str(p);
        if i == last {
            rel.push_str(".md");
        }
    }
    Ok(rel)
}

/// 记录锁路径 = 记录规范路径 + `.lock`（锁文件独立于记录文件存在——absent/待创建
/// 同样可锁，D12/红线24）。
pub fn record_lock_rel(record_rel: &str) -> String {
    format!("{record_rel}.lock")
}

/// records/ 相对路径 → attic/ 相对路径（受管 tombstone 同相对路径，规范 §3.3）。
pub fn attic_rel(record_rel: &str) -> MemResult<String> {
    let rest = record_rel
        .strip_prefix("records/")
        .ok_or_else(|| MemError::BadArgument(format!("非 records/ 路径：{record_rel}")))?;
    Ok(format!("attic/{rest}"))
}

/// attic/ 相对路径 → records/ 相对路径（restore 用）。
pub fn records_rel(attic_rel_path: &str) -> MemResult<String> {
    let rest = attic_rel_path
        .strip_prefix("attic/")
        .ok_or_else(|| MemError::BadArgument(format!("非 attic/ 路径：{attic_rel_path}")))?;
    Ok(format!("records/{rest}"))
}

/// 判定 path 是否位于 root 目录内（R11：`--body-out` 目标与 restore-body 的
/// `file:` 源共用；CLI/MCP 不得各自复写）。词法前缀 + 规范化前缀双判——目标可能
/// 尚不存在（父目录链不一定可规范化）；Windows 大小写不敏感。
pub fn path_is_inside(root: &std::path::Path, path: &std::path::Path) -> bool {
    fn norm(p: &std::path::Path) -> String {
        p.to_string_lossy()
            .to_lowercase()
            .replace('/', std::path::MAIN_SEPARATOR_STR)
    }
    if norm(path).starts_with(&norm(root)) {
        return true;
    }
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => return false,
    };
    match std::fs::canonicalize(parent) {
        Ok(c) => {
            let root_canon = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
            c.starts_with(&root_canon)
        }
        Err(_) => false,
    }
}
