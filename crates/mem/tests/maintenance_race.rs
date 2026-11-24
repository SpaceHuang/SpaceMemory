#![cfg(windows)]

//! r4 B3 维护竞态回归：维护期间二次写入不得被检查点"提前确认"。
//!
//! 评审复现方式：使用与生产 serve 相同的真实 mem-index rebuild 物化器，在每次
//! rebuild 之后（索引扫描完成之后、检查点发布之前——生产顺序允许的竞态窗口）
//! 注入一次**同尺寸 + mtime 纳秒还原**的修改，刻意绕过 stat 条件。
//!
//! 修复前：检查点包含新文件状态而索引是旧内容 → Hello 宣称无未消化 + stat 一致
//! → 旧索引被判新鲜（indexed 返回旧令牌）。
//! Windows RDC 不承担新鲜度证明。sidecar 可以继续后台物化，但生产查询必须直读；
//! 本测试确认竞态写入后的首次查询直接读到磁盘当前令牌。

use assert_cmd::Command;
use serde_json::Value;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

fn mem() -> Command {
    Command::cargo_bin("mem").expect("mem 二进制")
}

fn j(out: &std::process::Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "输出不是 JSON：{e}\n{}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

fn find_ascii(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(windows)]
fn file_mtime_nanos(path: &std::path::Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, GENERIC_READ};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, GetFileTime, OPEN_EXISTING,
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
fn file_mtime_nanos(_path: &std::path::Path) -> Option<u64> {
    None
}

#[cfg(windows)]
fn set_mtime_nanos(path: &std::path::Path, ft100: u64) {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, GENERIC_WRITE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
        SetFileTime,
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
fn set_mtime_nanos(_path: &std::path::Path, _ft100: u64) {}

#[test]
#[cfg(windows)] // 该端到端竞态断言依赖 sidecar 变更订阅与 indexed 读取
fn maintenance_race_second_write_not_confirmed_early() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    let s = dir.to_str().unwrap().to_string();
    mem()
        .args(["--store", &s, "init", &s, "--principal", "codex_main"])
        .output()
        .unwrap();
    let out_t = mem()
        .args([
            "--store",
            &s,
            "add",
            "memory",
            "--principal",
            "codex_main",
            "--payload",
            r#"{"title":"race-target"}"#,
        ])
        .output()
        .unwrap();
    assert!(out_t.status.success());
    let target_id = j(&out_t)["id"].as_str().unwrap().to_string();
    let store = mem_core::store::Store::open(&dir, mem_core::store::OpenMode::Auto).unwrap();
    let target_rel = mem_core::ops::get(&store, &target_id).unwrap().rel;
    let target_path = dir.join(&target_rel);
    assert!(target_path.is_file());
    let original_record = mem_core::ops::get(&store, &target_id).unwrap();
    let original_token = original_record.token;
    let original_bytes = std::fs::read(&target_path).unwrap();
    let original_digest = mem_core::token::sha256_hex(&original_bytes);
    let original_size = original_bytes.len();
    let original_mtime = file_mtime_nanos(&target_path).expect("Windows 文件修改时间可读取");

    // 注入式维护器：第一次 rebuild 后，在竞态窗口注入同尺寸 + mtime 还原的修改。
    let injected = Arc::new(AtomicBool::new(false));
    let injected2 = injected.clone();
    let target2 = target_path.clone();
    let (pause_tx, pause_rx) = std::sync::mpsc::channel();
    let (resume_tx, resume_rx) = std::sync::mpsc::channel();
    let (complete_tx, complete_rx) = std::sync::mpsc::channel();
    let maintainer = move |store: &mem_core::store::Store,
                           permit: &mem_core::store::IndexMaintenancePermit<'_>|
          -> mem_core::error::MemResult<mem_core::freshness::MaintainOutcome> {
        let mut idx = mem_index::Index::open_store_under_maintenance(store, permit)?;
        let (_, _, scan_entries) = idx.rebuild_with_entries_under_maintenance(permit)?;
        idx.set_body_texts_under_maintenance(permit)?;
        if !injected2.load(Ordering::SeqCst) {
            // 测试先停在索引扫描后，让父线程确认完成信号尚未发出，再允许改写。
            pause_tx.send(()).expect("通知父测试进入可控暂停点");
            resume_rx.recv().expect("等待父测试放行改写");

            // 竞态窗口注入：索引扫描完成之后。同尺寸（r→R 等长，摘要必变）
            // + mtime 纳秒还原（绕过 stat 条件）。
            let mut edited = std::fs::read(&target2).unwrap();
            let pos = find_ascii(&edited, b"race-target").expect("原始记录含竞态标记");
            edited[pos] = b'R';
            std::fs::write(&target2, &edited).unwrap();
            set_mtime_nanos(&target2, original_mtime);

            // 发完成信号前先验证等长改写、正文摘要改变、路径与 mtime 恢复。
            let edited_bytes = std::fs::read(&target2).unwrap();
            assert_eq!(
                edited_bytes.len(),
                original_size,
                "竞态改写必须保持文件尺寸"
            );
            assert_ne!(
                mem_core::token::sha256_hex(&edited_bytes),
                original_digest,
                "等长改写必须改变完整文件摘要"
            );
            assert_eq!(
                file_mtime_nanos(&target2),
                Some(original_mtime),
                "改写后必须恢复原修改时间"
            );
            injected2.store(true, Ordering::SeqCst);
            complete_tx.send(()).expect("通知父测试改写和复核均已完成");
        }
        Ok(mem_core::freshness::MaintainOutcome {
            index_epoch: idx.epoch().unwrap_or(0),
            scan_entries,
        })
    };

    let store2 = mem_core::store::Store::open(&dir, mem_core::store::OpenMode::Auto).unwrap();
    let h = std::thread::spawn(move || {
        mem_core::sidecar::serve_with_maintainer(
            &store2,
            std::time::Duration::from_secs(6),
            || false,
            Some(&maintainer),
        )
    });

    // 可控暂停：确认改写未完成时父测试不会误将旧令牌当作竞态结果。
    pause_rx
        .recv_timeout(std::time::Duration::from_secs(8))
        .expect("维护器应在首次索引扫描后到达暂停点");
    assert!(
        matches!(
            complete_rx.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ),
        "改写尚未放行时不能发出完成信号"
    );
    resume_tx.send(()).expect("放行测试改写");
    complete_rx
        .recv_timeout(std::time::Duration::from_secs(8))
        .expect("改写、摘要变化与 mtime 恢复复核后才发完成信号");
    assert!(
        injected.load(Ordering::SeqCst),
        "完成信号必须对应已完成的改写"
    );

    let rewritten_record = mem_core::ops::get(&store, &target_id).unwrap();
    let rewritten_token = rewritten_record.token;
    assert_ne!(
        rewritten_token.digest, original_token.digest,
        "改写后的记录令牌摘要必须不同于改写前"
    );
    assert_eq!(rewritten_record.rel, target_rel, "改写不能改变记录路径");
    assert_eq!(
        std::fs::metadata(&target_path).unwrap().len(),
        original_size as u64,
        "改写必须保持文件尺寸"
    );
    assert_eq!(
        file_mtime_nanos(&target_path),
        Some(original_mtime),
        "改写必须恢复原修改时间"
    );

    // 等 sidecar 握手应答，确保首个 CLI 查询确实发生在 sidecar 已就绪之后。
    let mut sidecar_hello = None;
    for _ in 0..80 {
        if let Some(hello) = mem_core::sidecar::probe(&store) {
            sidecar_hello = Some(hello);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let hello = sidecar_hello.expect("改写完成后 sidecar 应开始响应握手");
    assert_eq!(hello.backend, "rdc", "Windows 竞态测试应覆盖 RDC 后端");
    assert_eq!(hello.undigested, None, "RDC 不得冒充查询时点新鲜证明");

    // 断言：生产 Windows 查询明确直读，并返回查询前已核验的改写后令牌。
    let out = mem()
        .args(["--store", &s, "find", "race-target", "--limit", "5"])
        .output()
        .unwrap();
    let v = j(&out);
    assert!(
        v["read_mode"]
            .as_object()
            .is_some_and(|m| m.contains_key("direct")),
        "RDC 不提供查询新鲜度证明，生产查询必须直读：{v}"
    );
    let hit_token = v["hits"][0]["token"].as_str().unwrap_or("").to_string();
    assert_eq!(
        hit_token,
        rewritten_token.to_string(),
        "r4 B3：维护期间二次写入后，第一次读取不得返回旧索引内容\n{v}"
    );
    let _ = h;
}
