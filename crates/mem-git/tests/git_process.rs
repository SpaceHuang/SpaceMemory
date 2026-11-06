//! mem-git 集成测试（真实 git 子进程）。
//!
//! 覆盖生产面的此前缺漏：
//! - path-scoped 提交的**批次精确性**（树中恰为传入路径——不只是"有 SHA"）；
//! - "nothing to commit" → Ok(None)（提交滞后合法语义的一半）；
//! - index.lock 竞争重试耗尽 → Err（D17 可重试资源的关键分支）；
//! - HistoryStore::previous_file（fix 流依赖；mem-core 测试用的是 shim，此处测真实实现）；
//! - 无全局 git 身份时 -c user.name/email 兜底（干净环境隔离）；
//! - CommitSink 以 trait object 使用（与 ops::WriteContext 的接线形态一致）。

use mem_core::ops::{CommitSink, HistoryStore};
use mem_git::{Git, GitProcess};

fn run(dir: &std::path::Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?} 失败：{}", String::from_utf8_lossy(&out.stderr));
}

/// 隔离全局/系统 git 配置（证明 -c 标志自身足以提交——干净环境兜底）。
fn isolated(cmd: &mut std::process::Command) -> &mut std::process::Command {
    cmd.env("GIT_CONFIG_GLOBAL", "zzz-missing-gitconfig")
        .env("GIT_CONFIG_SYSTEM", "zzz-missing-gitconfig")
        .env("GIT_CONFIG_NOSYSTEM", "1");
    cmd
}

fn git_dir() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.md"), b"record bytes").unwrap();
    std::fs::write(tmp.path().join("events.jsonl"), b"{}\n").unwrap();
    run(tmp.path(), &["init", "-q"]);
    tmp
}

#[test]
fn commit_batch_contains_exactly_the_given_paths() {
    let tmp = git_dir();
    let root = tmp.path();
    // 树外多一个未跟踪文件：path-scoped add/commit 不得卷入它。
    std::fs::write(root.join("untracked.txt"), b"noise").unwrap();
    let g = GitProcess::new(root);
    let sha = g
        .commit_paths(root, &["a.md".into(), "events.jsonl".into()], "batch")
        .unwrap()
        .expect("有 .git 且有变更 → 提交");
    assert_eq!(sha.len(), 40);
    let tree = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-tree", "-r", "HEAD", "--name-only"])
        .output()
        .unwrap();
    let names = String::from_utf8_lossy(&tree.stdout);
    assert!(names.contains("a.md"), "{names}");
    assert!(names.contains("events.jsonl"), "{names}");
    assert!(!names.contains("untracked.txt"), "path-scoped 提交不得卷入树外路径：{names}");
}

#[test]
fn nothing_to_commit_returns_none() {
    let tmp = git_dir();
    let root = tmp.path();
    let g = GitProcess::new(root);
    g.commit_paths(root, &["a.md".into(), "events.jsonl".into()], "first").unwrap();
    // 无新变更：第二次提交 → Ok(None)（提交滞后合法，不是错误）。
    let out = g.commit_paths(root, &["a.md".into(), "events.jsonl".into()], "second").unwrap();
    assert!(out.is_none(), "nothing to commit 必须为 Ok(None)");
}

#[test]
fn stale_index_lock_exhausts_retries_and_errors() {
    let tmp = git_dir();
    let root = tmp.path();
    // 陈旧 index.lock：git 拒绝创建自己的锁 → add/commit 全部失败 → 重试耗尽。
    std::fs::write(root.join(".git/index.lock"), b"stale").unwrap();
    let g = GitProcess::new(root);
    let err = g
        .commit_paths(root, &["a.md".into(), "events.jsonl".into()], "blocked")
        .unwrap_err();
    assert!(err.to_string().contains("index.lock"), "{err}");
    // 清掉陈旧锁后可重试成功（可重试资源语义）。
    std::fs::remove_file(root.join(".git/index.lock")).unwrap();
    let sha = g.commit_paths(root, &["a.md".into(), "events.jsonl".into()], "after").unwrap();
    assert!(sha.is_some());
}

#[test]
fn previous_file_reads_head_and_reports_missing() {
    let tmp = git_dir();
    let root = tmp.path();
    let g = GitProcess::new(root);
    // 无 .git → None（红线10：历史缺席合法）。
    let nogit = tempfile::tempdir().unwrap();
    let g2 = GitProcess::new(nogit.path());
    assert!(g2.previous_file("a.md").unwrap().is_none());
    // HEAD 中存在 → 字节精确。
    g.commit_paths(root, &["a.md".into(), "events.jsonl".into()], "base").unwrap();
    assert_eq!(g.previous_file("a.md").unwrap(), Some(b"record bytes".to_vec()));
    // HEAD 中不存在（未提交/已删除路径）→ None，不报错。
    assert!(g.previous_file("records/task/never.md").unwrap().is_none());
}

#[test]
fn commit_sink_works_as_trait_object() {
    let tmp = git_dir();
    let root = tmp.path();
    // 与 ops::WriteContext 的接线形态一致：&dyn CommitSink。
    fn via_sink(sink: &dyn CommitSink, paths: &[String]) -> Option<String> {
        sink.commit(paths).unwrap()
    }
    let g = GitProcess::new(root);
    let sha = via_sink(&g, &["a.md".into(), "events.jsonl".into()]);
    assert!(sha.is_some());
}

#[test]
fn commit_succeeds_without_global_git_identity() {
    // 干净环境（无全局/系统配置）：仅靠 GitProcess 内置的 -c user.name/email。
    let tmp = git_dir();
    let root = tmp.path();
    let mut cmd = std::process::Command::new("git");
    let out = isolated(&mut cmd)
        .arg("-C")
        .arg(root)
        .args(["commit", "-m", "would fail without identity"])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "前置确认：隔离环境裸 git commit 应失败（无身份）——{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let g = GitProcess::new(root);
    let sha = g.commit_paths(root, &["a.md".into(), "events.jsonl".into()], "with -c flags").unwrap();
    assert!(sha.is_some(), "GitProcess 的 -c 兜底必须在无全局身份时成功");
}
