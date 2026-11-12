//! macOS 平台原语：fcntl(F_SETLK/F_SETLKW) / rename(2) / create_new /
//! 进程存活判定。
//!
//! 注意：fcntl 字节范围锁是**按进程**的（同进程不互斥）——同进程内的并发互斥由
//! lock.rs 的进程内注册表补足。FSEvents 订阅在 §8-6 落地；v1 探测保守返回
//! None（→ 永久直读形态，正确性不依赖订阅，D5/红线21）。

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::io::AsRawFd;
use std::path::Path;

/// 文件字节范围锁句柄（Drop 即解锁+关闭）。
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

/// 目标必须不存在时原子改名（tombstone 发布；目标冲突不会替换旧副本）。
pub fn rename_noreplace(source: &Path, target: &Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let from = std::ffi::CString::new(source.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "source 路径含 NUL"))?;
    let to = std::ffi::CString::new(target.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "target 路径含 NUL"))?;
    let rc = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) };
    if rc == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

impl std::fmt::Debug for LockFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LockFile").finish_non_exhaustive()
    }
}

impl LockFile {
    fn open(path: &Path) -> io::Result<File> {
        OpenOptions::new().read(true).write(true).create(true).open(path)
    }

    fn open_existing(path: &Path) -> io::Result<File> {
        OpenOptions::new().read(true).write(true).open(path)
    }

    fn lock(file: &File, exclusive: bool, blocking: bool) -> io::Result<()> {
        let mut op = if exclusive { libc::LOCK_EX } else { libc::LOCK_SH };
        if !blocking { op |= libc::LOCK_NB; }
        let rc = unsafe { libc::flock(file.as_raw_fd(), op) };
        if rc == -1 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn unlock(file: &File) {
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN); }
    }

    pub fn acquire_exclusive(path: &Path) -> io::Result<LockFile> {
        let file = Self::open(path)?;
        Self::lock(&file, true, true)?;
        Ok(LockFile { file })
    }

    pub fn try_acquire_exclusive(path: &Path) -> io::Result<LockFile> {
        let file = Self::open(path)?;
        Self::lock(&file, true, false)?;
        Ok(LockFile { file })
    }

    pub fn acquire_shared(path: &Path) -> io::Result<LockFile> {
        let file = Self::open(path)?;
        Self::lock(&file, false, true)?;
        Ok(LockFile { file })
    }

    pub fn acquire_shared_existing(path: &Path) -> io::Result<LockFile> {
        let file = Self::open_existing(path)?;
        Self::lock(&file, false, true)?;
        Ok(LockFile { file })
    }

    pub fn try_acquire_shared(path: &Path) -> io::Result<LockFile> {
        let file = Self::open(path)?;
        Self::lock(&file, false, false)?;
        Ok(LockFile { file })
    }
}

impl Drop for LockFile {
    fn drop(&mut self) {
        Self::unlock(&self.file);
    }
}

/// 进程存活判定（陈旧锁，红线22）：kill(pid,0)；ESRCH → 死，EPERM → 活。
pub fn process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if rc == 0 {
        return true;
    }
    match io::Error::last_os_error().raw_os_error() {
        Some(libc::ESRCH) => false,
        Some(libc::EPERM) => true,
        _ => false,
    }
}

/// temp + 原子改名落盘（rename(2) 原子替换）。
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
    if std::fs::rename(&tmp, target).is_err() {
        let err = io::Error::last_os_error();
        let _ = std::fs::remove_file(&tmp);
        return Err(err);
    }
    Ok(())
}

/// 原子独占创建：已存在 → Err。
pub fn create_new(target: &Path, bytes: &[u8]) -> io::Result<()> {
    // r4 B5：崩溃原子 + 唯一性——temp 写完 sync 后用 link(2) 原子发布（目标已存在
    // → EEXIST 且目标不动），再 unlink temp。macOS rename(2) 是覆盖语义，不可用。
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
    let c_tmp = std::ffi::CString::new(tmp.as_os_str().as_encoded_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "路径含 NUL"))?;
    let c_target = std::ffi::CString::new(target.as_os_str().as_encoded_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "路径含 NUL"))?;
    let rc = unsafe { libc::link(c_tmp.as_ptr(), c_target.as_ptr()) };
    let _ = std::fs::remove_file(&tmp); // link 成败都清理 temp（成功后为硬链接解引用）
    if rc != 0 {
        return Err(io::Error::last_os_error()); // EEXIST → 输者协议
    }
    Ok(())
}

/// 卷文件系统名（FSEvents 能力探测；v1 保守返回 None → 永久直读）。
pub fn fs_name_for(_path: &Path) -> Option<String> {
    // FSEvents 在 HFS+/APFS 均可用；此处保守关闭（正确性不依赖订阅，D5/红线21）。
    // TODO(§8-6)：接入 FSEvents 能力探测与订阅（开放问题39 能力矩阵）。
    None
}
