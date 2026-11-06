//! Git 基座（D4）。
//!
//! git 承担历史、审计、撤销、多机同步；**path-scoped 提交**：记录路径 + events.jsonl
//! 同批提交（commit-pending 重试同；不分配序号）。派生关系禁用 git 祖先承载（红线2）；
//! 线性 main、无分支模型（红线7）；`.git` 是附属物——去掉后存储仍完整合法（红线10）；
//! git 提交只提供历史，不提供 worktree 跨记录原子可见性。
//!
//! 实现路线：CLI 子进程首发 → git2/libgit2（§6）。
//!
//! 无 .git 形态合法：全部增量与正确性语义不依赖 git（D12/红线26）——本实现探到
//! 无 .git / 无 git 可执行文件时静默跳过（返回 Ok(None)），历史缺席不是故障。

pub mod process;

pub use process::GitProcess;

use mem_core::error::MemResult;

/// Git 提交能力（mem-core 的 `CommitSink` 由本类型实现——见 process.rs）。
pub trait Git {
    /// path-scoped 提交：`git commit -m <msg> -- <paths…>`（含 events.jsonl 同批）。
    /// 返回提交 SHA；无 .git / git 不可用 → Ok(None)。
    fn commit_paths(&self, workdir: &std::path::Path, paths: &[String], msg: &str)
        -> MemResult<Option<String>>;
}
