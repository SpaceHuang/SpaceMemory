//! 下游契约的独立进程公开 API 回归。
use serde_json::Value;
use std::process::{Command, Output};

fn mem(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mem"))
        .args(args)
        .output()
        .unwrap()
}

fn json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout 不是 JSON: {e}; stdout={}; stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn run_bounded(args: &[&str], timeout: std::time::Duration) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_mem"))
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("命令超过配置验收时限 {:?}: {args:?}", timeout);
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn independent_cli_processes_compete_on_same_cas_token() {
    let tmp = tempfile::tempdir().unwrap();
    let store = tmp.path().join("store");
    let store_s = store.to_str().unwrap();
    let init = mem(&[
        "--store",
        store_s,
        "init",
        store_s,
        "--principal",
        "codex_main",
    ]);
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );

    let added = mem(&[
        "--store",
        store_s,
        "add",
        "memory",
        "--principal",
        "codex_main",
        "--payload",
        r#"{"title":"initial"}"#,
    ]);
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    let added_json = json(&added);
    let id = added_json["id"].as_str().unwrap().to_owned();
    let token = added_json["token"].as_str().unwrap().to_owned();
    let events_path = store.join("events.jsonl");
    let events_before = std::fs::read_to_string(&events_path)
        .unwrap()
        .lines()
        .count();

    let make_update = |title: &'static str| {
        Command::new(env!("CARGO_BIN_EXE_mem"))
            .args([
                "--store",
                store_s,
                "update",
                &id,
                "--expect",
                &token,
                "--payload",
                &format!(r#"{{"title":"{title}"}}"#),
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap()
    };
    let a = make_update("winner-a");
    let b = make_update("winner-b");
    let a = a.wait_with_output().unwrap();
    let b = b.wait_with_output().unwrap();
    let outcomes = [a, b];
    assert_eq!(
        outcomes.iter().filter(|o| o.status.success()).count(),
        1,
        "恰一笔 CAS 成功"
    );
    let loser = outcomes.iter().find(|o| !o.status.success()).unwrap();
    let loser_error: Value = serde_json::from_slice(&loser.stderr).unwrap();
    assert_eq!(
        loser_error["conflict"], true,
        "输者必须收到结构化 CAS 冲突: {loser_error}"
    );
    assert!(
        loser_error["error"].as_str().unwrap().contains("当前"),
        "冲突需包含当前令牌: {loser_error}"
    );

    let shown = mem(&["--store", store_s, "show", &id]);
    assert!(
        shown.status.success(),
        "{}",
        String::from_utf8_lossy(&shown.stderr)
    );
    let shown = json(&shown);
    assert_eq!(shown["envelope"]["version"], 2);
    assert!(matches!(
        shown["payload"]["title"].as_str(),
        Some("winner-a" | "winner-b")
    ));
    let events_after = std::fs::read_to_string(events_path)
        .unwrap()
        .lines()
        .count();
    assert_eq!(events_after, events_before + 1, "失败写不得追加事件");
}

#[test]
fn independent_cli_processes_create_same_unique_identity_once() {
    let tmp = tempfile::tempdir().unwrap();
    let store = tmp.path().join("store");
    let store_s = store.to_str().unwrap();
    let init = mem(&[
        "--store",
        store_s,
        "init",
        store_s,
        "--principal",
        "codex_main",
    ]);
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    let events_path = store.join("events.jsonl");
    let events_before = std::fs::read_to_string(&events_path)
        .unwrap()
        .lines()
        .count();

    let spawn = |title: &'static str| {
        Command::new(env!("CARGO_BIN_EXE_mem"))
            .args([
                "--store",
                store_s,
                "add",
                "project",
                "--principal",
                "codex_main",
                "--payload",
                &format!(r#"{{"slug":"same-key","title":"{title}","status":"active"}}"#),
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap()
    };
    let a = spawn("process-a");
    let b = spawn("process-b");
    let a = a.wait_with_output().unwrap();
    let b = b.wait_with_output().unwrap();
    assert!(a.status.success(), "{}", String::from_utf8_lossy(&a.stderr));
    assert!(b.status.success(), "{}", String::from_utf8_lossy(&b.stderr));
    let a = json(&a);
    let b = json(&b);
    assert_eq!(a["id"], b["id"], "同唯一键只能指向一个 ID");
    assert_ne!(a["created"], b["created"], "仅一个调用报告新建");
    let events_after = std::fs::read_to_string(events_path)
        .unwrap()
        .lines()
        .count();
    assert_eq!(
        events_after,
        events_before + 1,
        "输者不能为相同身份追加第二个创建事件"
    );
}

#[test]
fn independent_process_writes_to_distinct_records_share_event_sequence() {
    let tmp = tempfile::tempdir().unwrap();
    let store = tmp.path().join("store");
    let store_s = store.to_str().unwrap();
    let init = mem(&[
        "--store",
        store_s,
        "init",
        store_s,
        "--principal",
        "codex_main",
    ]);
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    let events_path = store.join("events.jsonl");
    let before: Vec<mem_core::event::Event> = std::fs::read_to_string(&events_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let max_before = before.last().map(|e| e.seq).unwrap_or(0);

    let spawn = |title: &'static str| {
        Command::new(env!("CARGO_BIN_EXE_mem"))
            .args([
                "--store",
                store_s,
                "add",
                "memory",
                "--principal",
                "codex_main",
                "--payload",
                &format!(r#"{{"title":"{title}"}}"#),
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap()
    };
    let a = spawn("parallel-a");
    let b = spawn("parallel-b");
    let a = a.wait_with_output().unwrap();
    let b = b.wait_with_output().unwrap();
    assert!(a.status.success(), "{}", String::from_utf8_lossy(&a.stderr));
    assert!(b.status.success(), "{}", String::from_utf8_lossy(&b.stderr));
    let ids = [
        json(&a)["id"].as_str().unwrap().to_owned(),
        json(&b)["id"].as_str().unwrap().to_owned(),
    ];
    assert_ne!(ids[0], ids[1]);
    let after: Vec<mem_core::event::Event> = std::fs::read_to_string(events_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(after.len(), before.len() + 2);
    assert_eq!(after[after.len() - 2].seq, max_before + 1);
    assert_eq!(after[after.len() - 1].seq, max_before + 2);
    assert_eq!(
        after[after.len() - 2..]
            .iter()
            .map(|event| event.id.as_str())
            .collect::<std::collections::BTreeSet<_>>(),
        ids.iter()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>(),
    );
}

#[test]
fn independent_cli_processes_enforce_reference_delete_contract() {
    let tmp = tempfile::tempdir().unwrap();
    let store = tmp.path().join("store");
    let store_s = store.to_str().unwrap();
    let init = mem(&[
        "--store",
        store_s,
        "init",
        store_s,
        "--principal",
        "codex_main",
    ]);
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );

    let project = mem(&[
        "--store",
        store_s,
        "add",
        "project",
        "--principal",
        "codex_main",
        "--payload",
        r#"{"slug":"a3-project","title":"A3 Project","status":"active"}"#,
    ]);
    assert!(
        project.status.success(),
        "{}",
        String::from_utf8_lossy(&project.stderr)
    );
    let project = json(&project);
    let project_id = project["id"].as_str().unwrap();
    let task = mem(&[
        "--store",
        store_s,
        "add",
        "task",
        "--principal",
        "codex_main",
        "--payload",
        &format!(
            r#"{{"slug":"a3-task","title":"A3 Task","status":"todo","project":"{project_id}"}}"#
        ),
    ]);
    assert!(
        task.status.success(),
        "{}",
        String::from_utf8_lossy(&task.stderr)
    );

    let blocked = mem(&[
        "--store",
        store_s,
        "delete",
        project_id,
        "--expect",
        project["token"].as_str().unwrap(),
    ]);
    assert!(!blocked.status.success(), "被任务引用的项目默认不得删除");
    let after_block = mem(&["--store", store_s, "show", project_id]);
    assert!(after_block.status.success(), "拒绝删除后原记录必须保留");

    let forced = mem(&[
        "--store",
        store_s,
        "delete",
        project_id,
        "--expect",
        project["token"].as_str().unwrap(),
        "--force",
    ]);
    assert!(
        forced.status.success(),
        "强制删除应保留悬空引用警示: {}",
        String::from_utf8_lossy(&forced.stderr)
    );
    assert!(
        json(&forced)["tombstone"]
            .as_str()
            .unwrap()
            .starts_with("attic/")
    );
}

#[test]
fn already_open_store_handle_observes_other_process_writes() {
    let tmp = tempfile::tempdir().unwrap();
    let store_path = tmp.path().join("store");
    let store_s = store_path.to_str().unwrap();
    let init = mem(&[
        "--store",
        store_s,
        "init",
        store_s,
        "--principal",
        "codex_main",
    ]);
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    let store = mem_core::store::Store::open(&store_path, mem_core::store::OpenMode::Auto).unwrap();

    let added = mem(&[
        "--store",
        store_s,
        "add",
        "memory",
        "--principal",
        "codex_main",
        "--payload",
        r#"{"title":"visible-to-existing-handle"}"#,
    ]);
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    let added = json(&added);
    let id = added["id"].as_str().unwrap();
    let record = mem_core::ops::get(&store, id).expect("已有 Store 句柄必须读到另一进程提交的文件");
    assert_eq!(record.record.payload["title"], "visible-to-existing-handle");
    assert_eq!(record.record.envelope.version, 1);
}

#[test]
#[cfg(debug_assertions)]
fn scan_can_observe_a_valid_mixed_snapshot_during_other_process_updates() {
    let tmp = tempfile::tempdir().unwrap();
    let store_path = tmp.path().join("store");
    let store_s = store_path.to_str().unwrap();
    let init = mem(&[
        "--store",
        store_s,
        "init",
        store_s,
        "--principal",
        "codex_main",
    ]);
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    let store = mem_core::store::Store::open(&store_path, mem_core::store::OpenMode::Auto).unwrap();
    let add = |title: &'static str| {
        let output = mem(&[
            "--store",
            store_s,
            "add",
            "memory",
            "--principal",
            "codex_main",
            "--payload",
            &format!(r#"{{"title":"{title}"}}"#),
        ]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        json(&output)
    };
    let first = add("before-first");
    let second = add("before-second");
    let first_id = first["id"].as_str().unwrap().to_owned();
    let second_id = second["id"].as_str().unwrap().to_owned();
    let first_token = first["token"].as_str().unwrap().to_owned();
    let second_token = second["token"].as_str().unwrap().to_owned();
    let interleaved = std::cell::Cell::new(false);

    let scanned = mem_core::ops::scan_all_with_hook_test(&store, |record| {
        if record.record.envelope.id != first_id || interleaved.get() {
            return;
        }
        interleaved.set(true);
        for (id, token, title) in [
            (&first_id, &first_token, "after-first"),
            (&second_id, &second_token, "after-second"),
        ] {
            let output = mem(&[
                "--store",
                store_s,
                "update",
                id,
                "--expect",
                token,
                "--payload",
                &format!(r#"{{"title":"{title}"}}"#),
            ]);
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    })
    .unwrap();
    assert!(interleaved.get(), "扫描必须经过同步暂停点");

    let observed: std::collections::BTreeMap<_, _> = scanned
        .records
        .iter()
        .filter(|record| {
            record.record.envelope.id == first_id || record.record.envelope.id == second_id
        })
        .map(|record| {
            (
                record.record.envelope.id.as_str(),
                record.record.payload["title"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(observed.get(first_id.as_str()), Some(&"before-first"));
    assert_eq!(observed.get(second_id.as_str()), Some(&"after-second"));
    for id in [&first_id, &second_id] {
        let current = mem_core::ops::get(&store, id).unwrap();
        assert!(
            current.record.payload["title"]
                .as_str()
                .unwrap()
                .starts_with("after-")
        );
    }
}

#[test]
#[cfg(debug_assertions)]
fn child_writer_pauses_at_requested_durability_window() {
    let (Ok(root), Ok(ready), Ok(stage)) = (
        std::env::var("SM_KILL_WINDOW_STORE"),
        std::env::var("SM_KILL_WINDOW_READY"),
        std::env::var("SM_KILL_WINDOW_STAGE"),
    ) else {
        return;
    };
    let store =
        mem_core::store::Store::open(std::path::Path::new(&root), mem_core::store::OpenMode::Auto)
            .unwrap();
    let types = mem_schema::TypeSet::load_dir(&store.abs("types")).unwrap();
    let ready_path = std::path::PathBuf::from(ready);
    let wanted = stage.clone();
    let hook: mem_core::event::PauseHook = Box::new(move |observed| {
        if observed != wanted {
            return;
        }
        std::fs::write(&ready_path, b"ready").unwrap();
        loop {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    });
    let ctx = mem_core::ops::WriteContext::new(&store, &types, "kill-window-child")
        .with_committer(None)
        .with_session_id(None);
    let _ = mem_core::ops::add_with_pause_hook_test(
        &ctx,
        mem_core::ops::AddArgs {
            type_name: "memory".into(),
            payload: serde_json::json!({"title":"kill-window"}),
            body: Vec::new(),
            principal: Some("codex_main".into()),
        },
        hook,
    )
    .unwrap();
}

#[test]
fn child_leaves_partial_event_tail_for_reconcile() {
    let (Ok(root), Ok(ready)) = (
        std::env::var("SM_PARTIAL_EVENT_STORE"),
        std::env::var("SM_PARTIAL_EVENT_READY"),
    ) else {
        return;
    };
    let store =
        mem_core::store::Store::open(std::path::Path::new(&root), mem_core::store::OpenMode::Auto)
            .unwrap();
    use std::io::Write;
    let path = store.abs("events.jsonl");
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(b"{\"seq\":999,\"ts\":\"partial").unwrap();
    file.sync_all().unwrap();
    std::fs::write(ready, b"ready").unwrap();
    loop {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn reconcile_discards_partial_event_without_duplicate_when_store_is_stable() {
    use std::time::{Duration, Instant};
    let tmp = tempfile::tempdir().unwrap();
    let store_path = tmp.path().join("store");
    let store_s = store_path.to_str().unwrap();
    let init = mem(&[
        "--store",
        store_s,
        "init",
        store_s,
        "--principal",
        "codex_main",
    ]);
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    let store = mem_core::store::Store::open(&store_path, mem_core::store::OpenMode::Auto).unwrap();
    let types = mem_schema::TypeSet::load_dir(&store.abs("types")).unwrap();
    let ctx = mem_core::ops::WriteContext::new(&store, &types, "prepare-stable-tail")
        .with_committer(None)
        .with_session_id(None);
    let added = mem_core::ops::add(
        &ctx,
        mem_core::ops::AddArgs {
            type_name: "memory".into(),
            payload: serde_json::json!({"title":"stable-tail"}),
            body: Vec::new(),
            principal: Some("codex_main".into()),
        },
    )
    .unwrap();
    let before_events = mem_core::event::EventLog::new(&store)
        .read_log()
        .unwrap()
        .events
        .len();
    let ready = tmp.path().join("partial-event-ready");
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "child_leaves_partial_event_tail_for_reconcile",
            "--nocapture",
        ])
        .env("SM_PARTIAL_EVENT_STORE", &store_path)
        .env("SM_PARTIAL_EVENT_READY", &ready)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready.exists() {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("子进程未留下半行: {status}");
        }
        assert!(Instant::now() < deadline, "等待部分事件尾行超时");
        std::thread::sleep(Duration::from_millis(10));
    }
    child.kill().unwrap();
    let _ = child.wait().unwrap();

    let report = mem_core::event::reconcile_backfill(&store, "parent-reconcile", None).unwrap();
    assert!(report.appended.is_empty());
    let repaired = mem_core::event::EventLog::new(&store).read_log().unwrap();
    assert!(!repaired.torn_tail);
    assert_eq!(repaired.events.len(), before_events);
    assert_eq!(repaired.events.last().unwrap().id, added.record.envelope.id);
}

#[test]
fn reconcile_repairs_event_tail_left_by_killed_process() {
    use std::time::{Duration, Instant};
    let tmp = tempfile::tempdir().unwrap();
    let store_path = tmp.path().join("store");
    let store_s = store_path.to_str().unwrap();
    let init = mem(&[
        "--store",
        store_s,
        "init",
        store_s,
        "--principal",
        "codex_main",
    ]);
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    let store = mem_core::store::Store::open(&store_path, mem_core::store::OpenMode::Auto).unwrap();
    let types = mem_schema::TypeSet::load_dir(&store.abs("types")).unwrap();
    let ctx = mem_core::ops::WriteContext::new(&store, &types, "prepare-partial-tail")
        .with_committer(None)
        .with_session_id(None);
    let added = mem_core::ops::add(
        &ctx,
        mem_core::ops::AddArgs {
            type_name: "memory".into(),
            payload: serde_json::json!({"title":"tail-recovery"}),
            body: Vec::new(),
            principal: Some("codex_main".into()),
        },
    )
    .unwrap();
    let mut envelope = added.record.envelope.clone();
    envelope.version += 1;
    envelope.modified = "2026-11-22T00:00:00Z".into();
    let changed = mem_core::record::serialize_new(
        &envelope,
        &serde_json::json!({"title":"externally-updated"}),
        b"",
    );
    std::fs::write(store.abs(&added.rel), changed).unwrap();
    let events_path = store.abs("events.jsonl");
    let before_bytes = std::fs::read(&events_path).unwrap();
    let before_events = mem_core::event::EventLog::new(&store)
        .read_log()
        .unwrap()
        .events
        .len();

    let ready = tmp.path().join("partial-event-ready");
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "child_leaves_partial_event_tail_for_reconcile",
            "--nocapture",
        ])
        .env("SM_PARTIAL_EVENT_STORE", &store_path)
        .env("SM_PARTIAL_EVENT_READY", &ready)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready.exists() {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("子进程未留下部分事件尾行，退出状态 {status}");
        }
        assert!(Instant::now() < deadline, "等待部分事件尾行超时");
        std::thread::sleep(Duration::from_millis(10));
    }
    child.kill().unwrap();
    let _ = child.wait().unwrap();

    let torn = mem_core::event::EventLog::new(&store).read_log().unwrap();
    assert!(torn.torn_tail);
    let bytes_with_tail = std::fs::read(&events_path).unwrap();
    assert!(bytes_with_tail.starts_with(&before_bytes));
    assert!(bytes_with_tail.len() > before_bytes.len());
    let reconciled = run_bounded(&["--store", store_s, "reconcile"], Duration::from_secs(10));
    assert!(
        reconciled.status.success(),
        "reconcile 超时或失败: {}",
        String::from_utf8_lossy(&reconciled.stderr)
    );
    assert_eq!(json(&reconciled)["backfill_appended"], 1);
    let repaired = mem_core::event::EventLog::new(&store).read_log().unwrap();
    assert!(!repaired.torn_tail);
    assert_eq!(repaired.events.len(), before_events + 1);
    assert_eq!(repaired.events.last().unwrap().id, added.record.envelope.id);
    assert!(mem_core::drift::diff_since(&store, 0, 200, 100).is_ok());
    let read = run_bounded(
        &["--store", store_s, "show", &added.record.envelope.id],
        Duration::from_secs(10),
    );
    assert!(
        read.status.success(),
        "恢复后的记录不可读: {}",
        String::from_utf8_lossy(&read.stderr)
    );
    let write = run_bounded(
        &[
            "--store",
            store_s,
            "add",
            "memory",
            "--principal",
            "codex_main",
            "--payload",
            r#"{"title":"write-after-tail-recovery"}"#,
        ],
        Duration::from_secs(10),
    );
    assert!(
        write.status.success(),
        "恢复后不能继续写: {}",
        String::from_utf8_lossy(&write.stderr)
    );
}

#[test]
#[cfg(debug_assertions)]
fn process_death_at_publish_and_event_boundaries_is_reconcilable() {
    use std::time::{Duration, Instant};
    for stage in ["record_published", "event_appended"] {
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path().join("store");
        let store_s = store_path.to_str().unwrap();
        let init = mem(&[
            "--store",
            store_s,
            "init",
            store_s,
            "--principal",
            "codex_main",
        ]);
        assert!(
            init.status.success(),
            "{}",
            String::from_utf8_lossy(&init.stderr)
        );
        let store =
            mem_core::store::Store::open(&store_path, mem_core::store::OpenMode::Auto).unwrap();
        let before_ids: std::collections::BTreeSet<_> = mem_core::ops::scan_all(&store)
            .unwrap()
            .records
            .into_iter()
            .map(|record| record.record.envelope.id)
            .collect();
        let before_events = mem_core::event::EventLog::new(&store)
            .read_log()
            .unwrap()
            .events
            .len();
        let ready = tmp.path().join("child-ready");
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "child_writer_pauses_at_requested_durability_window",
                "--nocapture",
            ])
            .env("SM_KILL_WINDOW_STORE", &store_path)
            .env("SM_KILL_WINDOW_READY", &ready)
            .env("SM_KILL_WINDOW_STAGE", stage)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready.exists() {
            if let Some(status) = child.try_wait().unwrap() {
                panic!("子进程未到达 {stage} 暂停点，退出状态 {status}");
            }
            assert!(Instant::now() < deadline, "等待子进程到达 {stage} 超时");
            std::thread::sleep(Duration::from_millis(10));
        }
        child.kill().unwrap();
        let _ = child.wait().unwrap();

        let after_scan = mem_core::ops::scan_all(&store).unwrap();
        let created: Vec<_> = after_scan
            .records
            .iter()
            .filter(|record| !before_ids.contains(&record.record.envelope.id))
            .collect();
        assert_eq!(created.len(), 1, "发布完成的记录应保持可解析");
        let after_events = mem_core::event::EventLog::new(&store)
            .read_log()
            .unwrap()
            .events;
        let new_events: Vec<_> = after_events.iter().skip(before_events).collect();
        if stage == "record_published" {
            assert!(
                new_events.is_empty(),
                "发布与事件之间被杀时不得出现半行事件"
            );
        } else {
            assert_eq!(new_events.len(), 1, "事件已同步追加后必须保留");
            assert_eq!(new_events[0].id, created[0].record.envelope.id);
        }

        let reconciled = run_bounded(&["--store", store_s, "reconcile"], Duration::from_secs(10));
        assert!(
            reconciled.status.success(),
            "恢复进程超时或失败: {}",
            String::from_utf8_lossy(&reconciled.stderr)
        );
        let report = json(&reconciled);
        if stage == "record_published" {
            assert_eq!(report["backfill_appended"], 1, "缺失事件应由显式回填补齐");
        } else {
            assert_eq!(report["backfill_appended"], 0, "已有事件不得重复回填");
        }
        let final_events = mem_core::event::EventLog::new(&store)
            .read_log()
            .unwrap()
            .events;
        assert_eq!(
            final_events
                .iter()
                .filter(|event| event.id == created[0].record.envelope.id)
                .count(),
            1
        );
        let read = run_bounded(
            &["--store", store_s, "show", &created[0].record.envelope.id],
            Duration::from_secs(10),
        );
        assert!(
            read.status.success(),
            "恢复后的记录不可读: {}",
            String::from_utf8_lossy(&read.stderr)
        );
        let write = run_bounded(
            &[
                "--store",
                store_s,
                "add",
                "memory",
                "--principal",
                "codex_main",
                "--payload",
                r#"{"title":"write-after-process-death"}"#,
            ],
            Duration::from_secs(10),
        );
        assert!(
            write.status.success(),
            "写者进程崩溃后存活进程不能继续写: {}",
            String::from_utf8_lossy(&write.stderr)
        );
    }
}

#[test]
fn child_installs_missing_feature_for_stale_handle_check() {
    let Ok(root) = std::env::var("SM_FEATURE_CHANGE_STORE") else {
        return;
    };
    let config_path = std::path::Path::new(&root).join("mem.json");
    let mut config: Value = serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    config["required_features"] = serde_json::json!(["record.concurrent-upgrade/v1"]);
    std::fs::write(config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
}

#[test]
#[cfg(debug_assertions)]
fn child_migration_pauses_at_format_switch() {
    let (Ok(root), Ok(ready), Ok(stage)) = (
        std::env::var("SM_MIGRATION_KILL_STORE"),
        std::env::var("SM_MIGRATION_KILL_READY"),
        std::env::var("SM_MIGRATION_KILL_STAGE"),
    ) else {
        return;
    };
    let ready_path = std::path::PathBuf::from(ready);
    let wanted = stage.clone();
    mem_core::store::Store::migrate_format_v1_test_with_pause(
        std::path::Path::new(&root),
        "backup-process-test",
        move |observed| {
            if observed != wanted {
                return;
            }
            std::fs::write(&ready_path, b"ready").unwrap();
            loop {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        },
    )
    .unwrap();
}

#[test]
#[cfg(debug_assertions)]
fn migration_process_death_before_and_after_atomic_switch_is_recoverable() {
    use std::time::{Duration, Instant};
    for (stage, expected_version, retry_status) in [
        ("before_format_switch", serde_json::json!(1), "migrated"),
        (
            "after_format_switch",
            serde_json::json!("spacememory/2"),
            "already_migrated",
        ),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path().join("legacy-store");
        mem_core::store::Store::create(&store_path).unwrap();
        let config_path = store_path.join("mem.json");
        let mut config: Value =
            serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
        config["format_version"] = serde_json::json!(1);
        config.as_object_mut().unwrap().remove("required_features");
        config["downstream_unknown"] = serde_json::json!({"preserve": ["x", 9]});
        std::fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();

        let ready = tmp.path().join("migration-ready");
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "child_migration_pauses_at_format_switch",
                "--nocapture",
            ])
            .env("SM_MIGRATION_KILL_STORE", &store_path)
            .env("SM_MIGRATION_KILL_READY", &ready)
            .env("SM_MIGRATION_KILL_STAGE", stage)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready.exists() {
            if let Some(status) = child.try_wait().unwrap() {
                panic!("迁移子进程未到达 {stage} 暂停点，退出状态 {status}");
            }
            assert!(Instant::now() < deadline, "等待迁移暂停点 {stage} 超时");
            std::thread::sleep(Duration::from_millis(10));
        }
        child.kill().unwrap();
        let _ = child.wait().unwrap();

        let interrupted: Value =
            serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
        assert_eq!(
            interrupted["format_version"], expected_version,
            "stage={stage}"
        );
        assert_eq!(
            interrupted["downstream_unknown"],
            config["downstream_unknown"]
        );
        let retry = run_bounded(
            &[
                "--store",
                store_path.to_str().unwrap(),
                "migrate",
                "--backup-identity",
                "backup-process-test",
            ],
            Duration::from_secs(10),
        );
        assert!(
            retry.status.success(),
            "迁移恢复重试失败: {}",
            String::from_utf8_lossy(&retry.stderr)
        );
        assert_eq!(json(&retry)["status"], retry_status, "stage={stage}");
        let final_config: Value =
            serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
        assert_eq!(final_config["format_version"], "spacememory/2");
        assert_eq!(final_config["required_features"], serde_json::json!([]));
        assert_eq!(
            final_config["downstream_unknown"],
            config["downstream_unknown"]
        );
    }
}

#[test]
fn already_open_handle_rechecks_features_changed_by_another_process() {
    let tmp = tempfile::tempdir().unwrap();
    let store_path = tmp.path().join("store");
    let store_s = store_path.to_str().unwrap();
    let init = mem(&[
        "--store",
        store_s,
        "init",
        store_s,
        "--principal",
        "codex_main",
    ]);
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    let added = mem(&[
        "--store",
        store_s,
        "add",
        "memory",
        "--principal",
        "codex_main",
        "--payload",
        r#"{"title":"before-feature-change"}"#,
    ]);
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    let added = json(&added);
    let id = added["id"].as_str().unwrap();
    let token: mem_core::token::Token = added["token"].as_str().unwrap().parse().unwrap();
    let store = mem_core::store::Store::open(&store_path, mem_core::store::OpenMode::Auto).unwrap();
    let types = mem_schema::TypeSet::load_dir(&store.abs("types")).unwrap();
    let rel = mem_core::ops::get(&store, id).unwrap().rel;
    let before_record = std::fs::read(store.abs(&rel)).unwrap();
    let before_events = std::fs::read(store.abs("events.jsonl")).unwrap();
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "child_installs_missing_feature_for_stale_handle_check",
        ])
        .env("SM_FEATURE_CHANGE_STORE", &store_path)
        .stdout(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(child.status.success());

    let get_error = mem_core::ops::get(&store, id).unwrap_err();
    let scan_error = mem_core::ops::scan_all(&store).unwrap_err();
    let event_error = mem_core::event::EventLog::new(&store)
        .read_log()
        .unwrap_err();
    let session_error = mem_core::session::scan_ledgers(&store).unwrap_err();
    for error in [get_error, scan_error, event_error, session_error] {
        assert!(
            matches!(error, mem_core::error::MemError::MissingFeatures(ref features)
            if features == &["record.concurrent-upgrade/v1"]),
            "读取旧句柄必须拒绝未支持特性: {error}"
        );
    }

    let ctx = mem_core::ops::WriteContext::new(&store, &types, "stale-handle")
        .with_committer(None)
        .with_session_id(None);
    let error = mem_core::ops::update(
        &ctx,
        id,
        &token,
        serde_json::json!({"title":"must-not-write"}),
        None,
    )
    .unwrap_err();
    assert!(
        matches!(error, mem_core::error::MemError::MissingFeatures(ref features)
        if features.len() == 1 && features[0] == "record.concurrent-upgrade/v1"),
        "{error}"
    );
    assert_eq!(std::fs::read(store.abs(&rel)).unwrap(), before_record);
    assert_eq!(
        std::fs::read(store.abs("events.jsonl")).unwrap(),
        before_events
    );
}

#[test]
fn cli_write_without_git_is_durable_and_creates_no_pending_commit() {
    let tmp = tempfile::tempdir().unwrap();
    let store = tmp.path().join("store");
    let store_s = store.to_str().unwrap();
    let init = mem(&[
        "--store",
        store_s,
        "init",
        store_s,
        "--principal",
        "codex_main",
    ]);
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    std::fs::remove_dir_all(store.join(".git")).unwrap();

    let added = mem(&[
        "--store",
        store_s,
        "add",
        "memory",
        "--principal",
        "codex_main",
        "--payload",
        r#"{"title":"no-git-durable"}"#,
    ]);
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    let added = json(&added);
    let shown = mem(&["--store", store_s, "show", added["id"].as_str().unwrap()]);
    assert!(shown.status.success());
    assert_eq!(json(&shown)["payload"]["title"], "no-git-durable");
    assert!(
        std::fs::read_dir(store.join(".pending-commits"))
            .unwrap()
            .next()
            .is_none()
    );
}

#[test]
fn failed_git_commit_keeps_writes_and_reconcile_commits_pending_batch() {
    let tmp = tempfile::tempdir().unwrap();
    let store = tmp.path().join("store");
    let store_s = store.to_str().unwrap();
    let init = mem(&[
        "--store",
        store_s,
        "init",
        store_s,
        "--principal",
        "codex_main",
    ]);
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    let first = mem(&[
        "--store",
        store_s,
        "add",
        "memory",
        "--principal",
        "codex_main",
        "--payload",
        r#"{"title":"committed-first"}"#,
    ]);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );

    let git_dir = store.join(".git");
    let git_backup = store.join(".git-valid-backup");
    std::fs::rename(&git_dir, &git_backup).unwrap();
    std::fs::write(&git_dir, b"temporarily unavailable").unwrap();
    let second = mem(&[
        "--store",
        store_s,
        "add",
        "memory",
        "--principal",
        "codex_main",
        "--payload",
        r#"{"title":"pending-second"}"#,
    ]);
    assert!(
        second.status.success(),
        "真相写入成功后提交失败不得回滚: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    let second = json(&second);
    let pending: Vec<_> = std::fs::read_dir(store.join(".pending-commits"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(pending.len(), 1, "失败的 Git 汇总应留下唯一 pending 条目");
    let pending_value: Value =
        serde_json::from_slice(&std::fs::read(&pending[0]).unwrap()).unwrap();
    assert!(
        pending_value["paths"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p == "events.jsonl")
    );

    let first_id = json(&first)["id"].as_str().unwrap().to_owned();
    let first_read = mem(&["--store", store_s, "show", &first_id]);
    let second_read = mem(&["--store", store_s, "show", second["id"].as_str().unwrap()]);
    assert!(
        first_read.status.success() && second_read.status.success(),
        "两笔真相写均应可读"
    );

    std::fs::remove_file(&git_dir).unwrap();
    std::fs::rename(git_backup, &git_dir).unwrap();
    let reconciled = mem(&["--store", store_s, "reconcile"]);
    assert!(
        reconciled.status.success(),
        "{}",
        String::from_utf8_lossy(&reconciled.stderr)
    );
    assert!(
        std::fs::read_dir(store.join(".pending-commits"))
            .unwrap()
            .next()
            .is_none()
    );
    let git_event = Command::new("git")
        .args(["-C", store_s, "show", "HEAD:events.jsonl"])
        .output()
        .unwrap();
    assert!(git_event.status.success());
    let event_text = String::from_utf8_lossy(&git_event.stdout);
    assert!(
        event_text.contains(second["id"].as_str().unwrap()),
        "reconcile 提交了 pending 事件批次"
    );
}

#[test]
fn mcp_process_and_cli_process_share_one_store_contract() {
    use std::io::Write;
    let tmp = tempfile::tempdir().unwrap();
    let store = tmp.path().join("store");
    let store_s = store.to_str().unwrap();
    let init = mem(&[
        "--store",
        store_s,
        "init",
        store_s,
        "--principal",
        "codex_main",
    ]);
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    let cli_added = mem(&[
        "--store",
        store_s,
        "add",
        "memory",
        "--principal",
        "codex_main",
        "--payload",
        r#"{"title":"cli-to-mcp"}"#,
    ]);
    assert!(
        cli_added.status.success(),
        "{}",
        String::from_utf8_lossy(&cli_added.stderr)
    );
    let cli_added = json(&cli_added);

    let mut mcp = Command::new(env!("CARGO_BIN_EXE_mem"))
        .arg("mcp")
        .env("MEM_STORE", store_s)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    {
        let stdin = mcp.stdin.as_mut().unwrap();
        writeln!(
            stdin,
            "{}",
            serde_json::json!({
                "jsonrpc":"2.0", "id":1, "method":"tools/call",
                "params":{"name":"mem_show","arguments":{"id":cli_added["id"]}}
            })
        )
        .unwrap();
        writeln!(stdin, "{}", serde_json::json!({
            "jsonrpc":"2.0", "id":2, "method":"tools/call",
            "params":{"name":"mem_add","arguments":{"type":"memory","principal":"codex_main","payload":{"title":"mcp-to-cli"}}}
        })).unwrap();
    }
    let mcp_output = mcp.wait_with_output().unwrap();
    assert!(
        mcp_output.status.success(),
        "{}",
        String::from_utf8_lossy(&mcp_output.stderr)
    );
    let responses: Vec<Value> = String::from_utf8_lossy(&mcp_output.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(responses.len(), 2);
    let mcp_seen_cli: Value = serde_json::from_str(
        responses[0]["result"]["content"][0]["text"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(mcp_seen_cli["payload"]["title"], "cli-to-mcp");
    let mcp_added: Value = serde_json::from_str(
        responses[1]["result"]["content"][0]["text"]
            .as_str()
            .unwrap(),
    )
    .unwrap();

    let cli_seen_mcp = mem(&[
        "--store",
        store_s,
        "show",
        mcp_added["id"].as_str().unwrap(),
    ]);
    assert!(
        cli_seen_mcp.status.success(),
        "{}",
        String::from_utf8_lossy(&cli_seen_mcp.stderr)
    );
    assert_eq!(json(&cli_seen_mcp)["payload"]["title"], "mcp-to-cli");
}

#[test]
fn migrate_command_preserves_unknown_config_and_rejects_unknown_format_without_probe() {
    let tmp = tempfile::tempdir().unwrap();
    let store = tmp.path().join("store");
    let store_s = store.to_str().unwrap();
    let init = mem(&[
        "--store",
        store_s,
        "init",
        store_s,
        "--principal",
        "codex_main",
    ]);
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );

    let config_path = store.join("mem.json");
    let mut config: Value = serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    config["format_version"] = Value::from(1);
    config["future_extension"] = serde_json::json!({"keep": true});
    config.as_object_mut().unwrap().remove("required_features");
    std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    let migrated = mem(&[
        "--store",
        store_s,
        "migrate",
        "--backup-identity",
        "snapshot-001",
    ]);
    assert!(
        migrated.status.success(),
        "{}",
        String::from_utf8_lossy(&migrated.stderr)
    );
    let migrated_config: Value =
        serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    assert_eq!(migrated_config["format_version"], "spacememory/2");
    assert_eq!(
        migrated_config["future_extension"],
        serde_json::json!({"keep": true})
    );

    migrated_config.as_object().unwrap();
    let mut unknown = migrated_config;
    unknown["format_version"] = Value::from(999);
    std::fs::write(&config_path, serde_json::to_vec(&unknown).unwrap()).unwrap();
    let before = std::fs::read(&config_path).unwrap();
    let probe = store.join(".tmp/probe");
    let _ = std::fs::remove_file(&probe);
    let result = mem(&["--store", store_s, "show", "missing"]);
    assert!(!result.status.success());
    assert!(!probe.exists(), "未知格式拒绝必须早于可写探针");
    assert_eq!(std::fs::read(config_path).unwrap(), before);
}

#[test]
fn cli_and_mcp_expose_structured_compatibility_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let store = tmp.path().join("store");
    let store_s = store.to_str().unwrap();
    let init = mem(&[
        "--store",
        store_s,
        "init",
        store_s,
        "--principal",
        "codex_main",
    ]);
    assert!(init.status.success());

    let config_path = store.join("mem.json");
    let mut config: Value = serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    config["required_features"] = serde_json::json!(["event.future/v9"]);
    std::fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();

    let cli = mem(&["--store", store_s, "reconcile"]);
    assert!(!cli.status.success());
    let cli_error: Value = serde_json::from_slice(&cli.stderr).unwrap();
    assert_eq!(cli_error["machine"]["code"], "missing_features");
    assert_eq!(
        cli_error["machine"]["details"]["features"],
        serde_json::json!(["event.future/v9"])
    );

    let request = serde_json::json!({
        "jsonrpc":"2.0", "id":1, "method":"tools/call",
        "params":{"name":"mem_reconcile", "arguments":{}}
    });
    let response = mem_mcp::handle_request(&store, &request).unwrap();
    assert_eq!(response["error"]["data"]["code"], "missing_features");
    assert_eq!(
        response["error"]["data"]["details"]["features"],
        serde_json::json!(["event.future/v9"])
    );

    config["format_version"] = serde_json::json!(1);
    config.as_object_mut().unwrap().remove("required_features");
    std::fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
    let cli = mem(&["--store", store_s, "list"]);
    let cli_error: Value = serde_json::from_slice(&cli.stderr).unwrap();
    assert_eq!(cli_error["machine"]["code"], "requires_migration");
    assert_eq!(cli_error["machine"]["details"]["current"], "1");
    assert_eq!(cli_error["machine"]["details"]["target"], "spacememory/2");
}

#[test]
fn concurrent_init_processes_publish_one_complete_store() {
    let tmp = tempfile::tempdir().unwrap();
    let store = tmp.path().join("store");
    let store_s = store.to_str().unwrap();
    let spawn = |principal: &'static str| {
        Command::new(env!("CARGO_BIN_EXE_mem"))
            .args([
                "--store",
                store_s,
                "init",
                store_s,
                "--principal",
                principal,
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap()
    };
    let a = spawn("init-a");
    let b = spawn("init-b");
    let a = a.wait_with_output().unwrap();
    let b = b.wait_with_output().unwrap();
    assert_eq!(
        [a.status.success(), b.status.success()]
            .iter()
            .filter(|v| **v)
            .count(),
        1,
        "恰好一个初始化进程完成: A={} B={}",
        String::from_utf8_lossy(&a.stderr),
        String::from_utf8_lossy(&b.stderr)
    );
    let opened = mem(&["--store", store_s, "verify"]);
    assert!(
        opened.status.success(),
        "初始化结果必须完整: {}",
        String::from_utf8_lossy(&opened.stderr)
    );
}

#[test]
#[cfg(debug_assertions)]
fn child_restore_body_pauses_at_feature_durability_window() {
    let (Ok(root), Ok(source), Ok(expect), Ok(ready), Ok(stage)) = (
        std::env::var("SM_RESTORE_FEATURE_STORE"),
        std::env::var("SM_RESTORE_FEATURE_SOURCE"),
        std::env::var("SM_RESTORE_FEATURE_EXPECT"),
        std::env::var("SM_RESTORE_FEATURE_READY"),
        std::env::var("SM_RESTORE_FEATURE_STAGE"),
    ) else {
        return;
    };
    let store =
        mem_core::store::Store::open(std::path::Path::new(&root), mem_core::store::OpenMode::Auto)
            .unwrap();
    let types = mem_schema::TypeSet::load_dir(&store.abs("types")).unwrap();
    let hit = mem_core::ops::scan_all(&store)
        .unwrap()
        .records
        .into_iter()
        .find(|r| r.record.envelope.r#type == "memory")
        .unwrap();
    let expected = expect.parse::<mem_core::token::Token>().unwrap();
    let ready = std::path::PathBuf::from(ready);
    let wanted = stage.clone();
    let hook: mem_core::event::PauseHook = Box::new(move |observed| {
        if observed == wanted {
            std::fs::write(&ready, b"ready").unwrap();
            loop {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    });
    let ctx = mem_core::ops::WriteContext::new(&store, &types, "feature-crash-child");
    let _ = mem_core::ops::restore_body_with_pause_hook_test(
        &ctx,
        &hit.record.envelope.id,
        &expected,
        &mem_core::ops::RestoreSource::File {
            path: source.into(),
        },
        None,
        hook,
    )
    .unwrap();
}

#[test]
#[cfg(debug_assertions)]
fn feature_registration_crash_windows_preserve_conservative_order() {
    use std::time::{Duration, Instant};

    for stage in ["feature_registered", "record_published", "event_appended"] {
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path().join("store");
        let store_s = store_path.to_str().unwrap();
        let init = mem(&[
            "--store",
            store_s,
            "init",
            store_s,
            "--principal",
            "codex_main",
        ]);
        assert!(
            init.status.success(),
            "{}",
            String::from_utf8_lossy(&init.stderr)
        );
        let added = mem(&[
            "--store",
            store_s,
            "add",
            "memory",
            "--principal",
            "codex_main",
            "--payload",
            r#"{"title":"feature-crash"}"#,
        ]);
        assert!(
            added.status.success(),
            "{}",
            String::from_utf8_lossy(&added.stderr)
        );
        let added_json = json(&added);
        let id = added_json["id"].as_str().unwrap();
        let expect = added_json["token"].as_str().unwrap();
        let store =
            mem_core::store::Store::open(&store_path, mem_core::store::OpenMode::Auto).unwrap();
        let record = mem_core::ops::get(&store, id).unwrap();
        let source = tmp.path().join("source.md");
        std::fs::write(&source, std::fs::read(store.abs(&record.rel)).unwrap()).unwrap();
        let original_record = std::fs::read(store.abs(&record.rel)).unwrap();
        let original_events = mem_core::event::EventLog::new(&store)
            .read_log()
            .unwrap()
            .events
            .len();
        let ready = tmp.path().join("restore-ready");
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "child_restore_body_pauses_at_feature_durability_window",
                "--nocapture",
            ])
            .env("SM_RESTORE_FEATURE_STORE", &store_path)
            .env("SM_RESTORE_FEATURE_SOURCE", &source)
            .env("SM_RESTORE_FEATURE_EXPECT", expect)
            .env("SM_RESTORE_FEATURE_READY", &ready)
            .env("SM_RESTORE_FEATURE_STAGE", stage)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready.exists() {
            if let Some(status) = child.try_wait().unwrap() {
                panic!("正文恢复子进程未到达 {stage}: {status}");
            }
            assert!(Instant::now() < deadline, "等待正文恢复阶段 {stage} 超时");
            std::thread::sleep(Duration::from_millis(10));
        }
        child.kill().unwrap();
        let _ = child.wait().unwrap();

        let reopened =
            mem_core::store::Store::open(&store_path, mem_core::store::OpenMode::Auto).unwrap();
        let config = reopened.config();
        assert_eq!(
            config.required_features,
            vec!["event.restore_body/v1"],
            "阶段 {stage}"
        );
        let current = mem_core::ops::get(&reopened, id).unwrap();
        let events = mem_core::event::EventLog::new(&reopened)
            .read_log()
            .unwrap()
            .events;
        match stage {
            "feature_registered" => {
                assert_eq!(
                    std::fs::read(reopened.abs(&current.rel)).unwrap(),
                    original_record
                );
                assert_eq!(events.len(), original_events);
            }
            "record_published" => {
                assert_eq!(
                    current.record.envelope.version,
                    record.record.envelope.version + 1
                );
                assert_eq!(events.len(), original_events);
                let repaired =
                    mem_core::event::reconcile_backfill(&reopened, "after-crash", None).unwrap();
                assert!(!repaired.appended.is_empty());
            }
            "event_appended" => {
                assert_eq!(events.len(), original_events + 1);
                assert_eq!(events.last().unwrap().op, mem_core::event::Op::RestoreBody);
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn child_registers_existing_feature_after_shared_start_signal() {
    let (Ok(root), Ok(start)) = (
        std::env::var("SM_REGISTER_FEATURE_STORE"),
        std::env::var("SM_REGISTER_FEATURE_START"),
    ) else {
        return;
    };
    let store =
        mem_core::store::Store::open(std::path::Path::new(&root), mem_core::store::OpenMode::Auto)
            .unwrap();
    let start = std::path::PathBuf::from(start);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !start.exists() {
        assert!(std::time::Instant::now() < deadline, "并发登记启动信号超时");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    store
        .register_required_feature("event.restore_body/v1")
        .unwrap();
}

#[test]
#[cfg(debug_assertions)]
fn child_add_pauses_after_record_publish_until_released() {
    let (Ok(root), Ok(ready), Ok(release)) = (
        std::env::var("SM_FEATURE_RACE_WRITER_STORE"),
        std::env::var("SM_FEATURE_RACE_WRITER_READY"),
        std::env::var("SM_FEATURE_RACE_WRITER_RELEASE"),
    ) else {
        return;
    };
    let store =
        mem_core::store::Store::open(std::path::Path::new(&root), mem_core::store::OpenMode::Auto)
            .unwrap();
    let types = mem_schema::TypeSet::load_dir(&store.abs("types")).unwrap();
    let ready = std::path::PathBuf::from(ready);
    let release = std::path::PathBuf::from(release);
    let hook: mem_core::event::PauseHook = Box::new(move |stage| {
        if stage != "record_published" {
            return;
        }
        std::fs::write(&ready, b"record-published").unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !release.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "等待父进程释放共享写许可超时"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    });
    let ctx = mem_core::ops::WriteContext::new(&store, &types, "feature-race-writer")
        .with_committer(None)
        .with_session_id(None);
    mem_core::ops::add_with_pause_hook_test(
        &ctx,
        mem_core::ops::AddArgs {
            type_name: "memory".into(),
            payload: serde_json::json!({"title":"feature-race-inflight"}),
            body: b"before-restore".to_vec(),
            principal: Some("codex_main".into()),
        },
        hook,
    )
    .unwrap();
}

#[test]
#[cfg(debug_assertions)]
fn child_restore_body_starts_after_feature_race_writer_is_paused() {
    let (Ok(root), Ok(id), Ok(expect), Ok(source), Ok(started)) = (
        std::env::var("SM_FEATURE_RACE_RESTORE_STORE"),
        std::env::var("SM_FEATURE_RACE_RESTORE_ID"),
        std::env::var("SM_FEATURE_RACE_RESTORE_EXPECT"),
        std::env::var("SM_FEATURE_RACE_RESTORE_SOURCE"),
        std::env::var("SM_FEATURE_RACE_RESTORE_STARTED"),
    ) else {
        return;
    };
    let store =
        mem_core::store::Store::open(std::path::Path::new(&root), mem_core::store::OpenMode::Auto)
            .unwrap();
    let types = mem_schema::TypeSet::load_dir(&store.abs("types")).unwrap();
    std::fs::write(started, b"attempting-restore").unwrap();
    let ctx = mem_core::ops::WriteContext::new(&store, &types, "feature-race-restore")
        .with_committer(None)
        .with_session_id(None);
    mem_core::ops::restore_body(
        &ctx,
        &id,
        &expect.parse().unwrap(),
        &mem_core::ops::RestoreSource::File {
            path: std::path::PathBuf::from(source),
        },
        None,
    )
    .unwrap();
}

#[test]
#[cfg(debug_assertions)]
fn feature_dependent_write_waits_for_inflight_shared_writer_to_finish() {
    let tmp = tempfile::tempdir().unwrap();
    let store_path = tmp.path().join("store");
    let store_s = store_path.to_str().unwrap();
    let init = mem(&[
        "--store",
        store_s,
        "init",
        store_s,
        "--principal",
        "codex_main",
    ]);
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    let store = mem_core::store::Store::open(&store_path, mem_core::store::OpenMode::Auto).unwrap();
    let event_path = store.abs("events.jsonl");
    let events_before = std::fs::read(&event_path).unwrap();
    let config_path = store.abs("mem.json");
    let ready = tmp.path().join("writer-ready");
    let release = tmp.path().join("writer-release");
    let mut writer = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "child_add_pauses_after_record_publish_until_released",
            "--nocapture",
        ])
        .env("SM_FEATURE_RACE_WRITER_STORE", &store_path)
        .env("SM_FEATURE_RACE_WRITER_READY", &ready)
        .env("SM_FEATURE_RACE_WRITER_RELEASE", &release)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !ready.exists() {
        if let Some(status) = writer.try_wait().unwrap() {
            panic!("暂停写者提前退出: {status}");
        }
        assert!(
            std::time::Instant::now() < deadline,
            "写者未到达记录发布暂停点"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }

    let record_path = std::fs::read_dir(store.abs("records/memory"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|ext| ext == "md"))
        .expect("暂停窗口应已有完整记录文件");
    let record_bytes = std::fs::read(&record_path).unwrap();
    let record = mem_core::record::Record::from_bytes(&record_bytes).unwrap();
    let token = mem_core::token::for_bytes(record.envelope.version, &record_bytes);
    let source_path = tmp.path().join("restore-source.md");
    std::fs::write(
        &source_path,
        mem_core::record::serialize_new(
            &record.envelope,
            &record.payload,
            b"restored-after-shared-write",
        ),
    )
    .unwrap();

    let started = tmp.path().join("restore-started");
    let mut restore = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "child_restore_body_starts_after_feature_race_writer_is_paused",
            "--nocapture",
        ])
        .env("SM_FEATURE_RACE_RESTORE_STORE", &store_path)
        .env("SM_FEATURE_RACE_RESTORE_ID", &record.envelope.id)
        .env("SM_FEATURE_RACE_RESTORE_EXPECT", token.to_string())
        .env("SM_FEATURE_RACE_RESTORE_SOURCE", &source_path)
        .env("SM_FEATURE_RACE_RESTORE_STARTED", &started)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !started.exists() {
        if let Some(status) = restore.try_wait().unwrap() {
            panic!("恢复进程未进入调用便退出: {status}");
        }
        assert!(
            std::time::Instant::now() < deadline,
            "恢复进程未进入尝试窗口"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert!(
        restore.try_wait().unwrap().is_none(),
        "恢复写必须等待在途共享写完成"
    );
    let config: Value = serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    assert_eq!(config["required_features"], serde_json::json!([]));
    assert_eq!(std::fs::read(&event_path).unwrap(), events_before);

    std::fs::write(&release, b"continue").unwrap();
    let wait_bounded = |child: &mut std::process::Child, what: &str| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                return status;
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("{what} 超过 10 秒恢复期限");
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    };
    let writer_status = wait_bounded(&mut writer, "共享写者");
    assert!(writer_status.success(), "共享写者失败: {writer_status}");
    let restore_status = wait_bounded(&mut restore, "特性依赖写者");
    assert!(
        restore_status.success(),
        "特性依赖写者失败: {restore_status}"
    );

    let final_config: Value = serde_json::from_slice(&std::fs::read(config_path).unwrap()).unwrap();
    assert_eq!(
        final_config["required_features"],
        serde_json::json!(["event.restore_body/v1"])
    );
    let reopened =
        mem_core::store::Store::open(&store_path, mem_core::store::OpenMode::Auto).unwrap();
    let restored = mem_core::ops::get(&reopened, &record.envelope.id).unwrap();
    assert_eq!(restored.record.body, b"restored-after-shared-write");
    let events = mem_core::event::EventLog::new(&reopened)
        .read_log()
        .unwrap()
        .events
        .into_iter()
        .filter(|event| event.id == record.envelope.id)
        .map(|event| event.op)
        .collect::<Vec<_>>();
    assert_eq!(
        events,
        vec![
            mem_core::event::Op::Create,
            mem_core::event::Op::RestoreBody
        ]
    );
}

#[test]
fn two_processes_concurrently_register_same_supported_feature_idempotently() {
    let tmp = tempfile::tempdir().unwrap();
    let store_path = tmp.path().join("store");
    let store_s = store_path.to_str().unwrap();
    let init = mem(&[
        "--store",
        store_s,
        "init",
        store_s,
        "--principal",
        "codex_main",
    ]);
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    let start = tmp.path().join("start-registration");
    let spawn = || {
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "child_registers_existing_feature_after_shared_start_signal",
                "--nocapture",
            ])
            .env("SM_REGISTER_FEATURE_STORE", &store_path)
            .env("SM_REGISTER_FEATURE_START", &start)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap()
    };
    let a = spawn();
    let b = spawn();
    std::fs::write(&start, b"go").unwrap();
    let a = a.wait_with_output().unwrap();
    let b = b.wait_with_output().unwrap();
    assert!(
        a.status.success(),
        "A: {}",
        String::from_utf8_lossy(&a.stderr)
    );
    assert!(
        b.status.success(),
        "B: {}",
        String::from_utf8_lossy(&b.stderr)
    );
    let config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(store_path.join("mem.json")).unwrap()).unwrap();
    assert_eq!(
        config["required_features"],
        serde_json::json!(["event.restore_body/v1"])
    );
}
