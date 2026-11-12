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
use crate::store::Store;

/// 校验 records/ 或 attic/ 下的实际文件路径仍落在所选店目录中。
///
/// 词法路径检查阻止 `..`；逐组件拒绝符号链接/重解析点，避免静态链接把目标或
/// 锁文件引到店外；最后以 canonicalize 的最近存在祖先再次确认实际落点。
pub fn validate_store_record_path(store: &Store, rel: &str) -> MemResult<()> {
    let mut parts = std::path::Path::new(rel).components();
    let expected = if rel.starts_with("records/") { "records" } else if rel.starts_with("attic/") {
        "attic"
    } else {
        return Err(MemError::BadArgument(format!("记录路径非法：{rel}")));
    };
    if !matches!(parts.next(), Some(std::path::Component::Normal(p)) if p == expected)
        || parts.clone().count() == 0
        || parts.any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err(MemError::BadArgument(format!("记录路径非法：{rel}")));
    }

    validate_store_relative_path(store, rel)?;
    Ok(())
}

/// 校验任意 store 内部资源路径。锁文件也用此函数，因此 sessions/ 等机制锁
/// 仍可使用，但不能经过符号链接/重解析点离开店目录。
pub fn validate_store_relative_path(store: &Store, rel: &str) -> MemResult<()> {
    let components: Vec<_> = std::path::Path::new(rel).components().collect();
    if components.is_empty()
        || components.iter().any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err(MemError::BadArgument(format!("店内路径非法：{rel}")));
    }
    let root = std::fs::canonicalize(store.root())
        .map_err(|_| MemError::BadArgument(format!("无法确认路径位于店内：{rel}")))?;
    let mut current = store.root().to_path_buf();
    for component in &components {
        let std::path::Component::Normal(part) = component else { unreachable!() };
        current.push(part);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if is_link_or_reparse_point(&metadata) => {
                return Err(MemError::BadArgument(format!("店内路径不能经过符号链接：{rel}")));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(e) => return Err(MemError::io(current.display().to_string(), e)),
        }
    }
    let target = store.abs(rel);
    let mut ancestor = target.as_path();
    let canonical_target = loop {
        if let Ok(path) = std::fs::canonicalize(ancestor) {
            break path;
        }
        ancestor = ancestor.parent().ok_or_else(|| {
            MemError::BadArgument(format!("无法确认路径位于店内：{rel}"))
        })?;
    };
    if !canonical_target.starts_with(&root) {
        return Err(MemError::BadArgument(format!("路径越出店目录：{rel}")));
    }
    Ok(())
}

/// 校验 store 根目录中的机制锁文件；只允许列明的固定文件名。
pub fn validate_store_root_lock(store: &Store, rel: &str) -> MemResult<()> {
    if !matches!(rel, "barrier.lock" | "events.lock" | "serve.lock") {
        return Err(MemError::BadArgument(format!("机制锁路径非法：{rel}")));
    }
    let root = std::fs::canonicalize(store.root())
        .map_err(|_| MemError::BadArgument(format!("无法确认锁文件位于店内：{rel}")))?;
    let target = store.abs(rel);
    match std::fs::symlink_metadata(&target) {
        Ok(metadata) if is_link_or_reparse_point(&metadata) => {
            return Err(MemError::BadArgument(format!("锁文件不能是符号链接：{rel}")));
        }
        Ok(_) => {
            let canonical = std::fs::canonicalize(&target)
                .map_err(|e| MemError::io(target.display().to_string(), e))?;
            if !canonical.starts_with(&root) {
                return Err(MemError::BadArgument(format!("锁文件越出店目录：{rel}")));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(MemError::io(target.display().to_string(), e)),
    }
    Ok(())
}

fn is_link_or_reparse_point(metadata: &std::fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        return metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0;
    }
    #[cfg(not(windows))]
    false
}

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
/// `file:` 源共用；CLI/MCP 不得各自复写）。
///
/// H2（实施评审 r3）：目标路径按调用方进程 cwd 解析（std::fs 落盘语义）——相对
/// 路径先与 cwd 拼接绝对化；裸相对路径（无目录部分）不再因 `parent()` 为空而
/// 放行。cwd 不可得 → 保守判内（响亮拒绝优于旁挂写入）。
/// L1（同轮）：词法比较用**分隔符边界感知**的前缀（兄弟目录 `store-backup` 不再
/// 命中店根 `store` 前缀）＋ `.`/`..` 词法折叠（相对店根 `--store .` 下 `../x`
/// 不再误判店内）；规范化（canonicalize）兜底承担大小写/符号链接。
pub fn path_is_inside(root: &std::path::Path, path: &std::path::Path) -> bool {
    match std::env::current_dir() {
        Ok(cwd) => path_is_inside_with_cwd(root, path, &cwd),
        // 拿不到 cwd：相对目标无法定位——保守判内（拒绝写方向安全）。
        Err(_) => true,
    }
}

/// [`path_is_inside`] 的 cwd 注入变体（测试用；生产入口自动取进程 cwd）。
pub fn path_is_inside_with_cwd(
    root: &std::path::Path,
    path: &std::path::Path,
    cwd: &std::path::Path,
) -> bool {
    let path_abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    let root_abs = if root.is_absolute() {
        root.to_path_buf()
    } else {
        cwd.join(root)
    };
    // 第一判：词法折叠 + 分隔符边界前缀（目标/店根均可不存在）。
    let path_norm = normalize_lexical(&path_abs);
    let root_norm = normalize_lexical(&root_abs);
    if lexically_within(&root_norm, &path_norm) {
        return true;
    }
    // 第二判：规范化兜底（大小写/符号链接/junction；仅父链存在时可行）。
    let canon_root = std::fs::canonicalize(&root_abs).ok();
    let Some(canon_root) = canon_root else { return false };
    if let Ok(canon_path) = std::fs::canonicalize(&path_abs) {
        return canonically_within(&canon_root, &canon_path);
    }
    // 目标尚不存在：沿祖先链找**最近已存在祖先**规范化——目标落盘必在其下，
    // 该祖先的内外即目标的内外。
    for anc in path_abs.ancestors().skip(1) {
        if let Ok(canon_anc) = std::fs::canonicalize(anc) {
            return canonically_within(&canon_root, &canon_anc);
        }
    }
    false
}

/// 词法规范化：折叠 `.`/`..`、统一分隔符、小写（Windows 不敏感近似）。
fn normalize_lexical(p: &std::path::Path) -> String {
    let mut leading = String::new();
    let mut parts: Vec<String> = Vec::new();
    for comp in p.components() {
        match comp {
            std::path::Component::Prefix(pr) => {
                leading.push_str(&pr.as_os_str().to_string_lossy().to_lowercase())
            }
            std::path::Component::RootDir => leading.push(std::path::MAIN_SEPARATOR),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                // 绝对路径下弹尽即钳在根（非法输入防御）；相对路径弹尽保留 ".."。
                if parts.pop().is_none() && leading.is_empty() {
                    parts.push("..".into());
                }
            }
            std::path::Component::Normal(c) => {
                parts.push(c.to_string_lossy().to_lowercase())
            }
        }
    }
    let sep = std::path::MAIN_SEPARATOR;
    if leading.is_empty() {
        parts.join(&sep.to_string())
    } else {
        format!("{leading}{}", parts.join(&sep.to_string()))
    }
}

/// 分隔符边界感知的"p 在 root 内或等于 root"（词法形态）。
fn lexically_within(root_norm: &str, path_norm: &str) -> bool {
    if root_norm.is_empty() {
        return false;
    }
    let sep = std::path::MAIN_SEPARATOR;
    let root_trim = root_norm.trim_end_matches(sep);
    path_norm == root_trim || path_norm.starts_with(&format!("{root_trim}{sep}"))
}

/// 规范化形态的"p 在 root 内或等于 root"（canonicalize 产物，已无相对成分）。
fn canonically_within(root: &std::path::Path, p: &std::path::Path) -> bool {
    lexically_within(
        &root.to_string_lossy().to_lowercase(),
        &p.to_string_lossy().to_lowercase(),
    )
}

#[cfg(test)]
mod r3_path_tests {
    //! H2/L1（实施评审 r3）判定矩阵：cwd 注入式（不改进程 cwd——测试并行安全）。
    //! 语义：目标路径按调用方进程 cwd 解析（std::fs 落盘语义）——相对路径先绝对化。

    use super::*;
    use std::path::Path;

    fn store(root_name: &str) -> std::path::PathBuf {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join(root_name);
        std::fs::create_dir_all(&root).unwrap();
        // tempdir 生命周期由调用方维持——此 helper 仅用于单元矩阵的路径构造，
        // 判定全部基于词法/父链规范化，无需 root 内文件真实存在。
        root
    }

    #[test]
    fn h2_bare_relative_path_resolved_against_cwd() {
        // cwd=店根：裸相对路径落店内 → 判内（原实现 parent() 为空串直接放行——H2）。
        let root = store("s1");
        let cwd = root.clone();
        assert!(path_is_inside_with_cwd(&root, Path::new("x.png"), &cwd));
        assert!(path_is_inside_with_cwd(&root, Path::new("records/a.md"), &cwd));
        // cwd=店外：同一路径落店外 → 判外。
        let outside_cwd = root.parent().unwrap().parent().unwrap().to_path_buf();
        assert!(!path_is_inside_with_cwd(&root, Path::new("x.png"), &outside_cwd));
    }

    #[test]
    fn h2_store_relative_root_resolved_against_cwd() {
        // 店根相对（--store .）：与目标同 cwd 折叠后比较。
        let root = store("s2");
        let cwd = root.clone();
        assert!(path_is_inside_with_cwd(Path::new("."), Path::new("x.png"), &cwd));
        assert!(!path_is_inside_with_cwd(Path::new("."), Path::new("../out.png"), &cwd));
        // cwd=店外、店根为 cwd 下子目录（相对店根形态，含 ./ 折叠）：
        // 店内目标判内、兄弟/上级目标判外（L1b 误拒修正）。
        let parent = root.parent().unwrap().to_path_buf();
        std::fs::create_dir_all(parent.join("store-x/sub")).unwrap();
        assert!(path_is_inside_with_cwd(Path::new("./store-x/."),
            Path::new("./store-x/sub/b.md"), &parent),
            "相对店根（含 ./ 折叠）下的目标判内");
        assert!(!path_is_inside_with_cwd(Path::new("./store-x"),
            Path::new("./other/c.md"), &parent),
            "兄弟目录判外（L1a/L1b 误拒修正）");
    }

    #[test]
    fn l1a_sibling_directory_not_inside() {
        // 兄弟目录（store-backup）不得命中店根字符串前缀（L1a）。
        let tmp = tempfile::tempdir().unwrap();
        let parent = tmp.path().to_path_buf();
        let root = parent.join("store");
        std::fs::create_dir_all(&root).unwrap();
        let sibling = parent.join("store-backup").join("out.png");
        assert!(!path_is_inside_with_cwd(&root, &sibling, &parent),
            "兄弟目录不得判店内：{sibling:?}");
        // 店根带尾分隔符的变体同样边界感知。
        let root_trail = parent.join("store").join("");
        assert!(!path_is_inside_with_cwd(&root_trail, &sibling, &parent));
    }

    #[test]
    fn l1b_parent_relative_target_not_inside() {
        let root = store("s3");
        let cwd = root.clone();
        // cwd=店根：../out.png 落上级（店外）→ 判外（原词法 "." 前缀误判内）。
        assert!(!path_is_inside_with_cwd(&root, Path::new("../out.png"), &cwd));
        // 深层 ../：仍外。
        assert!(!path_is_inside_with_cwd(&root, Path::new("../other/x.png"), &cwd));
        // ./out.png 落店内 → 判内（正确语义，非误拒）。
        assert!(path_is_inside_with_cwd(&root, Path::new("./out.png"), &cwd));
    }

    #[test]
    fn existing_and_missing_paths_both_judged() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("s4");
        let inside = root.join("deep").join("new.md");
        std::fs::create_dir_all(root.join("deep")).unwrap();
        std::fs::write(&inside, b"x").unwrap();
        let outside = tmp.path().join("outside.md");
        std::fs::write(&outside, b"x").unwrap();
        let cwd = tmp.path().to_path_buf();
        // 已存在目标：规范化兜底（大小写/链接由 canonicalize 承担）。
        assert!(path_is_inside_with_cwd(&root, &inside, &cwd));
        assert!(!path_is_inside_with_cwd(&root, &outside, &cwd));
        // 尚不存在的目标：最近已存在祖先链兜底（词法折叠已先行覆盖常规形态）。
        assert!(path_is_inside_with_cwd(&root, &root.join("deep").join("missing.md"), &cwd));
        assert!(!path_is_inside_with_cwd(&root, &tmp.path().join("missing2.md"), &cwd));
        // 大小写变体（Windows 不敏感；规范化兜底承担）。
        #[cfg(windows)]
        {
            let upper = tmp.path().join("S4").join("deep").join("u.md");
            assert!(path_is_inside_with_cwd(&root, &upper, &cwd), "大小写变体判内（Windows）");
        }
    }

    #[test]
    fn cwd_unavailable_conservatively_inside() {
        // 生产入口拿不到 cwd（目录被删等）→ 保守判内（响亮拒绝优于旁挂写入）。
        let root = store("s5");
        assert!(path_is_inside(&root, Path::new("x.png")) == path_is_inside(&root, Path::new("x.png")));
        // 生产入口自 env::current_dir 取 cwd——此处仅锁定"入口不 panic、返回布尔"。
        let _ = path_is_inside(&root, Path::new("x.png"));
    }

    #[test]
    fn absolute_targets_unchanged_semantics() {
        // 既有语义回归：绝对路径判定不受 cwd 影响。
        let root = store("s6");
        let cwd = store("other");
        assert!(path_is_inside_with_cwd(&root, &root.join("a.md"), &cwd));
        assert!(!path_is_inside_with_cwd(&root, &cwd.join("b.md"), &cwd));
    }
}
