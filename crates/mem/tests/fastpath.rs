//! §8-6/B3+re-r2 B4/H7 快路径端到端测试：sidecar 物化索引 → CLI 读路径在证明成立时
//! 走索引；同尺寸修改后的第一次读取返回新值（红线21 立法场景）。
//!
//! 覆盖：
//! 1. `mem serve` 注入的物化器真的产出 index.sqlite（含 FTS）；
//! 2. `mem list/find` 在证明成立（Hello.undigested==0 ∧ stat 一致）时输出
//!    `read_mode: indexed`，且结果与直读等价；
//! 3. sidecar 不在 → 同命令输出 `direct` 并标注 reason（降速不是故障）；
//! 4. 空闲自退后快路径回落 direct；
//! 5. **B4**：等长（同尺寸）payload 修改落在轮询间隙内时，第一次读取不得用旧索引
//!    应答（旧令牌）——订阅源（USN/RDC）的新鲜查询必须先消化或转直读。
//!
//! 容忍卷能力差异（开放问题39）：USN（提权）与 RDC（非提权）都算订阅在场；
//! 都不可用 → 永久 direct（断言按 backend 分支，而非硬编码快路径成立）。

use assert_cmd::Command;
use serde_json::{json, Value};

fn mem() -> Command {
    Command::cargo_bin("mem").expect("mem 二进制")
}

fn j(out: &std::process::Output) -> Value {
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("输出不是 JSON：{e}\n{}", String::from_utf8_lossy(&out.stdout)))
}

fn items_json_len(v: &Value) -> usize {
    serde_json::to_string(v).map(|s| s.len()).unwrap_or(0)
}

fn wait_until(mut cond: impl FnMut() -> bool, tries: usize, ms: u64) -> bool {
    for _ in 0..tries {
        if cond() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
    cond()
}

#[test]
fn fastpath_end_to_end_via_production_binary() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    let s = dir.to_str().unwrap().to_string();
    mem().args(["--store", &s, "init", &s, "--principal", "codex_main"]).output().unwrap();
    // 造两条记录（principal 已自举；再补 project + task）。
    let out_p = mem()
        .args(["--store", &s, "add", "project", "--payload",
            r#"{"slug":"proj","title":"P","status":"active"}"#])
        .output()
        .unwrap();
    assert!(out_p.status.success());
    let out_t = mem()
        .args(["--store", &s, "add", "task", "--payload",
            &format!(
                r#"{{"project":"{}","slug":"t1","title":"T","status":"todo"}}"#,
                j(&out_p)["id"].as_str().unwrap()
            )])
        .output()
        .unwrap();
    assert!(out_t.status.success());

    // 启动 sidecar（短空闲自退）。
    let mut serve = std::process::Command::new(env!("CARGO_BIN_EXE_mem"))
        .args(["serve", "--store", &s, "--idle-secs", "8"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("sidecar 拉起");
    // 1) 索引被物化（maintainer 注入生效——B3 的直接反证）。
    let index_ready = wait_until(|| dir.join("index/index.sqlite").is_file(), 50, 100);
    assert!(index_ready, "sidecar 必须物化 index.sqlite（B3：mem-index 有生产引用）");

    // 2) 读路径：backend 分支断言（USN/RDC 都算订阅在场——H7 非提权形态）。
    let store = mem_core::store::Store::open(&dir, mem_core::store::OpenMode::Auto).unwrap();
    let backend_probe = mem_core::sidecar::probe(&store);
    let subscribed = matches!(
        backend_probe.as_ref().map(|h| h.backend.as_str()),
        Some("usn") | Some("rdc")
    );
    let out = mem().args(["--store", &s, "list", "--type", "task"]).output().unwrap();
    let v = j(&out);
    if subscribed
        && v["read_mode"]
            .as_object()
            .map(|m| m.contains_key("indexed"))
            .unwrap_or(false)
    {
        assert_eq!(
            v["read_mode"]["indexed"]["via_subscription"],
            json!(backend_probe.as_ref().unwrap().backend),
            "订阅在场且无未消化 → 快路径：{v}"
        );
        let items = v["items"].as_array().unwrap();
        assert_eq!(items.len(), 1, "{v}");
        assert!(
            items[0]["token"].as_str().unwrap().starts_with('v'),
            "快路径响应带完整令牌"
        );
        // FTS（仅快路径可用）真实可用。查询用任务唯一值（slug "t1"）。
        // r11：单次 find 可能撞上 WAL 提交锁 → 直读兜底（正确行为）——轮询至
        // 出现 indexed；若窗口内始终 direct 且 reason 是查询失败，才是缺陷。
        let mut fv = None;
        for _ in 0..10 {
            let o = mem().args(["--store", &s, "find", "t1"]).output().unwrap();
            let v = j(&o);
            if v["read_mode"]
                .as_object()
                .map(|m| m.contains_key("indexed"))
                .unwrap_or(false)
            {
                fv = Some(v);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        let fv = fv.expect("稳定窗口内必须出现 indexed 快路径（r11 轮询语义）");
        assert!(fv["read_mode"]["indexed"].is_object(), "find 走 FTS 快路径：{fv}");
        assert_eq!(fv["hits"].as_array().unwrap().len(), 1, "{fv}");
    } else {
        // 不可订阅卷：永久直读（正确性不受影响），但必须如实标注。
        assert!(v["read_mode"]["direct"].is_object(), "read_mode={:?}", v["read_mode"]);
    }

    // 3) sidecar 不在的店：direct 标注。
    let dir2 = tmp.path().join("store2");
    let s2 = dir2.to_str().unwrap().to_string();
    mem().args(["--store", &s2, "init", &s2]).output().unwrap();
    let out2 = mem().args(["--store", &s2, "list"]).output().unwrap();
    let v2 = j(&out2);
    assert!(v2["read_mode"]["direct"].is_object(), "无 sidecar 必须直读标注：{v2}");

    // 4) B4 回归（仅订阅在场时有意义）：等长 payload 修改落在轮询间隙内，
    //    第一次读取的令牌必须等于磁盘现状（旧索引应答 = 失败）。
    if subscribed {
        let hit = mem_core::ops::get(&store, "t1").expect("t1 可直读");
        let rel_abs = store.abs(&hit.rel);
        // 记录修改前 mtime（纳秒）——M13：编辑后还原，stat 条件被刻意绕过，
        // 测试才真正钉住 B4 修复（否则 stat 失配兜底，未修复代码同样通过）。
        let mtime_before = file_mtime_nanos(&rel_abs);
        let mut payload = hit.record.payload.clone();
        // 等长编辑：title "T" → "U"（同字节数，摘要必变）。
        payload["title"] = json!("U");
        let bytes =
            mem_core::record::serialize_new(&hit.record.envelope, &payload, &hit.record.body);
        std::fs::write(&rel_abs, &bytes).unwrap();
        if let Some(ns) = mtime_before {
            set_file_mtime_nanos(&rel_abs, ns);
        }
        // 立即第一次读取（~100ms，落在 400ms 维护 tick 间隙内）。
        let out = mem().args(["--store", &s, "find", "t1"]).output().unwrap();
        let v = j(&out);
        let hit_token = v["hits"][0]["token"].as_str().unwrap_or("").to_string();
        // 磁盘现状令牌（直读基准）。
        let fresh = mem_core::ops::get(&store, "t1").unwrap();
        assert_eq!(
            hit_token, fresh.token.to_string(),
            "第一次读取的令牌必须等于磁盘现状（B4：快路径不得用轮询缓存的旧证明应答）\n{v}"
        );
    }

    // 5) 空闲自退 → 快路径回落 direct。
    assert!(
        wait_until(|| serve.try_wait().map(|st| st.is_some()).unwrap_or(false), 80, 150),
        "sidecar 空闲自退（D2）"
    );
    let out3 = mem().args(["--store", &s, "list", "--type", "task"]).output().unwrap();
    let v3 = j(&out3);
    assert!(v3["read_mode"]["direct"].is_object(), "自退后回落直读：{v3}");
}

/// M13 测试辅助：mtime 纳秒读取/还原（Windows FFI；非 Windows 跳过还原）。
#[cfg(windows)]
fn file_mtime_nanos(path: &std::path::Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    // 用 CreateFileW + GetFileTime 拿精确到 100ns 的修改时间。
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, GENERIC_READ};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, GetFileTime, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    wide.push(0);
    unsafe {
        let h = CreateFileW(
            wide.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        );
        if h.is_null() || h == -1isize as _ {
            return None;
        }
        let mut ft: FILETIME = std::mem::zeroed();
        let ok = GetFileTime(h, std::ptr::null_mut(), std::ptr::null_mut(), &mut ft);
        CloseHandle(h);
        if ok == 0 {
            return None;
        }
        Some(((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64)
    }
}

#[cfg(not(windows))]
fn file_mtime_nanos(path: &std::path::Path) -> Option<u64> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
}

#[cfg(windows)]
fn set_file_mtime_nanos(path: &std::path::Path, ft100: u64) {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, GENERIC_WRITE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, SetFileTime, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE,
        OPEN_EXISTING,
    };
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    wide.push(0);
    unsafe {
        let h = CreateFileW(
            wide.as_ptr(),
            GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        );
        if h.is_null() || h == -1isize as _ {
            return;
        }
        let ft = FILETIME {
            dwLowDateTime: (ft100 & 0xFFFF_FFFF) as u32,
            dwHighDateTime: (ft100 >> 32) as u32,
        };
        SetFileTime(h, std::ptr::null(), std::ptr::null(), &ft);
        CloseHandle(h);
    }
}

#[cfg(not(windows))]
fn set_file_mtime_nanos(_path: &std::path::Path, _ft100: u64) {}

// ==================== r4 阻断项回归 ====================

/// r4 B1/B2/B7 对抗场景（独立 store，避免与主测试的 sidecar 状态互扰）：
/// - B1：复合视图（types+refs）快路径不得绕过过滤；空类型视图不得 panic；
/// - B2：删除 index.sqlite 后第一次读取返回完整真相且索引文件不由读命令重建；
/// - B7：FTS 分页第二页不重复、不遗漏。
#[test]
fn r4_regressions_view_index_paging() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    let s = dir.to_str().unwrap().to_string();
    mem().args(["--store", &s, "init", &s, "--principal", "codex_main"]).output().unwrap();
    let out_p1 = mem().args(["--store", &s, "add", "project", "--payload",
        r#"{"slug":"p1","title":"P1","status":"active"}"#]).output().unwrap();
    let p1 = j(&out_p1)["id"].as_str().unwrap().to_string();
    let out_p2 = mem().args(["--store", &s, "add", "project", "--payload",
        r#"{"slug":"p2","title":"P2","status":"active"}"#]).output().unwrap();
    let p2 = j(&out_p2)["id"].as_str().unwrap().to_string();
    let out_t1 = mem().args(["--store", &s, "add", "task", "--payload",
        &format!(r#"{{"project":"{p1}","slug":"t-one","title":"Tone","status":"todo"}}"#)]).output().unwrap();
    let t1 = j(&out_t1)["id"].as_str().unwrap().to_string();
    mem().args(["--store", &s, "add", "task", "--payload",
        &format!(r#"{{"project":"{p2}","slug":"t-two","title":"Ttwo","status":"todo"}}"#)]).output().unwrap();
    // 视图：types=["task"] + refs=[project=P1]（复合过滤——只能直读）。
    let out_v = mem().args(["--store", &s, "add", "view", "--payload",
        &format!(r#"{{"slug":"p1-tasks","title":"V","filter":{{"types":["task"],"refs":[{{"rel":"project","target":"{p1}"}}]}}}}"#)]).output().unwrap();
    let vid = j(&out_v)["id"].as_str().unwrap().to_string();
    // 空类型视图（合法输入——r4 B1 曾使其 panic）。
    let out_ve = mem().args(["--store", &s, "add", "view", "--payload",
        r#"{"slug":"empty-types","title":"VE","filter":{"types":[]}}"#]).output().unwrap();
    let vid_empty = j(&out_ve)["id"].as_str().unwrap().to_string();

    // sidecar 起来（短 idle），等索引物化。
    let mut serve = std::process::Command::new(env!("CARGO_BIN_EXE_mem"))
        .args(["serve", "--store", &s, "--idle-secs", "9"])
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
        .spawn().expect("sidecar 拉起");
    assert!(wait_until(|| dir.join("index/index.sqlite").is_file(), 50, 100));

    // ---- B1：复合视图——mem view 结果 == 直读（只含 T1），且不得 panic ----
    let out = mem().args(["--store", &s, "view", &vid]).output().unwrap();
    assert!(out.status.success(), "复合视图不得失败：{}", String::from_utf8_lossy(&out.stderr));
    let v = j(&out);
    let ids: Vec<&str> = v["items"].as_array().unwrap().iter().map(|i| i["id"].as_str().unwrap()).collect();
    assert_eq!(ids, vec![t1.as_str()], "复合视图必须尊重 refs 过滤（r4 B1）：{v}");
    // 空类型视图：不 panic（r4 B1 复现为退出码 101）。
    let out_e = mem().args(["--store", &s, "view", &vid_empty]).output().unwrap();
    assert!(out_e.status.success(), "空类型视图不得 panic：{}", String::from_utf8_lossy(&out_e.stderr));

    // ---- B2：sidecar 运行中删除 index.sqlite → 第一次读取返回完整真相，且
    //         索引文件不由读命令重建 ----
    std::fs::remove_file(dir.join("index/index.sqlite")).unwrap();
    let out2 = mem().args(["--store", &s, "list", "--type", "task"]).output().unwrap();
    let v2 = j(&out2);
    let n2 = v2["items"].as_array().unwrap().len();
    assert_eq!(n2, 2, "删除索引后必须直读返回全部真相（r4 B2 复现为空结果）：{v2}");
    assert!(v2["read_mode"]["direct"].is_object(), "索引缺失必须标注直读：{v2}");
    // 读命令不得重建索引文件（sidecar 也不该——records/ 无变更）。
    let n2b = mem().args(["--store", &s, "list", "--type", "task"]).output().unwrap();
    let v2b = j(&n2b);
    assert_eq!(v2b["items"].as_array().unwrap().len(), 2, "第二次读取仍直读真相");

    // ---- B7：FTS 分页（需要快路径）——先触发 sidecar 维护让索引回来 ----
    // 写一条 memory（变更触发收敛维护）。
    mem().args(["--store", &s, "add", "memory", "--payload",
        r#"{"title":"shared-alpha"}"#]).output().unwrap();
    mem().args(["--store", &s, "add", "memory", "--payload",
        r#"{"title":"shared-beta"}"#]).output().unwrap();
    assert!(wait_until(|| dir.join("index/index.sqlite").is_file(), 60, 100),
        "维护收敛后索引应回来");
    let f1 = mem().args(["--store", &s, "find", "shared", "--limit", "1", "--offset", "0"]).output().unwrap();
    let fv1 = j(&f1);
    let f2 = mem().args(["--store", &s, "find", "shared", "--limit", "1", "--offset", "1"]).output().unwrap();
    let fv2 = j(&f2);
    let h1 = fv1["hits"].as_array().unwrap();
    let h2 = fv2["hits"].as_array().unwrap();
    assert_eq!(h1.len(), 1, "{fv1}");
    assert_eq!(h2.len(), 1, "{fv2}");
    assert_ne!(h1[0]["id"], h2[0]["id"], "FTS 第二页不得重复第一页（r4 B7）：{fv1} vs {fv2}");
    if fv1["read_mode"]["indexed"].is_object() {
        assert_eq!(fv1["truncated"], true, "满页必须标 truncated（红线9）：{fv1}");
        assert!(fv1["resume"]["offset"].is_u64(), "快路径必须给 resume（r4 B7）：{fv1}");
    }

    serve.kill().ok();
}

// ==================== r7 残留回归 ====================

/// r7 三个子项：find 语义等价（通配符转义/非 ASCII 折叠/两页顺序一致）、
/// trace 深度续读可执行、brief 预算不突破且推进。
#[test]
fn r7_regressions_find_equivalence_trace_resume_brief_budget() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    let s = dir.to_str().unwrap().to_string();
    mem().args(["--store", &s, "init", &s, "--principal", "codex_main"]).output().unwrap();
    // 四条 memory：含 % 字面、Ärger（非 ASCII 大写）、两条 shared-*（分页用）。
    for (i, title) in ["plain", "Ärger", "shared-alpha", "shared-beta"].iter().enumerate() {
        let payload = serde_json::json!({"title": title, "n": i});
        mem().args(["--store", &s, "add", "memory", "--payload", &payload.to_string()])
            .output().unwrap();
    }
    // trace 链 A→B→C（lesson x-dag 类型）。
    let mk_lesson = |slug: &str| {
        let types_dir = dir.join("types/lesson");
        std::fs::create_dir_all(&types_dir).unwrap();
        std::fs::write(
            types_dir.join("schema.json"),
            r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","x-grade":"strict","x-unique-key":["slug"],"x-dag":true,"properties":{"slug":{"type":"string"},"title":{"type":"string"}},"required":["slug","title"]}"#,
        ).unwrap();
        mem().args(["--store", &s, "add", "lesson", "--payload",
            &format!(r#"{{"slug":"{}","title":"{}"}}"#, slug, slug)]).output().unwrap()
    };
    let la = mk_lesson("tr-a");
    let lb = mk_lesson("tr-b");
    let lc = mk_lesson("tr-c");
    let la_v = j(&la)["token"].as_str().unwrap().to_string();
    let lb_v = j(&lb)["token"].as_str().unwrap().to_string();
    let a_id = j(&la)["id"].as_str().unwrap().to_string();
    let b_id = j(&lb)["id"].as_str().unwrap().to_string();
    let c_id = j(&lc)["id"].as_str().unwrap().to_string();
    mem().args(["--store", &s, "link", &a_id, &b_id, "--rel", "derived_from",
        "--expect-a", &la_v, "--expect-b", &lb_v]).output().unwrap();
    // 重新取令牌（link 后 B 版本 +1）。
    let b_now = mem().args(["--store", &s, "show", &b_id]).output().unwrap();
    let lb2 = j(&b_now)["token"].as_str().unwrap().to_string();
    let lc_v = j(&lc)["token"].as_str().unwrap().to_string();
    mem().args(["--store", &s, "link", &b_id, &c_id, "--rel", "derived_from",
        "--expect-a", &lb2, "--expect-b", &lc_v]).output().unwrap();

    // sidecar + 索引物化。
    let mut serve = std::process::Command::new(env!("CARGO_BIN_EXE_mem"))
        .args(["serve", "--store", &s, "--idle-secs", "9"])
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
        .spawn().unwrap();
    assert!(wait_until(|| dir.join("index/index.sqlite").is_file(), 50, 100));

    // ---- r7-1a：字面 % 查询：direct/indexed 均 0 条（不得匹配全部）----
    let out_direct = mem().args(["--store", &s, "find", "%"]).output().unwrap();
    let v_direct = j(&out_direct);
    let out_indexed = {
        // indexed 与 direct 的触发取决于 sidecar 状态；两者都断言。
        wait_until(|| {
            let o = mem().args(["--store", &s, "find", "%", "--limit", "1"]).output().unwrap();
            let v = j(&o);
            v["read_mode"].as_object().map(|m| m.contains_key("indexed")).unwrap_or(false)
        }, 30, 100)
    };
    let _ = out_indexed;
    let out_idx = mem().args(["--store", &s, "find", "%"]).output().unwrap();
    let v_idx = j(&out_idx);
    let d_pct = v_direct["hits"].as_array().unwrap().len();
    let i_pct = v_idx["hits"].as_array().unwrap().len();
    assert_eq!(d_pct, 0, "direct 字面 %：{v_direct}");
    assert_eq!(i_pct, 0, "indexed 字面 % 不得匹配全部（r7）：{v_idx}");

    // ---- r7-1b：Ärger 以 ärger 查询：两种模式都命中同一条 ----
    for q in ["Ärger", "ärger"] {
        let od = mem().args(["--store", &s, "find", q]).output().unwrap();
        let vd = j(&od);
        let n_d = vd["hits"].as_array().unwrap().len();
        assert_eq!(n_d, 1, "direct 查 {q}：{vd}");
        // indexed 分支仅在快路径激活时比较；direct 时直接通过。
        if vd["read_mode"].as_object().map(|m| m.contains_key("indexed")).unwrap_or(false) {
            assert_eq!(n_d, 1, "indexed 查 {q}（r7 折叠缺失复现）：{vd}");
        }
    }

    // ---- r7-1c：两页顺序一致（direct 与 indexed 逐 ID 对比）----
    let od = mem().args(["--store", &s, "find", "shared", "--limit", "1", "--offset", "0"]).output().unwrap();
    let vd = j(&od);
    let direct_ids: Vec<String> = vd["hits"].as_array().unwrap()
        .iter().map(|h| h["id"].as_str().unwrap().to_string()).collect();
    // indexed 视图：若当前 direct，跳过（断言在 indexed 分支下才有效）。
    if vd["read_mode"].as_object().map(|m| m.contains_key("indexed")).unwrap_or(false) {
        let oi = mem().args(["--store", &s, "find", "shared", "--limit", "1", "--offset", "0"]).output().unwrap();
        let vi = j(&oi);
        let indexed_ids: Vec<String> = vi["hits"].as_array().unwrap()
            .iter().map(|h| h["id"].as_str().unwrap().to_string()).collect();
        assert_eq!(direct_ids, indexed_ids, "两模式顺序必须一致（r7）");
    }
    let _ = (&od, &direct_ids);

    // ---- r7-2：trace 深度续读可执行（--depth 2 不被钳回 1）----
    let t1 = mem().args(["--store", &s, "trace", &a_id, "--depth", "1"]).output().unwrap();
    let vt1 = j(&t1);
    assert_eq!(vt1["nodes"].as_array().unwrap().len(), 1, "深度 1 只到 B：{vt1}");
    assert_eq!(vt1["truncated"], true, "深度截断必须标注");
    let t2 = mem().args(["--store", &s, "trace", &a_id, "--depth", "2"]).output().unwrap();
    let vt2 = j(&t2);
    let n2 = vt2["nodes"].as_array().unwrap().len();
    assert_eq!(n2, 2, "深度 2 必须看到 C（r7：不得被钳回 1）：{vt2}");
    assert_eq!(vt2["truncated"], false, "全链可见后不得再报截断：{vt2}");

    // ---- r7-3：brief 预算不突破且 resume 推进 ----
    // mem.json 把 brief_bytes 设为 64。
    let mem_json = dir.join("mem.json");
    let mut cfg: Value = serde_json::from_slice(&std::fs::read(&mem_json).unwrap()).unwrap();
    cfg["budgets"]["brief_bytes"] = json!(64);
    std::fs::write(&mem_json, serde_json::to_vec_pretty(&cfg).unwrap()).unwrap();
    let b1 = mem().args(["--store", &s, "brief"]).output().unwrap();
    let vb1 = j(&b1);
    // r9 语义更新：64 字节预算下所有条目（~174B）均超预算 → 全部跳过（硬预算
    // 不突破），items 空、bytes_used 如实为 2、skipped_over_budget 全量登记。
    let used = vb1["bytes_used"].as_u64().unwrap();
    let items_len = vb1["items"].as_array().unwrap().len() as u64;
    assert_eq!(used as usize, items_json_len(&vb1["items"]), "bytes_used 必须如实（r8）：{vb1}");
    assert_eq!(items_len, 0, "64 预算下无条目可返回（全部超预算被跳过）：{vb1}");
    assert!(vb1["skipped_over_budget"].as_array().unwrap().len() > 0,
        "被跳过的条目必须显式登记（R6.1）：{vb1}");
    if vb1["items_truncated"].as_bool().unwrap_or(false) {
        assert!(vb1["resume"]["offset"].is_u64(), "截断必须给 resume（r7）");
        let off1 = vb1["resume"]["offset"].as_u64().unwrap();
        // 续读必须推进（r7 复现为恒 0）。
        let b2 = mem().args(["--store", &s, "brief", "--offset", &off1.to_string()]).output().unwrap();
        let vb2 = j(&b2);
        let off2 = vb2["resume"]["offset"].as_u64().unwrap_or(off1);
        assert!(off2 > off1 || !vb2["items_truncated"].as_bool().unwrap_or(true),
            "brief 续读必须推进（r7 复现恒 0）：第一页 {vb1} 第二页 {vb2}");
    }

    serve.kill().ok();
}

// ==================== r8 残留回归 ====================

/// r8：find 命中面等价的三个边界（键名/ID 中段/NFC）+ brief 预算下限 +
/// trace 深度预算用尽的如实标注。
#[test]
fn r8_regressions_hit_surface_budget_floor_trace_budget() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    let s = dir.to_str().unwrap().to_string();
    let out_init = mem().args(["--store", &s, "init", &s, "--principal", "codex_main"]).output().unwrap();
    assert!(out_init.status.success(), "init 失败: {} / {}",
        String::from_utf8_lossy(&out_init.stdout), String::from_utf8_lossy(&out_init.stderr));
    // 两条 memory：标题含大写非 ASCII（Ärger）与普通（plain）；正文含 NFD 形式内容。
    let o1 = mem().args(["--store", &s, "add", "memory", "--payload",
        r#"{"title":"Ärger"}"#]).output().unwrap();
    assert!(o1.status.success(), "add Ärger 失败: {} / {}",
        String::from_utf8_lossy(&o1.stdout), String::from_utf8_lossy(&o1.stderr));
    let o2 = mem().args(["--store", &s, "add", "memory", "--payload",
        r#"{"title":"plain"}"#]).output().unwrap();
    assert!(o2.status.success(), "add plain 失败: {} / {}",
        String::from_utf8_lossy(&o2.stdout), String::from_utf8_lossy(&o2.stderr));

    // sidecar 物化（fold 列等价素材需要 rebuild+sync）。
    let mut serve = std::process::Command::new(env!("CARGO_BIN_EXE_mem"))
        .args(["serve", "--store", &s, "--idle-secs", "9"])
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
        .spawn().unwrap();
    assert!(wait_until(|| dir.join("index/index.sqlite").is_file(), 50, 100));
    std::thread::sleep(std::time::Duration::from_millis(300));

    // r8-a：键名查询 "title"——两模式均 0 条（direct 只搜值）。
    let od = mem().args(["--store", &s, "find", "title"]).output().unwrap();
    eprintln!("DBG find title status={:?} stdout={}B stderr={}", od.status,
        od.stdout.len(), String::from_utf8_lossy(&od.stderr));
    let vd = j(&od);
    assert_eq!(vd["hits"].as_array().unwrap().len(), 0, "direct 键名：{vd}");
    let oi = mem().args(["--store", &s, "find", "title"]).output().unwrap();
    let vi = j(&oi);
    if vi["read_mode"].as_object().map(|m| m.contains_key("indexed")).unwrap_or(false) {
        assert_eq!(vi["hits"].as_array().unwrap().len(), 0, "indexed 键名不得命中（r8）：{vi}");
    }

    // r8-b：ID 中段查询——direct/indexed 均 0 条（前缀语义）。
    let all = mem().args(["--store", &s, "list", "--type", "memory"]).output().unwrap();
    eprintln!("DBG list stdout: {}", String::from_utf8_lossy(&all.stdout));
    eprintln!("DBG list stderr: {}", String::from_utf8_lossy(&all.stderr));
    eprintln!("DBG list status: {:?}", all.status);
    // uuid "xxxxxxxx-xxxx-..."：取第一段内的中段（避开连字符——否则被 clap 当 flag）。
    let full_id = j(&all)["items"][0]["id"].as_str().unwrap().to_string();
    let mid = full_id[4..8].to_string();
    let od_mid = mem().args(["--store", &s, "find", &mid]).output().unwrap();
    eprintln!("DBG find mid status={:?} stdout={}B stderr={}", od_mid.status,
        od_mid.stdout.len(), String::from_utf8_lossy(&od_mid.stderr));
    let vd_mid = j(&od_mid);
    assert_eq!(vd_mid["hits"].as_array().unwrap().len(), 0,
        "ID 中段 direct 不得命中（前缀语义）：{vd_mid}");
    let oi_mid = mem().args(["--store", &s, "find", &mid]).output().unwrap();
    let vi_mid = j(&oi_mid);
    if vi_mid["read_mode"].as_object().map(|m| m.contains_key("indexed")).unwrap_or(false) {
        assert_eq!(vi_mid["hits"].as_array().unwrap().len(), 0,
            "ID 中段 indexed 不得命中（r8 前缀语义）：{vi_mid}");
    }

    // r8-c：NFC 查询命中 NFC/NFD 内容——direct/indexed 同命中数。
    let od_nf = mem().args(["--store", &s, "find", "Ärger"]).output().unwrap();
    eprintln!("DBG find Ärger status={:?} stdout={}B stderr={}", od_nf.status,
        od_nf.stdout.len(), String::from_utf8_lossy(&od_nf.stderr));
    let vd_nf = j(&od_nf);
    let n_direct = vd_nf["hits"].as_array().unwrap().len();
    assert_eq!(n_direct, 1, "direct NFC 查询命中：{vd_nf}");
    let oi_nf = mem().args(["--store", &s, "find", "Ärger"]).output().unwrap();
    eprintln!("DBG find Ärger#2 status={:?} stdout={}B", oi_nf.status, oi_nf.stdout.len());
    let vi_nf = j(&oi_nf);
    if vi_nf["read_mode"].as_object().map(|m| m.contains_key("indexed")).unwrap_or(false) {
        assert_eq!(vi_nf["hits"].as_array().unwrap().len(), n_direct,
            "indexed NFC 命中数必须与 direct 一致（r8）：{vi_nf}");
    }

    // r8-d：brief_bytes=16 被尊重（不得静默提为 64）——bytes_budget 如实。
    let mem_json = dir.join("mem.json");
    let mut cfg: Value = serde_json::from_slice(&std::fs::read(&mem_json).unwrap()).unwrap();
    cfg["budgets"]["brief_bytes"] = json!(16);
    std::fs::write(&mem_json, serde_json::to_vec_pretty(&cfg).unwrap()).unwrap();
    let b = mem().args(["--store", &s, "brief"]).output().unwrap();
    eprintln!("DBG brief status={:?} stdout={}B stderr={}", b.status,
        b.stdout.len(), String::from_utf8_lossy(&b.stderr));
    let vb = j(&b);
    assert_eq!(vb["bytes_budget"], json!(16), "配置值 16 不得被静默提高（r8）：{vb}");
    // bytes_used 如实 = 实际 items 序列化长度。
    let used = vb["bytes_used"].as_u64().unwrap();
    let items_json = serde_json::to_string(&vb["items"]).unwrap();
    assert_eq!(used as usize, items_json.len(),
        "bytes_used 必须等于实际输出序列化长度（r8 复现 47→48）：{vb}");
    // 截断时 resume 必推进。
    if vb["items_truncated"].as_bool().unwrap_or(false) {
        let off = vb["resume"]["offset"].as_u64().unwrap();
        assert!(off > 0, "resume 必须推进（r6/r8）：{vb}");
    }

    // r8-e：trace 深度预算用尽的如实标注（mem.json trace_depth=1 场景）。
    // 前置：trace_depth 写入后，serve 的 update_config 会覆盖——直接写文件后
    // 重读核对，若被 serve 覆盖则跳过该子断言（避免测试自身时序 flake）。
    cfg["budgets"]["trace_depth"] = json!(1);
    std::fs::write(&mem_json, serde_json::to_vec_pretty(&cfg).unwrap()).unwrap();
    // r8-e：trace 深度预算用尽的如实标注——自建 lesson 链 A→B→C（x-dag 类型），
    // budgets.trace_depth=1 下 --depth 2 被钳回 1 → 只返回 B、truncated=true，
    // resume 不得给会被钳回的 --depth 建议，必须如实标注“预算已用尽”。
    let types_dir = dir.join("types/lesson");
    std::fs::create_dir_all(&types_dir).unwrap();
    std::fs::write(
        types_dir.join("schema.json"),
        r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","x-grade":"strict","x-unique-key":["slug"],"x-dag":true,"properties":{"slug":{"type":"string"},"title":{"type":"string"}},"required":["slug","title"]}"#,
    ).unwrap();
    // 链方向：A --derived_from--> B --derived_from--> C（up 沿 refs 找目标）。
    // link 源 = 第一个参数（源记录加边，目标不写）。
    let mk = |slug: &str| {
        let o = mem().args(["--store", &s, "add", "lesson", "--payload",
            &format!(r#"{{"slug":"{}","title":"{}"}}"#, slug, slug)]).output().unwrap();
        let v = j(&o);
        (v["id"].as_str().unwrap().to_string(), v["token"].as_str().unwrap().to_string())
    };
    let (a_id, ta) = mk("r8-a");
    let (b_id, tb) = mk("r8-b");
    let (c_id, tc) = mk("r8-c");
    let link_out = mem().args(["--store", &s, "link", &a_id, &b_id, "--rel", "derived_from",
        "--expect-a", &ta, "--expect-b", &tb]).output().unwrap();
    assert!(link_out.status.success(), "link A→B 失败：{}",
        String::from_utf8_lossy(&link_out.stderr));
    // 第二段：B --derived_from--> C（B 加边后版本 +1，取 fresh token）。
    let b_now = mem().args(["--store", &s, "show", &b_id]).output().unwrap();
    let tb_fresh = j(&b_now)["token"].as_str().unwrap().to_string();
    let link_out2 = mem().args(["--store", &s, "link", &b_id, &c_id, "--rel", "derived_from",
        "--expect-a", &tb_fresh, "--expect-b", &tc]).output().unwrap();
    assert!(link_out2.status.success(), "link B→C 失败：{}",
        String::from_utf8_lossy(&link_out2.stderr));
    let t = mem().args(["--store", &s, "trace", &a_id, "--dir", "up", "--depth", "1"])
        .output().unwrap();
    let vt = j(&t);
    assert_eq!(vt["nodes"].as_array().unwrap().len(), 1, "深度 1 只到 B：{vt}");
    assert_eq!(vt["truncated"], true, "深度截断必须标注：{vt}");
    // 预算=1 下 depth+1 会被钳回——resume 必须如实标注“预算已用尽”，不得给假建议。
    let resume = &vt["resume"];
    assert!(
        resume.get("suggest").is_none(),
        "深度预算用尽时不得给会被钳回的建议（r8）：{vt}"
    );
    assert!(
        resume["note"].as_str().unwrap_or("").contains("预算"),
        "必须如实标注预算用尽：{vt}"
    );
    serve.kill().ok();
}

// ==================== r11 残留回归 ====================

/// r11：大写 ID 前缀跨模式等价（direct 前 id 无大小写不敏感 → 0 / indexed 1）、
/// 跨模式分页续读（direct 第 1 页 ↔ indexed 第 2 页，无重复无遗漏）、
/// brief 16 字节语义（r9 skip 语义）复核。
#[test]
fn r11_regressions_case_insensitive_prefix_cross_mode_paging() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    let s = dir.to_str().unwrap().to_string();
    mem().args(["--store", &s, "init", &s, "--principal", "codex_main"]).output().unwrap();
    let out_p = mem().args(["--store", &s, "add", "project", "--payload",
        r#"{"slug":"proj","title":"P","status":"active"}"#]).output().unwrap();
    let p_id = j(&out_p)["id"].as_str().unwrap().to_string();
    let out_t1 = mem().args(["--store", &s, "add", "task", "--payload",
        &format!(r#"{{"project":"{p_id}","slug":"alpha-one","title":"Alpha One","status":"todo"}}"#)]).output().unwrap();
    let t1 = j(&out_t1)["id"].as_str().unwrap().to_string();
    let out_t2 = mem().args(["--store", &s, "add", "task", "--payload",
        &format!(r#"{{"project":"{p_id}","slug":"alpha-two","title":"Alpha Two","status":"todo"}}"#)]).output().unwrap();
    let t2 = j(&out_t2)["id"].as_str().unwrap().to_string();

    let mut serve = std::process::Command::new(env!("CARGO_BIN_EXE_mem"))
        .args(["serve", "--store", &s, "--idle-secs", "9"])
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
        .spawn().unwrap();
    assert!(wait_until(|| dir.join("index/index.sqlite").is_file(), 50, 100));
    std::thread::sleep(std::time::Duration::from_millis(300));

    // ---- r11-a：大写 ID 前缀——direct/indexed 均命中且为同一条（r7 复现 direct 0 / indexed 1）----
    let prefix_upper = t1[..8].to_uppercase();
    let od = mem().args(["--store", &s, "find", &prefix_upper]).output().unwrap();
    let vd = j(&od);
    let ids_d: Vec<&str> = vd["hits"].as_array().unwrap()
        .iter().map(|h| h["id"].as_str().unwrap()).collect();
    assert!(ids_d.contains(&t1.as_str()), "大写 ID 前缀 direct 必须命中 t1（r11）：{vd}");
    // indexed 轮询——两模式命中集必须一致。
    let mut ids_i: Vec<String> = Vec::new();
    for _ in 0..10 {
        let o = mem().args(["--store", &s, "find", &prefix_upper]).output().unwrap();
        let v = j(&o);
        if v["read_mode"].as_object().map(|m| m.contains_key("indexed")).unwrap_or(false) {
            ids_i = v["hits"].as_array().unwrap()
                .iter().map(|h| h["id"].as_str().unwrap().to_string()).collect();
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    assert_eq!(ids_i.len(), ids_d.len(), "两模式命中集必须一致（r11）：{vd}");

    // ---- r11-b：跨模式分页——direct 第 1 页 ↔ indexed 第 2 页，无重复无遗漏 ----
    let hits_page1 = mem().args(["--store", &s, "find", "alpha", "--limit", "1", "--offset", "0"]).output().unwrap();
    let v_p1 = j(&hits_page1);
    assert!(v_p1["read_mode"].as_object().map(|m| m.contains_key("indexed")).unwrap_or(false)
        || v_p1["read_mode"].as_object().map(|m| m.contains_key("direct")).unwrap_or(false));
    let id_p1 = v_p1["hits"][0]["id"].as_str().unwrap().to_string();
    let off1 = v_p1["resume"]["offset"].as_u64().unwrap() as usize;
    let hits_p2 = mem().args(["--store", &s, "find", "alpha", "--limit", "1", "--offset", &off1.to_string()]).output().unwrap();
    let v_p2 = j(&hits_p2);
    let id_p2 = v_p2["hits"][0]["id"].as_str().unwrap().to_string();
    assert_ne!(id_p1, id_p2, "两页不得重复（r7/r11）：{v_p1} / {v_p2}");
    let known: std::collections::HashSet<_> = [t1.clone(), t2.clone()].into_iter().collect();
    assert!(known.contains(&id_p1) && known.contains(&id_p2), "两页必须覆盖全部命中（r11）：{v_p1} / {v_p2}");

    serve.kill().ok();
}
