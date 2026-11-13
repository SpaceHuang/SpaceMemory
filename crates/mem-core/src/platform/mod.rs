//! 平台抽象层：字节范围锁、原子改名、原子独占创建、进程存活判定、卷能力探测。
//!
//! 目标平台：Windows 与 macOS（设计文档 §6 依赖选型）。
//!
//! | 能力 | Windows | macOS |
//! |---|---|---|
//! | 字节范围读写锁（门闩/序列锁/记录锁） | LockFileEx（按句柄） | fcntl F_SETLK（按进程） |
//! | 原子改名（崩溃原子落盘，R3.1/红线6） | MoveFileEx(REPLACE_EXISTING) | rename(2) |
//! | 原子独占创建（R3.3） | CreateFileW(CREATE_NEW) | create_new(2) |
//! | 变更日志订阅（新鲜性证明覆盖来源，D5） | NTFS USN journal | FSEvents |
//!
//! 注意：Windows 锁按句柄（同进程双句柄互斥）；macOS fcntl 锁按进程（同进程
//! 不互斥）——同进程内的并发互斥由 lock.rs 的进程内注册表补足（§8-4b）。
//! 文件 watcher（notify）只是订阅之上的便利层，非新鲜度依据（红线21）。

pub mod process;

#[cfg(windows)]
pub mod windows;

#[cfg(windows)]
pub mod pipe_windows;

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(windows)]
pub use windows::*;
#[cfg(windows)]
pub use pipe_windows::*;

#[cfg(target_os = "macos")]
pub use macos::*;
