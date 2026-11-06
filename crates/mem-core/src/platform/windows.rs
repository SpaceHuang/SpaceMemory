//! Windows 平台原语：LockFileEx / MoveFileExW / CreateFileW(CREATE_NEW) /
//! 进程存活判定 / 卷文件系统探测（USN 订阅能力，开放问题39）。

use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::io::AsRawHandle;
use std::path::Path;

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND,
    ERROR_INVALID_PARAMETER, ERROR_SHARING_VIOLATION, HANDLE, STILL_ACTIVE,
};
use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, GetVolumeInformationW, LockFileEx, MoveFileExW, UnlockFileEx,
    CREATE_NEW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED,
    FILE_SHARE_READ, FILE_SHARE_WRITE, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
    MOVEFILE_REPLACE_EXISTING, OPEN_EXISTING,
};
use windows_sys::Win32::System::IO::OVERLAPPED;
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    WaitForSingleObject,
};

/// 锁哨兵字节偏移（1 MiB）：owner JSON 驻留 0 偏移、锁范围与之不重叠——
/// 其他句柄可自由读写 owner 信息，锁仲裁只看哨兵字节。全库统一同一范围。
pub const LOCK_OFFSET: u64 = 1 << 20;
const LOCK_RANGE: u32 = 1;

fn to_wide(p: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    let mut v: Vec<u16> = p.as_os_str().encode_wide().collect();
    v.push(0);
    v
}

/// 文件字节范围锁句柄（Drop 即解锁+关闭）。
///
/// Windows 锁按**句柄**：同进程两个句柄同样互斥（多线程并发安全的底座）。
pub struct LockFile {
    file: File,
}

impl LockFile {
    /// 通过**持锁句柄**覆写文件内容（锁文件 owner JSON）——避免用第二句柄写
    /// 已锁字节范围（Windows ERROR_LOCK_VIOLATION）。
    pub fn overwrite(&self, bytes: &[u8]) -> io::Result<()> {
        use std::io::{Seek, SeekFrom, Write};
        let mut f = &self.file;
        f.seek(SeekFrom::Start(0))?;
        f.write_all(bytes)?;
        f.set_len(bytes.len() as u64)?;
        f.flush()
    }
}

impl std::fmt::Debug for LockFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LockFile").finish_non_exhaustive()
    }
}

impl LockFile {
    /// （已被 [`Self::open_locked`] 取代的旧路径——保留给无锁竞争场景的直开语义。）
    #[allow(dead_code)]
    fn open(path: &Path) -> io::Result<File> {
        OpenOptions::new().read(true).write(true).create(true).open(path)
    }

    /// 打开锁文件（read+write）。Windows 已知行为：任何句柄持有独占字节范围锁时，
    /// 他者以写访问打开整个文件会得到 ERROR_LOCK_VIOLATION(33)——归一为 WouldBlock
    /// 语义（try）/ 重试（blocking）。
    fn open_locked(path: &Path, blocking: bool) -> io::Result<File> {
        loop {
            match OpenOptions::new().read(true).write(true).create(true).open(path) {
                Ok(f) => return Ok(f),
                Err(e) if e.raw_os_error() == Some(33) => {
                    if blocking {
                        std::thread::sleep(std::time::Duration::from_millis(15));
                        continue;
                    }
                    return Err(io::Error::from(io::ErrorKind::WouldBlock));
                }
                Err(e) => return Err(e),
            }
        }
    }

    fn lock(file: &File, exclusive: bool, fail_immediately: bool) -> io::Result<()> {
        let mut flags = 0u32;
        if exclusive {
            flags |= LOCKFILE_EXCLUSIVE_LOCK;
        }
        if fail_immediately {
            flags |= LOCKFILE_FAIL_IMMEDIATELY;
        }
        let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
        overlapped.Anonymous.Anonymous.Offset = (LOCK_OFFSET & 0xFFFF_FFFF) as u32;
        overlapped.Anonymous.Anonymous.OffsetHigh = (LOCK_OFFSET >> 32) as u32;
        loop {
            let ok = unsafe {
                LockFileEx(file.as_raw_handle() as HANDLE, flags, 0, LOCK_RANGE, 0, &mut overlapped)
            };
            if ok != 0 {
                return Ok(());
            }
            let e = io::Error::last_os_error();
            // ERROR_LOCK_VIOLATION(33)：范围已被锁——try → WouldBlock；blocking → 重试。
            if e.raw_os_error() == Some(33) {
                if fail_immediately {
                    return Err(io::Error::from(io::ErrorKind::WouldBlock));
                }
                std::thread::sleep(std::time::Duration::from_millis(15));
                continue;
            }
            return Err(e);
        }
    }

    fn unlock(file: &File) {
        let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
        overlapped.Anonymous.Anonymous.Offset = (LOCK_OFFSET & 0xFFFF_FFFF) as u32;
        overlapped.Anonymous.Anonymous.OffsetHigh = (LOCK_OFFSET >> 32) as u32;
        unsafe {
            UnlockFileEx(file.as_raw_handle() as HANDLE, 0, LOCK_RANGE, 0, &mut overlapped);
        }
    }

    pub fn acquire_exclusive(path: &Path) -> io::Result<LockFile> {
        let file = Self::open_locked(path, true)?;
        Self::lock(&file, true, false)?;
        Ok(LockFile { file })
    }

    pub fn try_acquire_exclusive(path: &Path) -> io::Result<LockFile> {
        let file = Self::open_locked(path, false)?;
        Self::lock(&file, true, true)?;
        Ok(LockFile { file })
    }

    pub fn acquire_shared(path: &Path) -> io::Result<LockFile> {
        let file = Self::open_locked(path, true)?;
        Self::lock(&file, false, false)?;
        Ok(LockFile { file })
    }

    pub fn try_acquire_shared(path: &Path) -> io::Result<LockFile> {
        let file = Self::open_locked(path, false)?;
        Self::lock(&file, false, true)?;
        Ok(LockFile { file })
    }
}

impl Drop for LockFile {
    fn drop(&mut self) {
        Self::unlock(&self.file);
    }
}

/// 进程存活判定（陈旧锁，红线22）。
///
/// OpenProcess 失败（含 ERROR_INVALID_PARAMETER / ACCESS_DENIED 之外的不可查询态）
/// 保守判死——抢锁的代价是对方必须重抢，判活错误的代价是无限等待；保守取向如上。
pub fn process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return false;
        }
        let mut code: u32 = 0;
        let ok = GetExitCodeProcess(h, &mut code);
        CloseHandle(h);
        if ok == 0 {
            return false;
        }
        // STILL_ACTIVE(259) 是"进程仍在运行"的退出码（注意语义不是"已停止"）。
        code == STILL_ACTIVE as u32
    }
}

/// temp + 原子改名落盘（R3.1/红线6）：同目录临时文件 + MoveFileEx(REPLACE_EXISTING)。
pub fn atomic_write(target: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = target.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "target 无父目录")
    })?;
    let name = target
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "target".into());
    // M6c：temp 名不以 .md 结尾——并发写窗口内的 temp 文件不会被
    // enumerate_md 当作记录条目（此前产生坏条目告警噪音 + stat 闪烁）。
    let tmp = dir.join(format!(
        "{}.tmp-{}-{}",
        name,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    ));
    let write_res = (|| {
        let mut f = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        io::Write::write_all(&mut f, bytes)?;
        f.sync_all()
    })();
    if let Err(e) = write_res {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    let wide = to_wide(&tmp);
    let twide = to_wide(target);
    let ok = unsafe {
        MoveFileExW(wide.as_ptr(), twide.as_ptr(), MOVEFILE_REPLACE_EXISTING)
    };
    if ok == 0 {
        let err = io::Error::last_os_error();
        let _ = std::fs::remove_file(&tmp);
        return Err(err);
    }
    Ok(())
}

/// 原子独占创建（R3.3 唯一键路径独占的底层语义）：已存在 → Err。
pub fn create_new(target: &Path, bytes: &[u8]) -> io::Result<()> {
    // r4 B5：崩溃原子 + 唯一性（R3.1/R3.3/红线6）——完整字节先写同目录临时文件
    //（非 .md 后缀，不进记录枚举）并同步，再以**不覆盖**的 MoveFileExW 原子发布：
    // 目标已存在 → ERROR_ALREADY_EXISTS 且目标分毫不动（输者协议依据）。
    // 此前直接对正式路径 create_new+write，崩溃/短写会在规范路径留下半条记录。
    let dir = target.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "target 无父目录")
    })?;
    let name = target
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "target".into());
    let tmp = dir.join(format!(
        "{name}.tmp-create-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let write_res = (|| {
        let mut f = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        use io::Write;
        f.write_all(bytes)?;
        f.sync_all()
    })();
    if let Err(e) = write_res {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    let twide = to_wide(&tmp);
    let gw = to_wide(target);
    // 不带 MOVEFILE_REPLACE_EXISTING：目标存在 → ERROR_ALREADY_EXISTS，移动原子。
    let ok = unsafe { MoveFileExW(twide.as_ptr(), gw.as_ptr(), 0) };
    if ok == 0 {
        let e = io::Error::last_os_error();
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// 卷文件系统名（订阅能力探测：NTFS → USN 可用；exFAT/FAT → 永久直读，开放问题39）。
pub fn fs_name_for(path: &Path) -> Option<String> {
    let wide = to_wide(path);
    let _buf = [0u16; 64];
    let mut fs_name = [0u16; 64];
    let ok = unsafe {
        GetVolumeInformationW(
            wide.as_ptr(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            fs_name.as_mut_ptr(),
            fs_name.len() as u32,
        )
    };
    if ok == 0 {
        return None;
    }
    let len = fs_name.iter().position(|&c| c == 0).unwrap_or(fs_name.len());
    Some(String::from_utf16_lossy(&fs_name[..len]))
}

/// GetLastError 是否为"文件不存在/访问拒绝/共享冲突/参数非法"之一
/// （供锁层与落盘层分类处理）。
pub fn last_error_kind() -> io::ErrorKind {
    unsafe {
        match GetLastError() {
            ERROR_FILE_NOT_FOUND => io::ErrorKind::NotFound,
            ERROR_ACCESS_DENIED => io::ErrorKind::PermissionDenied,
            ERROR_SHARING_VIOLATION => io::ErrorKind::WouldBlock,
            ERROR_INVALID_PARAMETER => io::ErrorKind::InvalidInput,
            _ => io::ErrorKind::Other,
        }
    }
}

/// 保留：FILE_FLAG_FIRST_PIPE_INSTANCE 在 sidecar 单实例握手（§8-6）中使用。
#[allow(dead_code)]
const _PIPE_FLAG_USED_LATER: u32 = FILE_FLAG_FIRST_PIPE_INSTANCE;
#[allow(dead_code)]
const _OPEN_EXISTING_USED_LATER: u32 = OPEN_EXISTING;
#[allow(dead_code)]
const _CREATE_NEW_USED_LATER: u32 = CREATE_NEW;
#[allow(dead_code)]
type _Handle = HANDLE;

// ===================== USN journal 订阅（D5 证明式新鲜性的覆盖来源） =====================
//
// FRN 快照法：物化时记录 store 记录文件的 File Reference Number 集合；此后 USN
// journal 流中命中该集合的记录 = 未消化变更（内容级修改即使保 mtime 同尺寸也被
// 捕获——这正是红线21 要求的覆盖）。卷不支持（exFAT/FAT/网络盘）→ None → 永久直读。

const FSCTL_QUERY_USN_JOURNAL: u32 = 0x0009_00E8;
const FSCTL_READ_USN_JOURNAL: u32 = 0x0009_00EB;

#[repr(C)]
#[derive(Clone, Copy)]
struct UsnJournalData {
    usn_journal_id: u64,
    next_usn: i64,
    lowest_valid_usn: i64,
    max_usn: i64,
    maximum_size: u64,
    allocation_delta: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ReadUsnJournalData {
    start_usn: i64,
    reason_mask: u32,
    return_only_on_close: u32,
    timeout: u64,
    bytes_to_wait_for: u64,
    usn_journal_id: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct UsnRecordV2 {
    record_length: u32,
    major_minor: u16,
    file_reference_number: u64,
    parent_file_reference_number: u64,
    usn: i64,
    timestamp: u64,
    reason: u32,
    source_info: u32,
    security_id: u32,
    file_attributes: u32,
    file_name_length: u32,
    file_name_offset: u32,
}

/// 记录文件的 FRN（File Reference Number）。
pub fn file_frn(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_SHARE_READ,
        FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    wide.push(0);
    unsafe {
        let h = CreateFileW(
            wide.as_ptr(),
            0x8000_0000, // GENERIC_READ
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        );
        if h.is_null() {
            return None;
        }
        let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
        let ok = GetFileInformationByHandle(h, &mut info);
        CloseHandle(h);
        if ok == 0 {
            return None;
        }
        Some(
            ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64,
        )
    }
}

fn volume_device_path(path: &Path) -> Option<String> {
    // "D:\a\b" → "\\.\D:"（盘符卷设备路径）。
    let s = path.to_string_lossy();
    let mut chars = s.chars();
    let drive = chars.next()?;
    if chars.next() != Some(':') {
        return None;
    }
    Some(format!(r"\\.\{}:", drive))
}

/// USN journal 订阅（单店一份；sidecar 持有）。
pub struct UsnWatcher {
    volume: File,
    journal_id: u64,
    next_usn: i64,
    frn_set: std::collections::HashSet<u64>,
    /// M12：握手路径维护失败时回注的未消化计数（语义同 RdcWatcher.pending）。
    pending: std::cell::Cell<u64>,
}

impl UsnWatcher {
    /// 建立订阅：FRN 快照 + journal 定位。失败（非 NTFS 等）→ None。
    pub fn attach(store_root: &Path, record_paths: &[&Path]) -> Option<UsnWatcher> {
        let dev = volume_device_path(store_root)?;
        let mut wide: Vec<u16> = dev.encode_utf16().collect();
        wide.push(0);
        unsafe {
            let volume = CreateFileW(
                wide.as_ptr(),
                0x8000_0000, // GENERIC_READ
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            );
            if volume.is_null() || volume == -1isize as _ {
                return None;
            }
            let volume = File::from_raw_handle(volume as _);
            let mut qjd: UsnJournalData = std::mem::zeroed();
            let mut bytes_ret = 0u32;
            let ok = DeviceIoControl(
                volume.as_raw_handle(),
                FSCTL_QUERY_USN_JOURNAL,
                std::ptr::null(),
                0,
                &mut qjd as *mut _ as *mut core::ffi::c_void,
                std::mem::size_of::<UsnJournalData>() as u32,
                &mut bytes_ret,
                std::ptr::null_mut(),
            );
            if ok == 0 {
                return None;
            }
            let mut frn_set = std::collections::HashSet::new();
            for p in record_paths {
                if let Some(frn) = file_frn(p) {
                    frn_set.insert(frn);
                }
            }
            Some(UsnWatcher {
                volume,
                journal_id: qjd.usn_journal_id,
                next_usn: qjd.next_usn,
                frn_set,
                pending: std::cell::Cell::new(0),
            })
        }
    }

    /// 订阅 FRN 更新（物化新文件后调用）。
    pub fn track(&mut self, path: &Path) {
        if let Some(frn) = file_frn(path) {
            self.frn_set.insert(frn);
        }
    }

    /// M12：维护失败的信号回注。
    pub fn reinject(&self, n: u64) {
        self.pending.set(self.pending.get() + n);
    }

    /// 自上次推进点以来的未消化变更数（Some = 订阅有效；None = 订阅失效）。
    pub fn undigested_changes(&mut self) -> Option<u64> {
        let mut count = self.pending.get();
        self.pending.set(0);
        let mut current = self.next_usn;
        // 循环读取直到追平（BytesToWaitFor=0：立即返回现有记录）。
        loop {
            let rq = ReadUsnJournalData {
                start_usn: current,
                reason_mask: 0xFFFF_FFFF,
                return_only_on_close: 0,
                timeout: 0,
                bytes_to_wait_for: 0,
                usn_journal_id: self.journal_id,
            };
            let mut buf = [0u8; 64 * 1024];
            let mut bytes_ret = 0u32;
            unsafe {
                let ok = DeviceIoControl(
                    self.volume.as_raw_handle(),
                    FSCTL_READ_USN_JOURNAL,
                    &rq as *const _ as *const core::ffi::c_void,
                    std::mem::size_of::<ReadUsnJournalData>() as u32,
                    buf.as_mut_ptr() as *mut core::ffi::c_void,
                    buf.len() as u32,
                    &mut bytes_ret,
                    std::ptr::null_mut(),
                );
                if ok == 0 {
                    return None; // journal 被删/卷失效 → 订阅失效（不可证明 → 直读）
                }
            }
            let mut offset = 8usize; // 跳过前导 NextUsn
            let available = bytes_ret as usize;
            if available <= offset {
                break;
            }
            while offset + std::mem::size_of::<UsnRecordV2>() <= available {
                let rec = unsafe { std::ptr::read_unaligned(buf.as_ptr().add(offset) as *const UsnRecordV2) };
                if rec.record_length == 0 {
                    break;
                }
                if rec.reason != 0 && self.frn_set.contains(&rec.file_reference_number) {
                    count += 1;
                }
                current = rec.usn + rec.record_length as i64;
                offset += rec.record_length as usize;
            }
            if available <= 8 + std::mem::size_of::<UsnRecordV2>() {
                // 没有完整记录：追平。
                let next = i64::from_le_bytes(buf[0..8].try_into().ok()?);
                if next == current {
                    break;
                }
            }
            self.next_usn = current;
        }
        self.next_usn = current;
        Some(count)
    }

    /// 消化确认（索引物化完成后推进基线）。
    pub fn mark_consumed(&mut self) {
        // 基线即当前 next_usn：读取例程已推进；无需额外操作。
    }
}

// DeviceIoControl 从 Win32::System::IO。
use windows_sys::Win32::System::IO::DeviceIoControl;
use std::os::windows::io::FromRawHandle;


// ===================== RDC 订阅（ReadDirectoryChangesW，H7） =====================
//
// USN journal 需要卷句柄——非提权进程 OpenVolume 被拒（H7 实测：标准 Windows 用户
// 环境快路径永不生效）。RDC（目录变更通知）无需提权、内核过滤驱动产生事件
// （写操作本身触发通知，非 stat 比对——同尺寸保 mtime 的写入同样被捕获），
// 是红线21 语义下合法的 OS 变更日志订阅源。
//
// 溢出语义：通知缓冲溢出（ERROR_NOTIFY_ENUM_DIR）= 覆盖缺口 → 永久 None（不可
// 证明 → 直读），与 USN journal 被删同等处理。open question 39：RDC 为实施侧
// 裁决，待设计收口。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

pub const ERROR_NOTIFY_ENUM_DIR: u32 = 1022;

/// FILE_NOTIFY_INFORMATION 的布局参照（逐字段手动解析，不构造此类型——保留作
/// 缓冲布局文档）。
#[allow(dead_code)]
#[repr(C)]
struct FileNotifyInformation {
    next_entry_offset: u32,
    action: u32,
    file_name_length: u32,
    _file_name: u16, // 变长数组起点
}

/// RDC 订阅（watch 目录整棵子树；线程持续读取，计数器聚合）。
pub struct RdcWatcher {
    inner: Arc<RdcInner>,
    watch_dir: std::path::PathBuf,
}

struct RdcInner {
    undigested: AtomicU64,
    /// M12：握手路径维护失败时回注的未消化计数（下次 fresh 查询返回，保证
    /// 变更信号不因一次维护失败而丢失——与 tick 路径"失败即终止 serve"汇合）。
    pending: AtomicU64,
    /// r12：watcher 线程已处理的**哨兵事件**计数——fresh 查询的排空栅栏。
    sentinel_seen: AtomicU64,
    gap: AtomicBool,
    stop: AtomicBool,
    dir_handle: std::sync::atomic::AtomicIsize,
}

impl RdcWatcher {
    /// 建立订阅。`ignore_name_parts`：**末段组件以这些模式结尾**的通知不计
    /// （*.lock 为机制自身副产物；r13 B9-4 收紧——此前"整条相对路径含子串"
    /// 的匹配会把文件名含该子串的记录文件事件一并吞掉，违反红线21）。
    pub fn attach(watch_dir: &Path, ignore_name_parts: &[&str]) -> Option<RdcWatcher> {
        use std::os::windows::ffi::OsStrExt;
        let mut wide: Vec<u16> = watch_dir.as_os_str().encode_wide().collect();
        wide.push(0);
        let ignore: Vec<String> = ignore_name_parts.iter().map(|s| s.to_string()).collect();
        unsafe {
            let handle = CreateFileW(
                wide.as_ptr(),
                0x8000_0000u32, // GENERIC_READ（目录列表权限即可）
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
                std::ptr::null_mut(),
            );
            if handle.is_null() || handle == -1isize as HANDLE {
                return None; // 目录打不开 → 订阅不可用（调用方降级）
            }
            // r12：创建哨兵文件（fresh 查询的排空栅栏——见 fresh_undigested）。
            let sentinel = watch_dir.join(".rdc-sentinel");
            let _ = std::fs::write(&sentinel, b"");
            let inner = Arc::new(RdcInner {
                undigested: AtomicU64::new(0),
                pending: AtomicU64::new(0),
                sentinel_seen: AtomicU64::new(0),
                gap: AtomicBool::new(false),
                stop: AtomicBool::new(false),
                dir_handle: std::sync::atomic::AtomicIsize::new(handle as isize),
            });
            let thread_inner = Arc::clone(&inner);
            // HANDLE 非 Send：线程内经 isize 原样重建（同一句柄的所有权移交线程）。
            let handle_val = handle as isize;
            std::thread::Builder::new()
                .name("mem-rdc".into())
                .spawn(move || {
                    rdc_read_loop(handle_val as HANDLE, &thread_inner, &ignore);
                })
                .ok()?;
            Some(RdcWatcher { inner, watch_dir: watch_dir.to_path_buf() })
        }
    }

    /// 新鲜查询 + 消化（B4/r12：**哨兵排空**——严格同代证明）。
    ///
    /// RDC 通知是异步投递的：仅读当前计数会漏掉"已写入、通知尚在投递中"的
    /// 变更（r12 复现 B4 竞态）。本方法利用同目录句柄**通知按序投递**的保证：
    /// 向哨兵文件（watch 目录内、不计入变更数）写入一次，内核完成 pending
    /// 通知时该批**必然已包含此前全部写入**——watcher 线程处理完哨兵事件后，
    /// 计数即为严格新鲜值。等待哨兵事件超时（watcher 卡顿）→ None（不可证明
    /// → 直读，安全侧）。
    pub fn fresh_undigested(&self) -> Option<u64> {
        if self.inner.gap.load(Ordering::SeqCst) {
            return None;
        }
        // 排空栅栏：追加哨兵 → 等 watcher 处理完哨兵事件（至多 ~500ms）。
        let target = self.inner.sentinel_seen.load(Ordering::SeqCst) + 1;
        if let Some(sp) = self.sentinel_dir() {
            if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&sp) {
                use std::io::Write;
                let _ = f.write_all(b" ");
                let _ = f.flush();
            }
        }
        for _ in 0..50 {
            if self.inner.sentinel_seen.load(Ordering::SeqCst) >= target {
                return Some(self.inner.pending.swap(0, Ordering::SeqCst)
                    + self.inner.undigested.swap(0, Ordering::SeqCst));
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        None // 哨兵事件未按时处理：不可证明（直读，安全侧）
    }

    fn sentinel_dir(&self) -> Option<std::path::PathBuf> {
        // 哨兵文件路径 = watch 目录 + .rdc-sentinel（由 attach 记录）。
        Some(self.watch_dir.join(".rdc-sentinel"))
    }

    /// M12：维护失败的信号回注（下次 fresh 查询返回 → 重试维护）。
    pub fn reinject(&self, n: u64) {
        self.inner.pending.fetch_add(n, Ordering::SeqCst);
    }
}

impl Drop for RdcWatcher {
    fn drop(&mut self) {
        self.inner.stop.store(true, Ordering::SeqCst);
        unsafe {
            CancelIoEx(self.inner.dir_handle.load(Ordering::SeqCst) as HANDLE, std::ptr::null());
        }
        // 线程被唤醒后见到 stop 退出；句柄由线程关闭。
    }
}

fn rdc_read_loop(handle: HANDLE, inner: &Arc<RdcInner>, ignore: &[String]) {
    unsafe {
        let event = CreateEventW(std::ptr::null(), 0, 0, std::ptr::null());
        if event.is_null() {
            // H8：订阅线程起不来 = 订阅死亡——必须置 gap（永久不可证明），
            // 否则 watcher 存活而事件流中断 = 僵尸订阅，证明名存实亡。
            inner.gap.store(true, Ordering::SeqCst);
            return;
        }
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            if inner.stop.load(Ordering::SeqCst) {
                CloseHandle(handle);
                CloseHandle(event);
                return;
            }
            let mut ov: OVERLAPPED = std::mem::zeroed();
            ov.hEvent = event;
            let mut bytes_ret = 0u32;
            let ok = ReadDirectoryChangesW(
                handle,
                buf.as_mut_ptr() as *mut core::ffi::c_void,
                buf.len() as u32,
                1, // watch subtree（records/ 深层键路径）
                FILE_NOTIFY_CHANGE_FILE_NAME
                    | FILE_NOTIFY_CHANGE_SIZE
                    | FILE_NOTIFY_CHANGE_LAST_WRITE
                    | FILE_NOTIFY_CHANGE_ATTRIBUTES,
                &mut bytes_ret,
                &mut ov,
                None, // LPOVERLAPPED_COMPLETION_ROUTINE（重叠 I/O 用事件通知）
            );
            if ok == 0 {
                // H8：ReadDirectoryChangesW 调用失败 = 事件流中断 → 置 gap（同上）。
                inner.gap.store(true, Ordering::SeqCst);
                CloseHandle(handle);
                CloseHandle(event);
                return;
            }
            // 阻塞等待本批通知（stop 由 CancelIoEx 解阻塞）。
            let w = WaitForSingleObject(event, 60_000);
            if inner.stop.load(Ordering::SeqCst) {
                CloseHandle(handle);
                CloseHandle(event);
                return;
            }
            if w != WAIT_OBJECT_0 {
                continue; // 超时：回环查 stop
            }
            let mut got = 0u32;
            if GetOverlappedResult(handle, &ov, &mut got, 0) == 0 {
                // 任何结果读取失败都是事件流缺口（含 ENUM_DIR 溢出）——永久不可证明。
                inner.gap.store(true, Ordering::SeqCst);
                CloseHandle(handle);
                CloseHandle(event);
                return;
            }
            if got == 0 {
                continue;
            }
            // 解析 FILE_NOTIFY_INFORMATION 链。
            let mut offset = 0usize;
            while offset + 12 <= got as usize {
                let next = u32::from_le_bytes(buf[offset..offset + 4].try_into().unwrap());
                let action = u32::from_le_bytes(buf[offset + 4..offset + 8].try_into().unwrap());
                let name_len =
                    u32::from_le_bytes(buf[offset + 8..offset + 12].try_into().unwrap()) as usize;
                if action == 0 {
                    break;
                }
                let name_start = offset + 12;
                if name_start + name_len <= got as usize {
                    let name_w: Vec<u16> = buf[name_start..name_start + name_len]
                        .chunks_exact(2)
                        .map(|c| u16::from_le_bytes([c[0], c[1]]))
                        .collect();
                    let name = String::from_utf16_lossy(&name_w);
                    // r12：哨兵文件事件 = 排空栅栏信号（不计入变更数）。
                    // r13 B9-4：**精确匹配**监视根下的哨兵路径本身（Windows 路径
                    // 大小写不敏感）——此前子串匹配把文件名含 ".rdc-sentinel" 的
                    // 记录文件事件一并吞掉（外部重命名/放置是设计承认的覆盖域），
                    // 该记录的新鲜性证明永久失效（红线21）。
                    if name.eq_ignore_ascii_case(".rdc-sentinel") {
                        inner.sentinel_seen.fetch_add(1, Ordering::SeqCst);
                    } else {
                        // r13 B9-4 同族收紧：ignore 只按**末段组件的后缀**匹配
                        //（机制锁文件形如 <name>.lock）——".tmp-" 中缀不再过滤：
                        // 机制 tmp 文件伴随真实写入出现，多计数只多触发一轮维护
                        //（安全侧）；误吞记录事件则不可接受。
                        let last = name.rsplit(['\\', '/']).next().unwrap_or(&name);
                        let ignored = ignore.iter().any(|p| last.ends_with(p.as_str()));
                        if !ignored {
                            inner.undigested.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                }
                if next == 0 {
                    break;
                }
                offset += next as usize;
            }
        }
    }
}

use windows_sys::Win32::Storage::FileSystem::{
    ReadDirectoryChangesW, FILE_NOTIFY_CHANGE_ATTRIBUTES, FILE_NOTIFY_CHANGE_FILE_NAME,
    FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_SHARE_DELETE,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult};


#[cfg(test)]
mod rdc_tests {
    use super::*;

    #[test]
    fn rdc_counts_changes_and_drains_but_ignores_mechanism_files() {
        let tmp = tempfile::tempdir().unwrap();
        let watch = tmp.path().join("records");
        std::fs::create_dir_all(&watch).unwrap();
        let w = RdcWatcher::attach(&watch, &[".lock"]).expect("目录可订阅");
        // 机制锁文件：不计（*.lock 末段后缀，r13 B9-4 收紧后的规则）。
        std::fs::write(watch.join("x.md.lock"), b"{}").unwrap();
        // tmp 命名文件伴随真实写入出现：按安全侧计数（多计数只多一轮维护，
        // 误吞才违反红线21——r13 B9-4 起 .tmp- 不再过滤）。
        std::fs::write(watch.join("y.md.tmp-123-1"), b"t").unwrap();
        // 真实变更：计数。
        std::fs::write(watch.join("a.md"), b"one").unwrap();
        // 通知异步到达：轮询等待计数。
        let mut saw_change = false;
        for _ in 0..50 {
            let r = w.fresh_undigested();
            if let Some(n) = r {
                if n > 0 {
                    saw_change = true;
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(saw_change, "同尺寸写入必须产生通知（内核写操作触发，非 stat 比对）");
        // 消化后归零。
        let n = w.fresh_undigested().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert_eq!(w.fresh_undigested(), Some(0), "消化后未消化计数为 0（残留 {n}）");
        // 同尺寸保 mtime 写入（红线21 立法场景）：仍被捕获。
        std::thread::sleep(std::time::Duration::from_millis(50));
        std::fs::write(watch.join("a.md"), b"two").unwrap(); // 同字节数
        let mut saw_again = false;
        for _ in 0..50 {
            if let Some(n) = w.fresh_undigested() {
                if n > 0 {
                    saw_again = true;
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(saw_again, "同尺寸内容修改必须被 RDC 捕获（B4/H7 场景的根本保障）");
    }

    #[test]
    fn rdc_sentinel_substring_record_events_counted() {
        // r13 B9-4：文件名含哨兵子串的记录文件（外部放置覆盖域）——事件必须计数，
        // 不得被当作哨兵信号吞掉（吞掉 → fresh Some(0) → 新鲜性误判 indexed →
        // 新内容漏命中，红线21）。
        let tmp = tempfile::tempdir().unwrap();
        let watch = tmp.path().join("records");
        std::fs::create_dir_all(watch.join("topic")).unwrap();
        let w = RdcWatcher::attach(&watch, &[".lock"]).expect("目录可订阅");
        std::fs::write(watch.join("topic/x.rdc-sentinel.md"), b"one").unwrap();
        let mut counted = false;
        for _ in 0..50 {
            if let Some(n) = w.fresh_undigested() {
                if n > 0 {
                    counted = true;
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(counted, "含哨兵子串文件名的记录变更不得被吞（B9-4/红线21）");
        // 收紧后的 ignore：仅 *.lock 末段后缀忽略（机制锁文件）。
        std::thread::sleep(std::time::Duration::from_millis(50));
        std::fs::write(watch.join("y.md.lock"), b"{}").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert_eq!(w.fresh_undigested(), Some(0), "*.lock 机制后缀仍被忽略");
    }

    #[test]
    fn rdc_reinject_preserves_signal_for_retry() {
        // M12：握手路径维护失败 → reinject 回注——信号不因一次维护失败而丢失。
        let tmp = tempfile::tempdir().unwrap();
        let watch = tmp.path().join("records");
        std::fs::create_dir_all(&watch).unwrap();
        let w = RdcWatcher::attach(&watch, &[".lock"]).unwrap();
        w.reinject(7);
        // 回注的计数在下一次 fresh 查询返回（+ 期间的真实事件）。
        let mut saw = false;
        for _ in 0..50 {
            if let Some(n) = w.fresh_undigested() {
                if n >= 7 {
                    saw = true;
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(saw, "回注信号必须在下一次新鲜查询返回");
        // 消化后归零（回注不泄漏为永久计数）。
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert_eq!(w.fresh_undigested(), Some(0));
    }
}
