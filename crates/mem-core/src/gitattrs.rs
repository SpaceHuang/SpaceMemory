//! 店级 `.gitattributes` 行尾直通（R11/BD5 四件套；R11.1/R11.2）。
//!
//! git 的行尾自动转换（core.autocrlf / text=auto）会把"提交时的字节"与"检出时的
//! 字节"改得不一致——克隆后盘上字节与事件日志登记摘要永久不符（深档审计永久报
//! 漂移、修订令牌跨机失效）。`* -text` 全店禁行尾转换：提交什么字节、检出什么字节。
//!
//! 四件套闭合（BD5）：
//! ① init 写入并随首次提交入树（`seed_at_root` + 批次携带）；
//! ② 写例程核验——工作区缺失则补种并把该文件纳入当批按路径提交
//!    （commit-pending 批次同样携带，直至核验入树——见 ops::commit_same_batch）；
//! ③ 工作区已存在且内容不同 → 结构化拒绝受管写入（含修复指引；读路径不受影响）；
//! ④ verify 增审计项——有 .git 的店，`.gitattributes` 已在**提交树**中且内容相符
//!    （完成标记 = 提交树核验，非工作区文件存在）。
//!
//! 无 .git 的店不适用（git 缺席即无行尾转换问题，红线10：`.git` 是附属物）。

use crate::error::{MemError, MemResult};
use crate::store::Store;

pub const GITATTRIBUTES_REL: &str = ".gitattributes";
/// 默认内容（BD5：全店禁行尾转换；末尾换行——git 文本文件的规范形态）。
pub const GITATTRIBUTES_CONTENT: &[u8] = b"* -text\n";

fn has_git(store: &Store) -> bool {
    store.root().join(".git").exists()
}

/// init 播种（四件套之①）：店根写入默认内容。幂等——已存在则不动（不吞用户
/// 自带文件；其冲突状态由 [`ensure_for_write`] 在受管写入时结构化处置）。
pub fn seed_at_root(root: &std::path::Path) -> MemResult<()> {
    let path = root.join(GITATTRIBUTES_REL);
    if path.exists() {
        return Ok(());
    }
    std::fs::write(&path, GITATTRIBUTES_CONTENT)
        .map_err(|e| MemError::io(path.display().to_string(), e))
}

/// 受管写前的行尾规则核验（四件套之②③；有 .git 的店适用）。
///
/// - 无 .git → 不适用，Ok(false)；
/// - 工作区缺文件 → 补种默认内容，Ok(true)（调用方把该文件纳入当批提交——
///   ops::commit_same_batch 统一携带，pending 批次同）；
/// - 工作区内容 ≠ 默认 → 结构化拒绝（恢复默认内容即解锁；读路径不受影响）；
/// - 内容相符 → Ok(false)。
pub fn ensure_for_write(store: &Store) -> MemResult<bool> {
    if !has_git(store) {
        return Ok(false);
    }
    let path = store.abs(GITATTRIBUTES_REL);
    match std::fs::read(&path) {
        Ok(bytes) if bytes == GITATTRIBUTES_CONTENT => Ok(false),
        Ok(_) => Err(MemError::GitAttributesConflict {
            path: path.display().to_string(),
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::write(&path, GITATTRIBUTES_CONTENT)
                .map_err(|e| MemError::io(path.display().to_string(), e))?;
            Ok(true)
        }
        Err(e) => Err(MemError::io(path.display().to_string(), e)),
    }
}

/// verify 审计判定（四件套之④）：有 .git 的店按**提交树**内容核验（完成标记 =
/// 提交树核验，§7 迁移表）。返回发现清单（空 = 通过）。
///
/// - `head_content = None`：未入树（含 HEAD 未诞生/读取失败——存量店升级窗口或
///   首次受管写入之前，BD5 后果所列注记）；
/// - 内容不符：行尾直通不成立（如实暴露——既有分叉由深档审计继续报告）。
pub fn audit_findings(has_git: bool, head_content: Option<&[u8]>) -> Vec<String> {
    if !has_git {
        return Vec::new();
    }
    match head_content {
        Some(bytes) if bytes == GITATTRIBUTES_CONTENT => Vec::new(),
        Some(_) => vec![format!(
            "{GITATTRIBUTES_REL} 已在提交树但内容与默认不符——行尾直通不成立（BD5 ④；恢复默认内容 `* -text` 并提交）"
        )],
        None => vec![format!(
            "{GITATTRIBUTES_REL} 未入提交树（首次受管写入时随批入树；存量店升级窗口，BD5 注记）"
        )],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::OpenMode;

    fn store_with_git(dir: &std::path::Path, git: bool) -> Store {
        let root = dir.join(if git { "git-store" } else { "plain-store" });
        Store::create(&root).unwrap();
        if git {
            std::fs::create_dir_all(root.join(".git")).unwrap();
        }
        Store::open(&root, OpenMode::Auto).unwrap()
    }

    #[test]
    fn no_git_store_not_applicable() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_with_git(tmp.path(), false);
        assert!(!has_git(&store));
        assert_eq!(ensure_for_write(&store).unwrap(), false);
        assert!(!store.abs(GITATTRIBUTES_REL).exists(), "无 .git 的店不补种（BD5：不适用）");
        assert!(audit_findings(false, None).is_empty());
    }

    #[test]
    fn missing_file_reseeded_and_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_with_git(tmp.path(), true);
        assert_eq!(ensure_for_write(&store).unwrap(), true, "补种须回报（入当批提交）");
        assert_eq!(std::fs::read(store.abs(GITATTRIBUTES_REL)).unwrap(), GITATTRIBUTES_CONTENT);
        // 再核验：内容相符 → Ok(false)。
        assert_eq!(ensure_for_write(&store).unwrap(), false);
    }

    #[test]
    fn conflicting_content_rejects_managed_writes_with_guidance() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_with_git(tmp.path(), true);
        std::fs::write(store.abs(GITATTRIBUTES_REL), b"*.png binary\n").unwrap();
        let err = ensure_for_write(&store).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("* -text"), "错误含修复指引（恢复默认内容即解锁）：{msg}");
        // 读路径不受影响——本函数只在写例程调用。
        // 恢复默认内容后解锁。
        std::fs::write(store.abs(GITATTRIBUTES_REL), GITATTRIBUTES_CONTENT).unwrap();
        assert_eq!(ensure_for_write(&store).unwrap(), false);
    }

    #[test]
    fn audit_findings_tree_content_is_the_marker() {
        // 未入树 → 发现（升级窗口如实标注）。
        assert_eq!(audit_findings(true, None).len(), 1);
        // 入树但内容不符 → 发现。
        assert_eq!(audit_findings(true, Some(b"*.png binary\n")).len(), 1);
        // 入树且内容恰为默认 → 通过（完成标记 = 提交树核验）。
        assert!(audit_findings(true, Some(GITATTRIBUTES_CONTENT)).is_empty());
    }

    #[test]
    fn seed_at_root_is_idempotent_and_never_overwrites() {
        let tmp = tempfile::tempdir().unwrap();
        seed_at_root(tmp.path()).unwrap();
        // 用户自带文件不被覆盖（冲突处置交给写例程的结构化拒绝）。
        std::fs::write(tmp.path().join(GITATTRIBUTES_REL), b"custom\n").unwrap();
        seed_at_root(tmp.path()).unwrap();
        assert_eq!(std::fs::read(tmp.path().join(GITATTRIBUTES_REL)).unwrap(), b"custom\n");
    }
}
