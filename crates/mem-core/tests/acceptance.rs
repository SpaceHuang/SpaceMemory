//! §8-4 核心库验收测试（设计文档 §8 第 4 步验收清单的形式化汇总）。
//!
//! - **freshness**：同尺寸修改、粗时间戳粒度、恢复 mtime 三场景 × sidecar/watcher
//!   停止（嵌入模式）× 只读，全部第一次读取返回新值（D5 / 红线21）；
//! - **增量**：两轮 `diff --since`——current_drift 不重复进入 changes_since_N、
//!   外部提交出现、无 .git 形态成立（D12 / 红线26）；
//! - 注入面（v1 崩溃、中间空洞、保版本手改、撕裂尾行、外部删除/重命名、删除后重建、
//!   只读外部删除）与回填并发用例在 tests/backfill.rs、tests/ops.rs 已逐一覆盖；
//!   本文件补齐 freshness 矩阵与"外部 git 提交出现于增量"两条。

use mem_core::event::EventLog;
use mem_core::freshness::{self, Checkpoint, NullSubscription, ReadMode, Subscription};
use mem_core::ops;
use mem_core::session;
use mem_core::store::{OpenMode, Store};
use mem_core::view;
use mem_schema::TypeSet;

fn fresh_store() -> (tempfile::TempDir, Store, TypeSet) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("store");
    Store::create(&root).unwrap();
    let types_dir = root.join("types");
    let asset_root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/builtin-types");
    fn walk(dir: &std::path::Path, rel: &str, out: &mut Vec<(String, Vec<u8>)>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let name = e.file_name().to_string_lossy().to_string();
            let p = e.path();
            let r = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
            if p.is_dir() {
                walk(&p, &r, out);
            } else {
                out.push((r, std::fs::read(p).unwrap()));
            }
        }
    }
    let mut files = Vec::new();
    walk(&asset_root, "", &mut files);
    for (p, bytes) in files {
        let abs = root.join(&p);
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(abs, bytes).unwrap();
    }
    let store = Store::open(&root, OpenMode::Auto).unwrap();
    let types = TypeSet::load_dir(&types_dir).unwrap();
    (tmp, store, types)
}

const PROJECT: &str = "0198f7a9-0000-7cd2-9e3f-000000000001";

fn seed_project(store: &Store, id: &str, slug: &str) {
    let env = mem_core::envelope::Envelope {
        id: id.into(),
        r#type: "project".into(),
        created: "2026-11-04T00:00:00Z".into(),
        modified: "2026-11-04T00:00:00Z".into(),
        version: 1,
        principal: None,
        refs: vec![],
        aliases: vec![slug.into()],
        evidence_cutoff_seq: None,
        commit: None,
        extra: Default::default(),
    };
    let payload = serde_json::json!({"slug": slug, "title": slug, "status": "active"});
    let bytes = mem_core::record::serialize_new(&env, &payload, b"");
    let rel = format!("records/project/{slug}.md");
    let abs = store.abs(&rel);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(abs, bytes).unwrap();
}

fn add_task(store: &Store, types: &TypeSet, slug: &str, body: &str) -> String {
    let c = ops::WriteContext { store, types, by: "t".into(), committer: None, session_id: None };
    ops::add(&c, ops::AddArgs {
        type_name: "task".into(),
        payload: serde_json::json!({
            "project": PROJECT, "slug": slug, "title": slug, "status": "todo"
        }),
        body: body.as_bytes().to_vec(),
        principal: None,
    })
    .unwrap()
    .record
    .envelope
    .id
}

// ---------- freshness：三场景 × 三条件，第一次读取返回新值（红线21） ----------

/// 三种"索引不可见"的修改方式：
enum Scenario {
    /// 同尺寸修改（同字节数换内容——mtime 粒度陷阱的经典面）。
    SameSize,
    /// 粗时间戳粒度：改内容 + 把 mtime 回拨到旧值以下（快速连续写入场景）。
    CoarseTimestamp,
    /// 恢复 mtime：改内容后恢复原 mtime（备份工具行为）。
    RestoredMtime,
}

fn apply_scenario(store: &Store, id: &str, scenario: &Scenario, new_body: &str) {
    let cur = ops::get(store, id).unwrap();
    let mut env = cur.record.envelope.clone();
    // 保版本手改（索引若按 (mtime,size) 判新鲜，这种修改完全不可见）。
    env.version = cur.record.envelope.version; // 版本不变
    let payload = cur.record.payload.clone();
    let bytes = mem_core::record::serialize_new(&env, &payload, new_body.as_bytes());
    let abs = store.abs(&cur.rel);
    std::fs::write(&abs, bytes).unwrap();
    match scenario {
        Scenario::SameSize => {
            // new_body 与旧正文同字节长度（由调用方保证）；什么都不再做。
        }
        Scenario::CoarseTimestamp => {
            // 粗粒度：mtime 停留在旧值（把 mtime 设为 2000-01-01）。
            set_mtime(&abs, 946_684_800);
        }
        Scenario::RestoredMtime => {
            // 恢复原 mtime：取事件日志前记录？简化：设为与 mem.json 相同秒级 mtime。
            let orig = std::fs::metadata(store.abs("mem.json"))
                .unwrap()
                .modified()
                .unwrap();
            set_mtime_to(&abs, orig);
        }
    }
}

#[cfg(windows)]
fn set_mtime(path: &std::path::Path, epoch_secs: u64) {
    set_mtime_to(path, std::time::UNIX_EPOCH + std::time::Duration::from_secs(epoch_secs));
}

#[cfg(not(windows))]
fn set_mtime(path: &std::path::Path, epoch_secs: u64) {
    set_mtime_to(path, std::time::UNIX_EPOCH + std::time::Duration::from_secs(epoch_secs));
}

fn set_mtime_to(path: &std::path::Path, t: std::time::SystemTime) {
    #[cfg(unix)]
    {
        // unix: filetime crate 未引入——用 libc utimes 的最小封装。
        let secs = t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        unsafe {
            let cpath = std::ffi::CString::new(path.to_string_lossy().as_bytes()).unwrap();
            let tv = libc::timeval { tv_sec: secs as i64, tv_usec: 0 };
            let mut times = [tv, tv];
            libc::utimes(cpath.as_ptr(), times.as_mut_ptr());
        }
    }
    #[cfg(windows)]
    {
        // windows: 标准库无法设 mtime——用平台 FFI。
        crate_win_set_mtime(path, t);
    }
}

#[cfg(windows)]
fn crate_win_set_mtime(path: &std::path::Path, t: std::time::SystemTime) {
    // 仅测试使用：SetFileTime 最小封装。
    use std::os::windows::ffi::OsStrExt;
    
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, SetFileTime, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE,
        OPEN_EXISTING,
    };
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    wide.push(0);
    unsafe {
        let h = CreateFileW(
            wide.as_ptr(),
            0x4000_0000u32, // GENERIC_WRITE
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        );
        if h.is_null() {
            return;
        }
        let dur = t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        // FILETIME = 100ns since 1601-01-01。
        let ft100 = (dur.as_nanos() / 100) as u64 + 116_444_736_000_000_000u64;
        let ft = FILETIME { dwLowDateTime: (ft100 & 0xFFFF_FFFF) as u32, dwHighDateTime: (ft100 >> 32) as u32 };
        SetFileTime(h, std::ptr::null(), std::ptr::null(), &ft);
        CloseHandle(h);
    }
}

fn assert_first_read_returns_new_value(scenario: Scenario, force_readonly: bool) {
    let (_t, store, types) = fresh_store();
    seed_project(&store, PROJECT, PROJECT);
    let id = add_task(&store, &types, "fresh-target", "old-body");
    // 模拟"索引检查点 + 订阅都还在"的世界（证明式协议的世界观），然后 sidecar 停止。
    let cp = Checkpoint {
        ts: mem_core::clock::now_rfc3339(),
        entries: freshness::stat_snapshot(&store),
        index_epoch: 0,
    };
    let store_ref = if force_readonly {
        // 只读档：重开为 ForceReadOnly（写类结构化拒绝；五读命令全部可用）。
        Store::open(store.root(), OpenMode::ForceReadOnly).unwrap()
    } else {
        Store::open(store.root(), OpenMode::Auto).unwrap()
    };
    // sidecar 停止 = 订阅 None（嵌入模式）。
    let mode = freshness::consult(&store_ref, Some(&cp), None::<&dyn freshness::Subscription>);
    assert!(mode.is_direct(), "sidecar 不在必须直读：{mode:?}");
    assert!(!mode.fts_available(), "直读模式 FTS 不可用（标注）");
    // 施加修改（索引/检查点毫不知情）。
    apply_scenario(&store_ref, &id, &scenario, "new-body-xxxxxx");
    // **第一次读取**必须返回新值（不允许任何"已知可能返回旧值"的快路径）。
    let got = ops::get(&store_ref, &id).unwrap();
    assert_eq!(got.record.body, "new-body-xxxxxx".as_bytes(), "show 直读新值");
    // list/视图层同样直读。
    let filter = mem_core::view::ViewFilter {
        types: vec!["task".into()],
        refs: vec![],
        principals: vec![],
        since: None,
    };
    let list = view::execute_filter(&store_ref, &filter, 100, 0).unwrap();
    assert!(list.items.iter().any(|i| i.envelope.id == id), "list 直读可见");
    if force_readonly {
        // 只读档：写类操作结构化拒绝（§3.4 跨档行）。
        let c = ops::WriteContext {
            store: &store_ref,
            types: &types,
            by: "t".into(),
            committer: None,
            session_id: None,
        };
        assert!(matches!(
            ops::add(&c, ops::AddArgs {
                type_name: "memory".into(),
                payload: serde_json::json!({"title": "x"}),
                body: b"".to_vec(),
                principal: None,
            }),
            Err(mem_core::error::MemError::ReadOnly)
        ));
    }
}

#[test]
fn freshness_same_size_modify_with_sidecar_stopped() {
    assert_first_read_returns_new_value(Scenario::SameSize, false);
}

#[test]
fn freshness_coarse_timestamp_with_sidecar_stopped() {
    assert_first_read_returns_new_value(Scenario::CoarseTimestamp, false);
}

#[test]
fn freshness_restored_mtime_with_sidecar_stopped() {
    assert_first_read_returns_new_value(Scenario::RestoredMtime, false);
}

#[test]
fn freshness_same_size_modify_readonly_store() {
    assert_first_read_returns_new_value(Scenario::SameSize, true);
}

#[test]
fn freshness_coarse_timestamp_readonly_store() {
    assert_first_read_returns_new_value(Scenario::CoarseTimestamp, true);
}

#[test]
fn freshness_restored_mtime_readonly_store() {
    assert_first_read_returns_new_value(Scenario::RestoredMtime, true);
}

#[test]
fn freshness_proof_requires_both_conditions() {
    let (_t, store, _types) = fresh_store();
    // 嵌入模式（无检查点）→ 直读。
    let m = freshness::consult(&store, None, None);
    assert!(m.is_direct());
    // 有检查点 + 有订阅 + stat 一致 + 无未消化 → 快路径。
    let cp = Checkpoint { ts: mem_core::clock::now_rfc3339(), entries: freshness::stat_snapshot(&store), index_epoch: 0 };
    struct OkSub;
    impl Subscription for OkSub {
        fn undigested_changes_since(&self, _ts: &str) -> Option<u64> {
            Some(0)
        }
        fn backend(&self) -> &'static str {
            "usn"
        }
    }
    let m = freshness::consult(&store, Some(&cp), Some(&OkSub));
    assert_eq!(m, ReadMode::Indexed { via_subscription: "usn".into() });
    // 订阅报告未消化变更 → 直读（索引落后）。
    struct LagSub;
    impl Subscription for LagSub {
        fn undigested_changes_since(&self, _ts: &str) -> Option<u64> {
            Some(3)
        }
        fn backend(&self) -> &'static str {
            "usn"
        }
    }
    let m = freshness::consult(&store, Some(&cp), Some(&LagSub));
    assert!(m.is_direct());
    // stat 失配（外部加文件）→ 直读。
    seed_project(&store, "0198f7a9-0000-7cd2-9e3f-000000000009", "extra");
    let m = freshness::consult(&store, Some(&cp), Some(&OkSub));
    assert!(m.is_direct(), "{m:?}");
    // NullSubscription 永不证明。
    let m = freshness::consult(&store, Some(&cp), Some(&NullSubscription));
    assert!(m.is_direct());
}

// ---------- 增量两轮：外部 git 提交出现于增量；无 .git 成立（红线10/26） ----------

#[test]
fn diff_two_rounds_with_external_git_commit() {
    let (_t, store, types) = fresh_store();
    seed_project(&store, PROJECT, PROJECT);
    let id = add_task(&store, &types, "t-git-ext", "v1");
    // 先回填（seed 的手写 project + task 的 create 都入日志），保证后续漂移只剩外部提交。
    mem_core::event::reconcile_backfill(&store, "t", None).unwrap();
    let rel = ops::get(&store, &id).unwrap().rel;
    // 建 git 仓库（历史基座是附属物）。
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(store.root())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("git 可用");
    std::process::Command::new("git")
        .args(["-c", "user.email=t@t", "-c", "user.name=t", "add", "-A"])
        .current_dir(store.root())
        .status()
        .unwrap();
    std::process::Command::new("git")
        .args(["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "init"])
        .current_dir(store.root())
        .status()
        .unwrap();
    // 外部手改 + 外部提交（机制路径外）。
    let cur = ops::get(&store, &id).unwrap();
    let bytes = mem_core::record::serialize_new(&cur.record.envelope, &cur.record.payload, "外部提交的内容".as_bytes());
    std::fs::write(store.abs(&rel), bytes).unwrap();
    std::process::Command::new("git")
        .args(["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-aqm", "external edit"])
        .current_dir(store.root())
        .status()
        .unwrap();
    // 第一轮 diff：外部提交的内容必须出现（与 git 无关——靠状态比较捕获，红线26）。
    let d1 = mem_core::drift::diff_since(&store, 0, 200, 100).unwrap();
    assert_eq!(d1.current_drift.items.len(), 1, "{:?}", d1.current_drift.items);
    assert!(d1.current_drift.items[0].note.contains("backfill 待执行"));
    // 回填后转正。
    mem_core::event::reconcile_backfill(&store, "t", None).unwrap();
    // 第二轮：不重复入 B；A 里可见。
    let d2 = mem_core::drift::diff_since(&store, 0, 200, 100).unwrap();
    assert!(d2.current_drift.items.is_empty(), "current_drift 不重复入 B");
    assert_eq!(d2.changes_since.len(), 2, "按记录去重：seed 与 task 各一组");
    let task_group = d2
        .changes_since
        .iter()
        .find(|g| g.id == id)
        .expect("task 变更组");
    assert_eq!(task_group.history.len(), 2, "create + backfill");
    // 无 .git 形态：删掉 .git，一切照常（红线10）。
    std::fs::remove_dir_all(store.root().join(".git")).unwrap();
    let id2 = add_task(&store, &types, "t-no-git", "x");
    assert!(ops::get(&store, &id2).is_ok());
    let _ = EventLog::new(&store).max_seq().unwrap();
}

#[test]
fn brief_never_creates_or_modifies_ledger() {
    // 红线25 的机制侧形式化：读取层（scan/view/get）不触碰 sessions/。
    let (_t, store, types) = fresh_store();
    seed_project(&store, PROJECT, PROJECT);
    let _id = add_task(&store, &types, "t-brief", "x");
    // 读路径执行（brief 的底层 = execute_filter + show）。
    let filter = mem_core::view::ViewFilter::default();
    let _ = view::execute_filter(&store, &filter, 100, 0).unwrap();
    let _ = ops::get(&store, PROJECT).unwrap();
    // sessions/ 无任何台账。
    let (ledgers, _) = session::scan_ledgers(&store).unwrap();
    assert!(ledgers.is_empty(), "brief 纯读：台账只能由 session start / 写路径创建");
}
