//! §8-5 CLI e2e 测试：init → session start → add/update/show/diff → flush 闭环，
//! brief 纯读、幂等 start、冲突退出码、无 git 形态（§3.3 命令面 / D13/D14 闭环）。

use assert_cmd::Command;
use serde_json::Value;

fn mem() -> Command {
    Command::cargo_bin("mem").expect("mem 二进制")
}

fn init_store(dir: &std::path::Path) -> Value {
    let out = mem()
        .args(["--store", dir.to_str().unwrap(), "init", dir.to_str().unwrap(), "--principal", "codex_main"])
        .output()
        .unwrap();
    assert!(out.status.success(), "init 失败：{}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).unwrap()
}

fn j(out: &std::process::Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!("输出不是 JSON：{e}\n{}", String::from_utf8_lossy(&out.stdout))
    })
}

#[test]
fn init_creates_store_and_bootstraps_principal() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    let v = init_store(&dir);
    assert_eq!(v["ok"], true);
    assert!(dir.join("mem.json").is_file());
    assert!(dir.join("types/task/schema.json").is_file(), "播种类型两件套");
    assert!(dir.join("conventions/CONVENTIONS.md").is_file());
    // 自举 principal 记录存在（红线13：经类型声明引入）。
    let principals = std::fs::read_dir(dir.join("records/principal"))
        .unwrap()
        .filter(|e| {
            e.as_ref().unwrap().path().extension().map(|x| x == "md").unwrap_or(false)
        })
        .count();
    assert_eq!(principals, 1, "自举 principal 恰一条（锁文件不计）");
    // 二次 init 响亮拒绝（单店单副本）。
    let out = mem()
        .args(["--store", dir.to_str().unwrap(), "init", dir.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!out.status.success());
}

#[test]
fn init_quarantines_and_preserves_an_interrupted_previous_attempt() {
    let tmp = tempfile::tempdir().unwrap();
    let store = tmp.path().join("store");
    let path = store.to_str().unwrap();
    let first = mem().args(["--store", path, "init", path, "--principal", "first-owner"]).output().unwrap();
    assert!(first.status.success(), "{}", String::from_utf8_lossy(&first.stderr));

    let marker = store.join(".init-in-progress");
    std::fs::write(&marker, b"interrupted").unwrap();
    let sentinel = store.join("records/keep-during-recovery.bin");
    std::fs::write(&sentinel, b"preserve incomplete store for diagnosis").unwrap();

    let recovered = mem().args(["--store", path, "init", path, "--principal", "second-owner"]).output().unwrap();
    assert!(recovered.status.success(), "{}", String::from_utf8_lossy(&recovered.stderr));
    let recovered_json: serde_json::Value = serde_json::from_slice(&recovered.stdout).unwrap();
    let quarantine = recovered_json["quarantined_incomplete_init"].as_str().unwrap();
    assert_eq!(std::fs::read(std::path::Path::new(quarantine).join("records/keep-during-recovery.bin")).unwrap(),
        b"preserve incomplete store for diagnosis");
    assert!(!marker.exists(), "新店发布前须清理中断标记");
    let verify = mem().args(["--store", path, "verify"]).output().unwrap();
    assert!(verify.status.success(), "新店应完整可用: {}", String::from_utf8_lossy(&verify.stderr));
}

#[test]
fn session_start_idempotent_and_flush() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    init_store(&dir);
    let s = dir.to_str().unwrap();
    let out = mem().args(["--store", s, "session", "start", "--principal", "codex_main"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let v1 = j(&out);
    // 幂等：同键再次 start → 同一台账。
    let out2 = mem().args(["--store", s, "session", "start", "--principal", "codex_main"]).output().unwrap();
    let v2 = j(&out2);
    assert_eq!(v1["session"], v2["session"]);
    assert_eq!(v2["idempotent"], true);
    let session_id = v1["session"].as_str().unwrap().to_string();
    // flush → flushed 可接续。
    let out3 = mem()
        .args(["--store", s, "session", "flush", "--session", &session_id, "--trail", "id-1"])
        .output()
        .unwrap();
    let v3 = j(&out3);
    assert_eq!(v3["status"], "flushed");
    assert_eq!(v3["read_trail"], serde_json::json!(["id-1"]));
    // 接手：新台账 + handoff_from。
    let out4 = mem()
        .args(["--store", s, "session", "start", "--principal", "codex_main", "--handoff", "auto"])
        .output()
        .unwrap();
    let v4 = j(&out4);
    assert_eq!(v4["handoff_from"], serde_json::json!(session_id));
    assert_ne!(v4["session"], serde_json::json!(session_id), "接手 = 新台账（R9.6）");
}

#[test]
fn brief_never_creates_ledger_and_reports_honestly() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    init_store(&dir);
    let s = dir.to_str().unwrap();
    // 无台账：如实报告（红线25：brief 纯读）。
    let out = mem().args(["--store", s, "brief"]).output().unwrap();
    let v = j(&out);
    assert!(v["identity"].is_null());
    assert!(v["identity_note"].as_str().unwrap().contains("无台账"));
    assert!(v["read_mode"].is_object(), "直读/索引标注");
    // sessions/ 仍无台账文件。
    let count = std::fs::read_dir(dir.join("sessions")).unwrap().count();
    assert_eq!(count, 0, "brief 不建台账");
    // 有台账后：identity 块出现。
    mem().args(["--store", s, "session", "start", "--principal", "codex_main"]).output().unwrap();
    let out2 = mem().args(["--store", s, "brief"]).output().unwrap();
    let v2 = j(&out2);
    assert!(v2["identity"].is_object());
    // brief 后 sessions/ 数量不变（仍 1 个台账）。
    let count2 = std::fs::read_dir(dir.join("sessions"))
        .unwrap()
        .filter(|e| e.as_ref().unwrap().path().extension().map(|x| x == "json").unwrap_or(false))
        .count();
    assert_eq!(count2, 1);
}

#[test]
fn add_show_update_conflict_flow() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    init_store(&dir);
    let s = dir.to_str().unwrap();
    // add（project 目标不存在 → x-ref 拒收，R8.5）。
    let out = mem()
        .args(["--store", s, "add", "task",
"--principal",
"codex_main",
            "--payload", r#"{"project":"0198ghost-0000-7cd2-9e3f-000000000001","slug":"t1","title":"T","status":"todo"}"#])
        .output()
        .unwrap();
    assert!(!out.status.success(), "引用目标不存在必须拒收");
    // 建 project 记录（principal 已由 init 自举；project 手工建）。
    let out_p = mem()
        .args(["--store", s, "add", "project",
"--principal",
"codex_main",
            "--payload", r#"{"slug":"proj","title":"Proj","status":"active"}"#])
        .output()
        .unwrap();
    assert!(out_p.status.success());
    let proj = j(&out_p);
    let project_id = proj["id"].as_str().unwrap().to_string();
    // add task（合法）。
    let payload = format!(
        r#"{{"project":"{project_id}","slug":"login-api","title":"实现登录接口","status":"todo"}}"#
    );
    let out2 = mem()
        .args(["--store", s, "add", "task",
"--principal",
"codex_main", "--payload", &payload])
        .output()
        .unwrap();
    assert!(out2.status.success(), "{}", String::from_utf8_lossy(&out2.stderr));
    let v = j(&out2);
    assert_eq!(v["created"], true);
    let id = v["id"].as_str().unwrap().to_string();
    let token1 = v["token"].as_str().unwrap().to_string();
    // show 返回令牌 + 正文。
    let out3 = mem().args(["--store", s, "show", &id]).output().unwrap();
    let v3 = j(&out3);
    assert_eq!(v3["token"], serde_json::json!(token1));
    // update 带令牌 → v2。
    let payload2 = format!(
        r#"{{"project":"{project_id}","slug":"login-api","title":"实现登录接口","status":"in_progress"}}"#
    );
    let out4 = mem()
        .args(["--store", s, "update", &id, "--expect", &token1, "--payload", &payload2])
        .output()
        .unwrap();
    assert!(out4.status.success(), "{}", String::from_utf8_lossy(&out4.stderr));
    let v4 = j(&out4);
    assert_eq!(v4["version"], 2);
    // 陈旧令牌 → 退出码 2（结构化冲突）。
    let out5 = mem()
        .args(["--store", s, "update", &id, "--expect", &token1, "--payload", &payload2])
        .output()
        .unwrap();
    assert_eq!(out5.status.code(), Some(2), "冲突独立退出码（D3）");
    let err: Value = serde_json::from_slice(&out5.stderr).unwrap();
    assert_eq!(err["conflict"], true);
    // diff：principal(init) + project + task 各一组；task 组含 create+update。
    let out6 = mem().args(["--store", s, "diff", "--since", "0"]).output().unwrap();
    let v6 = j(&out6);
    let groups = v6["changes_since"].as_array().unwrap();
    assert_eq!(groups.len(), 3, "按记录去重：3 条记录 3 组");
    let task_group = groups.iter().find(|g| g["id"] == serde_json::json!(id)).unwrap();
    assert_eq!(task_group["history_len"], 2, "create + update");
}

#[test]
fn verify_ok_and_history_hole_detection() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    init_store(&dir);
    let s = dir.to_str().unwrap();
    // R11/BD5 ①④：行尾规则随 init 播种、随**首次受管提交**入树（提交树核验 =
    // 完成标记）。init 后、首次受管写入前，verify 如实报告该窗口（gitattributes_audit
    // 非空 → ok=false）；一次受管写入后四件套闭合、全绿。
    let pre = mem().args(["--store", s, "verify", "--deep"]).output().unwrap();
    let pre_v = j(&pre);
    assert_eq!(pre_v["gitattributes_audit"].as_array().unwrap().len(), 1, "init 后窗口如实报告：{pre_v}");
    assert_eq!(pre_v["ok"], false, "窗口内不宣称完成：{pre_v}");
    let seal = mem()
        .args(["--store", s, "add", "memory",
"--principal",
"codex_main", "--payload", r#"{"title":"seal"}"#])
        .output()
        .unwrap();
    assert!(seal.status.success(), "{}", String::from_utf8_lossy(&seal.stderr));
    let out = mem().args(["--store", s, "verify", "--deep"]).output().unwrap();
    let v = j(&out);
    assert_eq!(v["gitattributes_audit"], serde_json::json!([]), "受管写入后提交树核验通过：{v}");
    assert_eq!(v["ok"], true, "{}", serde_json::to_string(&v).unwrap());
    // 注入历史空洞：伪造 update（old_v=5 跳过 v2..4）。
    let log = dir.join("events.jsonl");
    let mut content = std::fs::read_to_string(&log).unwrap();
    content.push_str(r#"{"by":"fake","id":"0198fake-0000-7cd2-9e3f-000000000001","kind":"op","new_digest":"d","new_v":6,"old_digest":"c","old_v":5,"op":"update","path":"records/task/x.md","seq":999,"ts":"2026-11-04T00:00:00Z"}"#);
    content.push('\n');
    std::fs::write(&log, content).unwrap();
    let out2 = mem().args(["--store", s, "verify"]).output().unwrap();
    let v2 = j(&out2);
    assert_eq!(v2["ok"], false);
    assert!(!v2["history_holes"].as_array().unwrap().is_empty(), "链空洞显式报告（红线24）");
}

#[test]
fn readonly_store_read_commands_work_writes_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    init_store(&dir);
    let s = dir.to_str().unwrap();
    // 用 Windows 只读属性模拟（目录只读不拦创建——改用 ForceReadOnly 不可从 CLI 设；
    // 这里验证自然探测：把 .tmp 换成普通文件 → open 探测失败 → 只读形态）。
    std::fs::remove_dir_all(dir.join(".tmp")).unwrap();
    std::fs::write(dir.join(".tmp"), b"not a dir").unwrap();
    // 五读命令可用（§3.4 跨档行）。
    for args in [
        vec!["brief"],
        vec!["list"],
        vec!["diff", "--since", "0"],
        vec!["verify"],
    ] {
        let out = mem().args(["--store", s]).args(&args).output().unwrap();
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }
    // 写命令结构化拒绝。
    let out = mem()
        .args(["--store", s, "add", "memory",
"--principal",
"codex_main", "--payload", r#"{"title":"x"}"#])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("只读"));
}

#[test]
fn trace_and_reconcile_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    init_store(&dir);
    let s = dir.to_str().unwrap();
    // 两条记录 + link。
    let out_p = mem()
        .args(["--store", s, "add", "project",
"--principal",
"codex_main", "--payload", r#"{"slug":"proj","title":"P","status":"active"}"#])
        .output()
        .unwrap();
    let project_id = j(&out_p)["id"].as_str().unwrap().to_string();
    let out_t = mem()
        .args(["--store", s, "add", "task",
"--principal",
"codex_main", "--payload",
            &format!(r#"{{"project":"{project_id}","slug":"t-a","title":"A","status":"todo"}}"#)])
        .output()
        .unwrap();
    let task_id = j(&out_t)["id"].as_str().unwrap().to_string();
    let task_token = j(&out_t)["token"].as_str().unwrap().to_string();
    let out_m = mem()
        .args(["--store", s, "add", "memory",
"--principal",
"codex_main", "--payload", r#"{"title":"M"}"#])
        .output()
        .unwrap();
    let mem_id = j(&out_m)["id"].as_str().unwrap().to_string();
    let mem_token = j(&out_m)["token"].as_str().unwrap().to_string();
    // task link memory（task 派生自 memory 证据）。
    let out_l = mem()
        .args(["--store", s, "link", &task_id, &mem_id, "--rel", "derived_from",
            "--expect-a", &task_token, "--expect-b", &mem_token])
        .output()
        .unwrap();
    assert!(out_l.status.success(), "{}", String::from_utf8_lossy(&out_l.stderr));
    // trace down：从 memory 找到依赖者 task。
    let out_tr = mem()
        .args(["--store", s, "trace", &mem_id, "--dir", "down"])
        .output()
        .unwrap();
    let v = j(&out_tr);
    let ids: Vec<&str> = v["nodes"].as_array().unwrap().iter().map(|n| n["id"].as_str().unwrap()).collect();
    assert!(ids.contains(&task_id.as_str()), "{v}");
    // reconcile：幂等（全部稳定）。
    let out_r = mem().args(["--store", s, "reconcile"]).output().unwrap();
    let v_r = j(&out_r);
    assert_eq!(v_r["backfill_appended"], 0, "全稳定：无回填");
}

#[test]
fn cli_reconcile_refuses_missing_feature_without_persisting_index_or_events() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    init_store(&dir);
    let store = mem_core::store::Store::open(&dir, mem_core::store::OpenMode::Auto).unwrap();
    let mut idx = mem_index::Index::open_store(&store).unwrap();
    idx.rebuild(&store).unwrap();
    drop(idx);
    let index_path = dir.join("index/index.sqlite");
    let before_index = std::fs::read(&index_path).unwrap();
    let event_path = dir.join("events.jsonl");
    let before_events = std::fs::read(&event_path).unwrap();
    let pending_dir = dir.join(".pending-commits");
    std::fs::write(pending_dir.join("reconcile.json"), br#"{"paths":["events.jsonl"],"ts":"2026-11-22T00:00:00Z","error":"fixture"}"#).unwrap();
    let before_pending = snapshot_files(&pending_dir);
    let checkpoint = dir.join("index/checkpoint.json");
    std::fs::write(&checkpoint, b"checkpoint fixture").unwrap();
    let before_index_tree = snapshot_files(&dir.join("index"));
    let config_path = dir.join("mem.json");
    let mut config: Value = serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    config["required_features"] = serde_json::json!(["record.future/v1"]);
    std::fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();

    let output = mem().args(["--store", dir.to_str().unwrap(), "reconcile"]).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("record.future/v1"));
    assert_eq!(std::fs::read(index_path).unwrap(), before_index);
    assert_eq!(std::fs::read(event_path).unwrap(), before_events);
    assert_eq!(snapshot_files(&pending_dir), before_pending);
    assert_eq!(snapshot_files(&dir.join("index")), before_index_tree);
}

fn snapshot_files(dir: &std::path::Path) -> std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> {
    std::fs::read_dir(dir).unwrap().map(|entry| {
        let path = entry.unwrap().path();
        (path.file_name().unwrap().into(), std::fs::read(&path).unwrap())
    }).collect()
}

#[test]
fn cli_migrate_legacy_store_reports_backup_and_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    mem_core::store::Store::create(&dir).unwrap();
    let config_path = dir.join("mem.json");
    let mut config: Value = serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    config["format_version"] = serde_json::json!(1);
    config.as_object_mut().unwrap().remove("required_features");
    std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();

    let first = mem().args(["--store", dir.to_str().unwrap(), "migrate", "--backup-identity", "backup-2026-11-22"]).output().unwrap();
    assert!(first.status.success(), "{}", String::from_utf8_lossy(&first.stderr));
    let first_json = j(&first);
    assert_eq!(first_json["status"], "migrated");
    assert_eq!(first_json["backup_identity"], "backup-2026-11-22");

    let second = mem().args(["--store", dir.to_str().unwrap(), "migrate", "--backup-identity", "backup-2026-11-22"]).output().unwrap();
    assert!(second.status.success(), "{}", String::from_utf8_lossy(&second.stderr));
    assert_eq!(j(&second)["status"], "already_migrated");
}


// ---------- 写路径归属绑定（方案 principal-binding §4.2 双端 e2e） ----------

#[test]
fn principal_binding_explicit_inherit_reject_three_modes() {
    let dir = tempfile::tempdir().unwrap();
    let s = dir.path().join("store").to_str().unwrap().to_string();
    init_store(std::path::Path::new(&s));
    let gitattrs = std::path::Path::new(&s).join(".gitattributes");

    // 显式态：--principal 短名 → 成功。
    let out_e = mem()
        .args(["--store", &s, "add", "memory", "--principal", "codex_main",
               "--payload", r#"{"title":"显式"}"#])
        .output()
        .unwrap();
    assert!(out_e.status.success(), "{}", String::from_utf8_lossy(&out_e.stderr));

    // 继承态：session start → add 只带 --session → 归属继承。
    let out_ss = mem()
        .args(["--store", &s, "session", "start", "--principal", "codex_main"])
        .output()
        .unwrap();
    assert!(out_ss.status.success(), "{}", String::from_utf8_lossy(&out_ss.stderr));
    let sid = j(&out_ss)["session"].as_str().unwrap().to_string();
    let out_i = mem()
        .args(["--store", &s, "add", "memory", "--session", &sid,
               "--payload", r#"{"title":"继承"}"#])
        .output()
        .unwrap();
    assert!(out_i.status.success(), "{}", String::from_utf8_lossy(&out_i.stderr));
    let rel_i = j(&out_i)["rel"].as_str().unwrap().to_string();
    let content = std::fs::read_to_string(std::path::Path::new(&s).join(&rel_i)).unwrap();
    assert!(
        content.contains("\"principal\": \"") && !content.contains("\"principal\": null"),
        "台账继承落信封（非空 uuid）：{content}"
    );

    // 拒绝态：无 --principal 无 --session → 响亮拒绝且全店文件树一致（含 .gitattributes 未被创建）。
    // 反例前置：此刻移除 .gitattributes（显式/继承态的合法写路径已重建过它），
    // 断言拒绝路径不重建。净零基线 = 拒绝调用之前的店态。
    if gitattrs.exists() {
        std::fs::remove_file(&gitattrs).unwrap();
    }
    let before = cli_truth_snapshot(std::path::Path::new(&s));
    let out_r = mem()
        .args(["--store", &s, "add", "memory", "--payload", r#"{"title":"拒绝"}"#])
        .output()
        .unwrap();
    assert!(!out_r.status.success(), "无归属创建必须失败");
    let err = String::from_utf8_lossy(&out_r.stderr);
    assert!(err.contains("归属缺失"), "{err}");
    let after = cli_truth_snapshot(std::path::Path::new(&s));
    assert_eq!(before, after, "拒绝路径全店文件树一致");
    assert!(!gitattrs.exists(), "写前核验不得先于预检");
}

fn cli_truth_snapshot(root: &std::path::Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    fn walk(dir: &std::path::Path, rel: &str, out: &mut std::collections::BTreeMap<String, Vec<u8>>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let name = e.file_name().to_string_lossy().to_string();
            let p = e.path();
            let r = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
            if p.is_dir() {
                if r == ".git" || r == "index" || r.starts_with("index/") || r == "sessions" {
                    continue;
                }
                walk(&p, &r, out);
            } else {
                if r.ends_with(".lock") || r == "sidecar-state.json" {
                    continue;
                }
                out.insert(r, std::fs::read(&p).unwrap());
            }
        }
    }
    let mut out = std::collections::BTreeMap::new();
    walk(root, "", &mut out);
    out
}
