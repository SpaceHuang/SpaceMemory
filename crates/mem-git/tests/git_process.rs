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

// ---------- R11（新增面9）：只读历史方法（restore_body 来源通道，BD4） ----------

mod r11_history {
    use super::*;
    use mem_core::ops::BodyHistory;

    #[test]
    fn list_versions_newest_first_and_bytes_at_rev() {
        let tmp = git_dir();
        let root = tmp.path();
        let g = GitProcess::new(root);
        // v1 → 提交 → v2 → 提交：按路径列历史（自新至旧）+ 按版本取字节。
        let sha1 = g
            .commit_paths(root, &["a.md".into(), "events.jsonl".into()], "v1")
            .unwrap()
            .unwrap();
        std::fs::write(root.join("a.md"), b"record bytes v2").unwrap();
        let sha2 = g
            .commit_paths(root, &["a.md".into(), "events.jsonl".into()], "v2")
            .unwrap()
            .unwrap();
        let versions = g.list_versions("a.md").unwrap();
        assert_eq!(versions, vec![sha2.clone(), sha1.clone()], "自新至旧");
        assert_eq!(g.file_bytes_at("a.md", &sha1).unwrap(), Some(b"record bytes".to_vec()));
        assert_eq!(g.file_bytes_at("a.md", &sha2).unwrap(), Some(b"record bytes v2".to_vec()));
    }

    #[test]
    fn file_bytes_at_reports_missing_path_as_none_and_bad_rev_as_error() {
        let tmp = git_dir();
        let root = tmp.path();
        let g = GitProcess::new(root);
        g.commit_paths(root, &["a.md".into()], "v1").unwrap();
        // 路径在该版本不存在 → Ok(None)（history 锚定扫描逐版本取文件的合法分支）。
        assert!(g.file_bytes_at("records/never.md", "HEAD").unwrap().is_none());
        // 坏版本引用 → Err（响亮，不静默当 None——错误信息区分成因）。
        assert!(g.file_bytes_at("a.md", "deadbeef-not-a-rev").is_err());
    }

    #[test]
    fn no_git_dir_is_structured_error_for_history_sources() {
        // 无 .git → 历史通道结构化报错（BD4：git 缺席即无历史通道，file: 源仍可用）。
        let nogit = tempfile::tempdir().unwrap();
        let g = GitProcess::new(nogit.path());
        let e1 = g.list_versions("a.md").unwrap_err();
        assert!(e1.to_string().contains(".git"), "{e1}");
        let e2 = g.file_bytes_at("a.md", "HEAD").unwrap_err();
        assert!(e2.to_string().contains(".git"), "{e2}");
    }

    #[test]
    fn body_history_trait_object_wiring() {
        // 与 ops::restore_body 的接线形态一致：&dyn BodyHistory。
        let tmp = git_dir();
        let root = tmp.path();
        let g = GitProcess::new(root);
        g.commit_paths(root, &["a.md".into()], "base").unwrap();
        fn via_trait(h: &dyn BodyHistory, rel: &str) -> Option<Vec<u8>> {
            let versions = h.list_versions(rel).unwrap();
            let rev = versions.first()?.clone();
            h.file_bytes_at(rel, &rev).unwrap()
        }
        assert_eq!(via_trait(&g, "a.md"), Some(b"record bytes".to_vec()));
    }
}

// ---------- R11（BD5 验证点 1）：git 克隆字节直通端到端 ----------

#[test]
fn r11_clone_byte_direct_with_gitattributes() {
    // 源机：记录文件（非法 UTF-8 正文 + LF 正文）+ events.jsonl + .gitattributes
    // 一次性入树（BD5 ①②：规则文件随受管写批次入树）。
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    let mut bin_body: Vec<u8> = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A];
    bin_body.extend_from_slice(b"binary\x00\xff\xfe");
    let record_bin = format_record(&bin_body);
    let record_lf = format_record(b"line1\nline2\n");
    let digest_bin = mem_core::token::sha256_hex(&record_bin);
    std::fs::create_dir_all(src.join("records/memory")).unwrap();
    std::fs::write(src.join("records/memory/bin.md"), &record_bin).unwrap();
    std::fs::write(src.join("records/memory/lf.md"), &record_lf).unwrap();
    let digest_lf = mem_core::token::sha256_hex(&record_lf);
    // 事件行：登记磁盘字节摘要（克隆后深档审计的对账锚）。
    std::fs::write(
        src.join("events.jsonl"),
        format!(
            "{{\"by\":\"m\",\"id\":\"bin\",\"kind\":\"op\",\"new_digest\":\"{digest_bin}\",\"new_v\":1,\"old_digest\":null,\"old_v\":null,\"op\":\"create\",\"path\":\"records/memory/bin.md\",\"seq\":1,\"ts\":\"t\"}}\n{{\"by\":\"m\",\"id\":\"lf\",\"kind\":\"op\",\"new_digest\":\"{digest_lf}\",\"new_v\":1,\"old_digest\":null,\"old_v\":null,\"op\":\"create\",\"path\":\"records/memory/lf.md\",\"seq\":2,\"ts\":\"t\"}}\n"
        )
        .as_bytes(),
    )
    .unwrap();
    // BD5 ①：.gitattributes 随首次提交入树。
    std::fs::write(src.join(".gitattributes"), b"* -text\n").unwrap();
    run(&src, &["init", "-q"]);
    let g = GitProcess::new(&src);
    let sha = g
        .commit_paths(
            &src,
            &[
                "records/memory/bin.md".into(),
                "records/memory/lf.md".into(),
                "events.jsonl".into(),
                ".gitattributes".into(),
            ],
            "seed",
        )
        .unwrap()
        .expect("源机提交");
    let _ = sha;
    // 断言规则已在提交树且内容恰为 `* -text`（完成标记 = 提交树核验）。
    let ls = std::process::Command::new("git").arg("-C").arg(&src)
        .args(["show", "HEAD:.gitattributes"]).output().unwrap();
    assert!(ls.status.success() && ls.stdout == b"* -text\n",
        ".gitattributes 必须在提交树且内容恰为 `* -text\\n`");

    // 克隆机：强制 core.autocrlf=true（行尾激进配置——BD5 的立法场景）。
    let dst = tmp.path().join("dst");
    run(tmp.path(), &["clone", "-q", "-c", "core.autocrlf=true", src.to_str().unwrap(), dst.to_str().unwrap()]);
    let cloned_bin = std::fs::read(dst.join("records/memory/bin.md")).unwrap();
    let cloned_lf = std::fs::read(dst.join("records/memory/lf.md")).unwrap();
    assert_eq!(cloned_bin, record_bin, "非法 UTF-8 正文逐字节直通");
    assert_eq!(cloned_lf, record_lf, "LF 正文逐字节直通（autocrlf=true 不改字节）");
    // 事件最后登记摘要 == 克隆机磁盘摘要（深档审计跨机一致，R11.1 迁移语义）。
    assert_eq!(mem_core::token::sha256_hex(&cloned_bin), digest_bin);
    assert_eq!(mem_core::token::sha256_hex(&cloned_lf), digest_lf);
}

/// 手写最小记录文件（三段式：--- / frontmatter JSON / 正文）。
fn format_record(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"---\n{\"envelope\":{\"created\":\"t\",\"id\":\"x\",\"modified\":\"t\",\"refs\":[],\"type\":\"memory\",\"version\":1},\"payload\":{\"title\":\"t\"}}\n---\n");
    out.extend_from_slice(body);
    out
}
