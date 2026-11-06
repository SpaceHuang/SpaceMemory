//! r4 B3 维护竞态回归：维护期间二次写入不得被检查点"提前确认"。
//!
//! 评审复现方式：使用与生产 serve 相同的真实 mem-index rebuild 物化器，在每次
//! rebuild 之后（索引扫描完成之后、检查点发布之前——生产顺序允许的竞态窗口）
//! 注入一次**同尺寸 + mtime 纳秒还原**的修改，刻意绕过 stat 条件。
//!
//! 修复前：检查点包含新文件状态而索引是旧内容 → Hello 宣称无未消化 + stat 一致
//! → 旧索引被判新鲜（indexed 返回旧令牌）。
//! 修复后（收敛原语）：维护后**再次新鲜查询**，>0 继续维护直至收敛；收敛时检查
//! 点与索引同代（index_epoch 核对）→ indexed 模式的令牌 == 磁盘当前令牌。

use assert_cmd::Command;
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

fn mem() -> Command {
    Command::cargo_bin("mem").expect("mem 二进制")
}

fn j(out: &std::process::Output) -> Value {
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("输出不是 JSON：{e}\n{}", String::from_utf8_lossy(&out.stdout)))
}

fn find_ascii(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(windows)]
fn file_mtime_nanos(path: &std::path::Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
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
fn file_mtime_nanos(_path: &std::path::Path) -> Option<u64> {
    None
}

#[cfg(windows)]
fn set_mtime_nanos(path: &std::path::Path, ft100: u64) {
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
fn set_mtime_nanos(_path: &std::path::Path, _ft100: u64) {}

#[test]
fn maintenance_race_second_write_not_confirmed_early() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    let s = dir.to_str().unwrap().to_string();
    mem().args(["--store", &s, "init", &s, "--principal", "codex_main"]).output().unwrap();
    let out_t = mem()
        .args(["--store", &s, "add", "memory", "--payload", r#"{"title":"race-target"}"#])
        .output()
        .unwrap();
    assert!(out_t.status.success());
    let target_id = j(&out_t)["id"].as_str().unwrap().to_string();
    let store = mem_core::store::Store::open(&dir, mem_core::store::OpenMode::Auto).unwrap();
    let target_rel = mem_core::ops::get(&store, &target_id).unwrap().rel;
    let target_path = dir.join(&target_rel);
    assert!(target_path.is_file());

    // 注入式维护器：第一次 rebuild 后，在竞态窗口注入同尺寸 + mtime 还原的修改。
    let injected = Arc::new(AtomicBool::new(false));
    let injected2 = injected.clone();
    let target2 = target_path.clone();
    let maintainer = move |store: &mem_core::store::Store|
        -> mem_core::error::MemResult<mem_core::freshness::MaintainOutcome>
    {
        let mut idx = mem_index::Index::open_store(store)?;
        let (_, _, scan_entries) = idx.rebuild_with_entries(store)?;
        idx.set_body_texts(store)?;
        if !injected2.swap(true, Ordering::SeqCst) {
            // 竞态窗口注入：索引扫描完成之后。同尺寸（r→R 等长，摘要必变）
            // + mtime 纳秒还原（绕过 stat 条件）。
            let mtime_before = file_mtime_nanos(&target2);
            let mut edited = std::fs::read(&target2).unwrap();
            if let Some(pos) = find_ascii(&edited, b"race-target") {
                edited[pos] = b'R';
            }
            std::fs::write(&target2, &edited).unwrap();
            if let Some(ns) = mtime_before {
                set_mtime_nanos(&target2, ns);
            }
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

    // 等收敛：注入发生后，收敛原语必须再维护一轮（fresh 查询 >0 → continue）。
    // 收敛后检查点 index_epoch >= 2（注入轮 + 收敛轮）。
    let mut converged = false;
    for _ in 0..80 {
        let store3 = mem_core::store::Store::open(&dir, mem_core::store::OpenMode::Auto).unwrap();
        if let Some(cp) = mem_core::freshness::load_checkpoint(&store3) {
            if cp.index_epoch >= 2 {
                converged = true;
                break;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(
        converged,
        "维护期间二次写入必须触发再维护（收敛原语），不得被检查点提前确认"
    );

    // 断言：indexed 模式第一次读取的令牌 == 磁盘现状令牌（修复前：旧索引令牌）。
    let out = mem()
        .args(["--store", &s, "find", "race-target", "--limit", "5"])
        .output()
        .unwrap();
    let v = j(&out);
    let hit_token = v["hits"][0]["token"].as_str().unwrap_or("").to_string();
    let fresh = mem_core::ops::get(&store, &target_id).unwrap();
    assert_eq!(
        hit_token, fresh.token.to_string(),
        "r4 B3：维护期间二次写入后，第一次读取不得返回旧索引内容\n{v}"
    );
    let _ = h;
}
