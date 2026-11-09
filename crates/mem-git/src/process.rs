//! Git trait 的 CLI 子进程实现（首发路线，D4）。
//!
//! index.lock 竞争可重试（指数退避，默认 ≤3 次）；耗尽 → commit-pending
//! （`.pending-commits` 对账，不分配序号）；提交滞后合法（D17）。
//! 无 .git / 无 git 可执行文件 → Ok(None)（历史缺席不是故障，红线10）。

use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

use mem_core::error::{MemError, MemResult};
use mem_core::ops::CommitSink;

use crate::Git;

const MAX_RETRIES: u32 = 3;

pub struct GitProcess {
    workdir: std::path::PathBuf,
    /// 提交者信息（首次提交时配置 -c）。
    committer: String,
    retries: AtomicU32,
}

impl GitProcess {
    pub fn new(workdir: impl Into<std::path::PathBuf>) -> Self {
        Self {
            workdir: workdir.into(),
            committer: "mem <mem@local>".into(),
            retries: AtomicU32::new(0),
        }
    }

    /// HEAD 上一版文件内容（fix 流：git 历史取上一版 frontmatter，D18/R6.3）。
    pub fn show_file(&self, rel: &str) -> Option<Vec<u8>> {
        if !self.workdir.join(".git").exists() {
            return None;
        }
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.workdir)
            .args(["show", &format!("HEAD:{rel}")])
            .output()
            .ok()?;
        if out.status.success() {
            Some(out.stdout)
        } else {
            None
        }
    }

    fn git(&self, args: &[&str]) -> std::io::Result<std::process::Output> {
        Command::new("git")
            .arg("-C")
            .arg(&self.workdir)
            .args(["-c", &format!("user.name={}", self.committer.split('<').next().unwrap_or("mem").trim())])
            .args(["-c", &format!("user.email={}", self.committer.split('<').nth(1).map(|s| s.trim_end_matches('>')).unwrap_or("mem@local"))])
            .args(args)
            .output()
    }
}

impl Git for GitProcess {
    fn commit_paths(
        &self,
        _workdir: &std::path::Path,
        paths: &[String],
        msg: &str,
    ) -> MemResult<Option<String>> {
        // 无 .git → 静默跳过（红线10：附属物）。
        if !self.workdir.join(".git").exists() {
            return Ok(None);
        }
        let mut last_err = String::new();
        for attempt in 0..=MAX_RETRIES {
            // add 与 commit 同处重试环：add 的 index.lock 失败与 commit 同根因
            //（此前 add 失败被静默吞掉，误导为 commit 的 pathspec 报错且不重试）。
            let mut add_args = vec!["add", "--"];
            add_args.extend(paths.iter().map(|s| s.as_str()));
            let add_err = match self.git(&add_args) {
                Ok(out) if out.status.success() => String::new(),
                Ok(out) => format!("add: {}", String::from_utf8_lossy(&out.stderr).trim()),
                Err(e) => format!("add: {e}"),
            };
            let mut commit_args = vec!["commit", "-m", msg, "--"];
            commit_args.extend(paths.iter().map(|s| s.as_str()));
            match self.git(&commit_args) {
                Ok(out) if out.status.success() => {
                    self.retries.store(0, Ordering::Relaxed);
                    // 取 SHA。
                    if let Ok(sha_out) = self.git(&["rev-parse", "HEAD"]) {
                        if sha_out.status.success() {
                            return Ok(Some(String::from_utf8_lossy(&sha_out.stdout).trim().to_string()));
                        }
                    }
                    return Ok(None);
                }
                Ok(out) => {
                    // "nothing to commit" 走 stdout，其余诊断在 stderr——双流合并分类。
                    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
                    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
                    let combined = format!("{stderr}\n{stdout}");
                    last_err = format!("{} {}", add_err, combined.trim());
                    // index.lock 竞争（add 或 commit 侧）→ 可重试（指数退避）。
                    if (combined.contains("index.lock") || add_err.contains("index.lock"))
                        && attempt < MAX_RETRIES
                    {
                        std::thread::sleep(std::time::Duration::from_millis(50 * (1 << attempt)));
                        continue;
                    }
                    // 无可提交内容（外部已提交）不算失败。
                    if combined.contains("nothing to commit")
                        || combined.contains("no changes added")
                    {
                        return Ok(None);
                    }
                    return Err(MemError::Git(last_err.trim().to_string()));
                }
                Err(e) => {
                    // git 可执行文件不存在 → 历史缺席合法。
                    if e.kind() == std::io::ErrorKind::NotFound {
                        return Ok(None);
                    }
                    return Err(MemError::Git(e.to_string()));
                }
            }
        }
        Err(MemError::Git(format!(
            "index.lock 重试耗尽（{MAX_RETRIES} 次）：{last_err}"
        )))
    }
}

impl CommitSink for GitProcess {
    fn commit(&self, paths: &[String]) -> MemResult<Option<String>> {
        let wd = self.workdir.clone();
        self.commit_paths(&wd, paths, "mem: 记录写入（含 events.jsonl 同批，D3/高优1）")
    }
}

impl mem_core::ops::HistoryStore for GitProcess {
    fn previous_file(&self, rel: &str) -> MemResult<Option<Vec<u8>>> {
        Ok(self.show_file(rel))
    }
}

// ---------- R11（新增面9）：只读历史方法（restore_body 来源通道，BD4） ----------

fn require_git_dir(workdir: &std::path::Path) -> MemResult<()> {
    if workdir.join(".git").exists() {
        Ok(())
    } else {
        Err(MemError::Git(format!(
            "无 .git（{}）——history/git: 历史通道不可用（BD4：git 缺席即无历史通道，file: 源仍可用）",
            workdir.display()
        )))
    }
}

impl GitProcess {
    /// HEAD 树中该相对路径的字节（verify 的 .gitattributes 提交树核验用，BD5 ④；
    /// 只读 best-effort：无 .git / git 缺席 / 读取失败 → Ok(None)——历史缺席是
    /// 合法形态，红线10）。
    pub fn head_file(&self, rel: &str) -> MemResult<Option<Vec<u8>>> {
        if !self.workdir.join(".git").exists() {
            return Ok(None);
        }
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.workdir)
            .args(["show", &format!("HEAD:{rel}")])
            .output()
            .map_err(|e| MemError::Git(format!("git show 失败：{e}")))?;
        Ok(if out.status.success() { Some(out.stdout) } else { None })
    }
}

impl mem_core::ops::BodyHistory for GitProcess {
    fn file_bytes_at(&self, rel: &str, rev: &str) -> MemResult<Option<Vec<u8>>> {
        require_git_dir(&self.workdir)?;
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.workdir)
            .args(["show", &format!("{rev}:{rel}")])
            .output()
            .map_err(|e| MemError::Git(format!("git show 失败：{e}")))?;
        if out.status.success() {
            return Ok(Some(out.stdout));
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        // "路径在该版本不存在"是扫描的合法分支 → Ok(None)；坏版本引用等 → Err。
        if stderr.contains("does not exist") {
            Ok(None)
        } else {
            Err(MemError::Git(format!(
                "取历史版本失败（{rev}:{rel}）：{}",
                stderr.trim()
            )))
        }
    }

    fn list_versions(&self, rel: &str) -> MemResult<Vec<String>> {
        require_git_dir(&self.workdir)?;
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.workdir)
            .args(["log", "--format=%H", "--", rel])
            .output()
            .map_err(|e| MemError::Git(format!("git log 失败：{e}")))?;
        if !out.status.success() {
            return Err(MemError::Git(format!(
                "列历史版本失败（{rel}）：{}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_without_git_dir_is_ok_none() {
        let tmp = tempfile::tempdir().unwrap();
        let g = GitProcess::new(tmp.path());
        let out = g.commit_paths(tmp.path(), &["a.md".into()], "m").unwrap();
        assert!(out.is_none(), "无 .git：历史缺席合法（红线10）");
    }

    #[test]
    fn commit_paths_same_batch_with_events() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("events.jsonl"), b"{}\n").unwrap();
        std::fs::write(root.join("a.md"), b"x").unwrap();
        run(root, &["init", "-q"]);
        let g = GitProcess::new(root);
        let sha = g
            .commit_paths(root, &["a.md".into(), "events.jsonl".into()], "test")
            .unwrap()
            .expect("有 .git 且有变更 → 提交");
        assert_eq!(sha.len(), 40, "完整 SHA-1");
    }

    fn run(dir: &std::path::Path, args: &[&str]) {
        let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?} 失败：{}", String::from_utf8_lossy(&out.stderr));
    }
}
