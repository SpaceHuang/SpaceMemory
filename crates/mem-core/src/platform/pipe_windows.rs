//! Windows named pipe 单步服务原语（sidecar 单实例握手，D2）。
//!
//! `PipeServer::create` 持有 FILE_FLAG_FIRST_PIPE_INSTANCE 语义的服务端实例；
//! `try_serve_one` 非阻塞尝试：无客户端连接 → false；有 → 读请求、写应答、断开、
//! 重建监听实例。客户端见 `pipe_client_exchange`。
//!
//! ## 重叠 I/O 所有权与收割纪律（评审 B1，两轮收敛）
//!
//! 带 `FILE_FLAG_OVERLAPPED` 的操作在完成（含取消完成）之前，内核随时可能写其
//! `OVERLAPPED` 与数据缓冲区。因此：
//!
//! 1. 所有 OVERLAPPED 与读写缓冲由句柄**持有**（Box/字段固定地址），绝不以栈
//!    局部承载挂起操作——栈帧可能先于完成消失（曾致 0xC0000005，CI 实测）；
//! 2. 放弃（超时 / 完成投递异常）一个操作时：对**原句柄**取消 + 有界排干。
//!    确认到达终态（成功/失败/取消）→ 槽位可复用并重建实例；仍
//!    `ERROR_IO_INCOMPLETE` → **暂停服务**（保留原句柄与槽位，下一轮入口再
//!    排干）——绝不复用未确认的槽位发起新 I/O，绝不用新句柄收割旧 I/O，
//!    绝不把非终态错误当作完成证明；
//! 3. 仅 Drop（进程即将释放一切）遇到仍未投递的取消完成时，才把整槽连同
//!    句柄一起泄漏给进程生命周期——迟到完成写入的仍是有效内存。泄漏是唯一
//!    既不用后释放也不无限阻塞的终点。

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_BROKEN_PIPE, ERROR_IO_INCOMPLETE, ERROR_OPERATION_ABORTED,
    ERROR_PIPE_CONNECTED, GetLastError, HANDLE, WAIT_OBJECT_0,
};
use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, FILE_SHARE_READ,
    FILE_SHARE_WRITE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Pipes::{ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe};
use windows_sys::Win32::System::Threading::{CreateEventW, ResetEvent, WaitForSingleObject};

const PIPE_TYPE_BYTE: u32 = 0x0000_0000;
const PIPE_WAIT: u32 = 0x0000_0000;
const PIPE_UNLIMITED_INSTANCES: u32 = 255;
const ERROR_IO_PENDING: u32 = 997;
const ERROR_PIPE_LISTENING: u32 = 536;
const CONNECT_POLL_MS: u32 = 100;
/// 握手读/写的重叠等待上限（客户端异常时防服务卡死；超时即放弃）。
const IO_TIMEOUT_MS: u32 = 2000;
/// 放弃挂起 I/O 后的完成事件等待上限（有界排干；异常等待源不再无限阻塞）。
const IO_DRAIN_TIMEOUT_MS: u32 = 5000;

fn to_wide(s: &str) -> Vec<u16> {
    let mut v: Vec<u16> = s.encode_utf16().collect();
    v.push(0);
    v
}

fn last_err() -> u32 {
    unsafe { GetLastError() }
}

/// Drop 时因取消完成仍未投递而泄漏的槽计数（防御路径，正常恒 0；测试断言用）。
static DROPPED_LEAKED_SLOTS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// 服务端单实例管道。
pub struct PipeServer {
    handle: HANDLE,
    event: HANDLE,
    name: String,
    pending_connect: bool,
    /// 连接 OVERLAPPED：Box 固定地址，活到完成可收割（含放弃后的迟到完成）。
    /// 曾以栈局部承载挂起的异步 ConnectNamedPipe，create 返回即释放栈帧，而内核
    /// 在客户端真正连上时（任意未来时刻）仍写该结构 → 踩已释放栈（0xC0000005，
    /// CI runner 实测崩溃）。
    /// 槽位字段全部 `ManuallyDrop`：正常 Drop 路径显式 `take` 后释放；泄漏
    /// 终点 `take` 后 `forget`——`ptr::read+forget` 复制值的写法不能阻止
    /// `Drop::drop` 返回后对原字段的自动析构（第三轮评审 B1：句柄泄漏、
    /// 内存照放），`ManuallyDrop` 才真正把析构权收进句柄自己的手里。
    connect_ov: std::mem::ManuallyDrop<Box<OVERLAPPED>>,
    /// 读/写共享的 I/O 槽（操作串行）：OVERLAPPED、读缓冲、写应答缓冲全部由
    /// 句柄持有。放弃后未确认完成的操作仍被内核引用——槽位与句柄保持存活，
    /// 服务暂停至下一轮入口排干确认。
    io_ov: std::mem::ManuallyDrop<Box<OVERLAPPED>>,
    io_buf: std::mem::ManuallyDrop<Box<[u8; 256]>>,
    io_reply: std::mem::ManuallyDrop<Vec<u8>>,
    /// 读/写槽存在放弃后未确认完成的 I/O（服务暂停，入口排干后才可复用）。
    io_outstanding: bool,
    /// 仅测试注入（debug 构建）：强制本实例的读收割走 INCOMPLETE 分支，
    /// 验证该分支绝不复用槽位写应答。按实例注入——并行测试互不串扰。
    #[cfg(debug_assertions)]
    force_read_reap_incomplete: bool,
}

/// 打开命名管道服务端实例与其完成事件（不含首次 ConnectNamedPipe）。
fn open_pipe(name: &str) -> std::io::Result<(HANDLE, HANDLE)> {
    let wide = to_wide(name);
    unsafe {
        let event = CreateEventW(std::ptr::null(), 1, 0, std::ptr::null());
        if event.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let handle = CreateNamedPipeW(
            wide.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED,
            PIPE_TYPE_BYTE | PIPE_WAIT,
            PIPE_UNLIMITED_INSTANCES,
            4096,
            4096,
            0,
            std::ptr::null(),
        );
        if handle.is_null() || handle == -1isize as HANDLE {
            CloseHandle(event);
            return Err(std::io::Error::last_os_error());
        }
        Ok((handle, event))
    }
}

fn is_valid_handle(h: HANDLE) -> bool {
    !h.is_null() && h != -1isize as HANDLE
}

impl PipeServer {
    pub fn create(name: &str) -> std::io::Result<PipeServer> {
        let (handle, event) = open_pipe(name)?;
        let mut server = PipeServer {
            handle,
            event,
            name: name.to_string(),
            pending_connect: false,
            // 零初始化的是内层 OVERLAPPED（FFI 合法），不是 Box 指针本身。
            connect_ov: std::mem::ManuallyDrop::new(Box::new(unsafe {
                std::mem::zeroed::<OVERLAPPED>()
            })),
            io_ov: std::mem::ManuallyDrop::new(Box::new(unsafe {
                std::mem::zeroed::<OVERLAPPED>()
            })),
            io_buf: std::mem::ManuallyDrop::new(Box::new([0u8; 256])),
            io_reply: std::mem::ManuallyDrop::new(Vec::new()),
            io_outstanding: false,
            #[cfg(debug_assertions)]
            force_read_reap_incomplete: false,
        };
        if !server.submit_connect() {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(server)
    }

    /// 在连接槽位发起（或重建后重新发起）异步 ConnectNamedPipe。
    /// 前提：连接槽已收割（首次创建或上一连接已完成）。
    fn submit_connect(&mut self) -> bool {
        unsafe {
            **self.connect_ov = std::mem::zeroed::<OVERLAPPED>();
            self.connect_ov.hEvent = self.event;
            let rc = ConnectNamedPipe(self.handle, self.connect_ov.as_mut());
            let pending =
                rc == 0 && (last_err() == ERROR_IO_PENDING || last_err() == ERROR_PIPE_LISTENING);
            if rc == 0 && !pending && last_err() != ERROR_PIPE_CONNECTED {
                return false;
            }
            let connected = rc == 0 && last_err() == ERROR_PIPE_CONNECTED;
            self.pending_connect = pending || connected;
            true
        }
    }

    /// 非阻塞尝试服务一次连接：无连接 → false；有 → 读请求、写应答、断开并重建。
    ///
    /// 重建顺序（评审发现的失聪根因）：必须**先关闭旧实例再创建新实例**——
    /// `FILE_FLAG_FIRST_PIPE_INSTANCE` 要求同名管道无其他存活实例；旧句柄若在
    /// CreateNamedPipeW 之后才 Drop，重建必败 → 服务端永久失聪。
    /// 读/写全部走重叠 I/O（句柄带 FILE_FLAG_OVERLAPPED，空 OVERLAPPED 未定义）。
    pub fn try_serve_one(&mut self, make_reply: impl FnOnce() -> Vec<u8>) -> bool {
        // 入口收割：上一放弃的 I/O 未确认完成时，对**同一仍存活句柄**做一次
        // 有界排干；确认完成 → 重建实例继续服务；仍 INCOMPLETE → 本轮暂停
        //（所有等待有界，调用方按节奏重试；句柄与槽位保持存活——绝不重建、
        // 绝不换槽，杜绝新旧句柄错配与未确认复用）。
        if self.io_outstanding {
            if !Self::drain_slot_bounded(self.handle, self.io_ov.as_mut()) {
                return false;
            }
            self.io_outstanding = false;
            if !self.recreate() {
                return false;
            }
        }
        unsafe {
            if self.pending_connect {
                // 等待连接完成（短轮询）。
                let w = WaitForSingleObject(self.event, CONNECT_POLL_MS);
                if w != WAIT_OBJECT_0 {
                    return false;
                }
                // 收割连接完成：完成已投递（事件已置位），此处读取其状态；
                // INCOMPLETE 是防御分支——保持挂起标记，下次进入重试。
                let mut connected_bytes = 0u32;
                let ok = GetOverlappedResult(
                    self.handle,
                    self.connect_ov.as_mut(),
                    &mut connected_bytes,
                    0,
                );
                if ok == 0 && last_err() == ERROR_IO_INCOMPLETE {
                    return false;
                }
                self.pending_connect = false;
            }
            // 读请求（重叠 I/O，缓冲由句柄持有）。
            ResetEvent(self.event);
            self.io_ov.hEvent = self.event;
            let ok = ReadFile(
                self.handle,
                self.io_buf.as_mut_ptr() as _,
                self.io_buf.len() as u32,
                std::ptr::null_mut(),
                self.io_ov.as_mut(),
            );
            if ok == 0 && last_err() != ERROR_IO_PENDING && last_err() != ERROR_BROKEN_PIPE {
                return self.recreate();
            }
            if WaitForSingleObject(self.event, IO_TIMEOUT_MS) != WAIT_OBJECT_0 {
                if !self.abandon_io() {
                    return false;
                }
                return self.recreate();
            }
            let mut got = 0u32;
            let okr = GetOverlappedResult(self.handle, self.io_ov.as_mut(), &mut got, 0);
            let reap_err = last_err();
            // 测试注入（仅 debug 构建存在该影子绑定）：把收割结果伪装成未完成
            //（评审 B1-1 回归入口）。cfg 影子让 mut 只存在于 debug——release
            // 构建的 -D warnings 不会因无谓 mut 报错。
            #[cfg(debug_assertions)]
            let (okr, reap_err) = if self.force_read_reap_incomplete {
                (0, ERROR_IO_INCOMPLETE)
            } else {
                (okr, reap_err)
            };
            if okr == 0 && reap_err == ERROR_IO_INCOMPLETE {
                // 完成投递异常：请求是否读出不可确认——**绝不复用该槽位写应答**
                //（评审 B1-1）。放弃并按放弃纪律处理；放弃在原句柄上执行。
                if !self.abandon_io() {
                    return false;
                }
                return self.recreate();
            }
            if okr == 0 && reap_err != ERROR_BROKEN_PIPE {
                return self.recreate();
            }
            // 只在请求已读完后构造应答，避免使用接受连接前的旧新鲜度快照。
            // 应答复制进句柄持有的缓冲：写挂起被放弃时内核仍可能读取它。
            *self.io_reply = make_reply();
            // 写应答（重叠 I/O）。
            ResetEvent(self.event);
            let mut written = 0u32;
            let okw = WriteFile(
                self.handle,
                self.io_reply.as_ptr() as _,
                self.io_reply.len() as u32,
                &mut written,
                self.io_ov.as_mut(),
            );
            if okw == 0 && last_err() == ERROR_IO_PENDING {
                if WaitForSingleObject(self.event, IO_TIMEOUT_MS) != WAIT_OBJECT_0 {
                    if !self.abandon_io() {
                        return false;
                    }
                    return self.recreate();
                }
            }
            DisconnectNamedPipe(self.handle);
        }
        self.recreate()
    }

    /// 放弃一次挂起的读/写：在**原句柄**上取消该操作并**有界**等待完成投递。
    /// 返回 true = 已确认到达终态（成功/失败/取消都是终态），槽位可复用、
    /// 句柄可关闭；返回 false = 仍 `ERROR_IO_INCOMPLETE` → 置未确认标记，
    /// 服务暂停（句柄与槽位保持存活，下一轮入口排干）。此时**不得**关闭句柄、
    /// 不得复用槽位——取消完成投递在极端调度下可迟于有界等待（run
    /// 45310735000 的无界排干即因此被弃用），而无限阻塞与「超时即释放」都
    /// 曾被否决：前者挂死（2 小时），后者用后释放（0xC0000005）。
    fn abandon_io(&mut self) -> bool {
        let confirmed = Self::drain_slot_bounded(self.handle, self.io_ov.as_mut());
        if !confirmed {
            self.io_outstanding = true;
        }
        confirmed
    }

    /// 对一个可能挂起的操作执行有界排干：取消 + 等待完成事件（至多
    /// `IO_DRAIN_TIMEOUT_MS`）+ 非阻塞读取结果。返回 true 仅当确认到达终态：
    /// 成功，或取消完成（`ERROR_OPERATION_ABORTED`）；`ERROR_IO_INCOMPLETE`
    /// 与其余错误一律按未完成处理——不把任何非终态错误当作完成证明。
    /// 自由函数：调用点（含 Drop）同时借用句柄字段与槽位，避免 &mut self 冲突。
    fn drain_slot_bounded(handle: HANDLE, ov: &mut OVERLAPPED) -> bool {
        unsafe {
            CancelIoEx(handle, ov);
            WaitForSingleObject(ov.hEvent, IO_DRAIN_TIMEOUT_MS);
            let mut ignored = 0u32;
            let ok = GetOverlappedResult(handle, ov, &mut ignored, 0);
            ok != 0 || last_err() == ERROR_OPERATION_ABORTED
        }
    }

    /// 关闭当前实例并重建监听（旧实例先行关闭——见函数级注释）。
    /// 调用前提：无未确认完成的挂起 I/O（放弃未确认时服务已暂停，入口排干
    /// 确认后才可到达此处）。
    fn recreate(&mut self) -> bool {
        unsafe {
            CloseHandle(self.handle);
            CloseHandle(self.event);
        }
        self.handle = std::ptr::null_mut();
        self.event = std::ptr::null_mut();
        match open_pipe(&self.name) {
            Ok((handle, event)) => {
                self.handle = handle;
                self.event = event;
                if !self.submit_connect() {
                    self.handle = std::ptr::null_mut();
                    self.event = std::ptr::null_mut();
                    return false;
                }
                true
            }
            Err(_) => false,
        }
    }
}

impl Drop for PipeServer {
    fn drop(&mut self) {
        // 先把全部槽位所有权从 self 中取出（ManuallyDrop::take）：此后这些
        // 字段不再参与任何自动析构——「正常释放」与「泄漏」都在这里显式处置，
        // 每份存储恰好处置一次（绝不出现句柄泄漏而内存照放的组合）。
        // take 是 unsafe（它放弃析构保证）——此处恰是放弃保证的使用现场。
        let (mut connect_ov, mut io_ov, io_buf, io_reply) = unsafe {
            (
                std::mem::ManuallyDrop::take(&mut self.connect_ov),
                std::mem::ManuallyDrop::take(&mut self.io_ov),
                std::mem::ManuallyDrop::take(&mut self.io_buf),
                std::mem::ManuallyDrop::take(&mut self.io_reply),
            )
        };
        let mut leaked_slot = false;
        unsafe {
            // ---- 连接槽：恰好处置一次（终态 → 释放；未确认 → 泄漏）----
            let connect_terminal;
            if self.pending_connect && is_valid_handle(self.handle) {
                // 连接仍挂起：取消并**有界**等待完成投递；未到达终态 →
                // 连接槽泄漏给进程生命周期（迟到完成写入的仍是有效内存），
                // 严禁在完成前释放。
                CancelIoEx(self.handle, connect_ov.as_ref());
                WaitForSingleObject(connect_ov.hEvent, IO_DRAIN_TIMEOUT_MS);
                let mut ignored = 0u32;
                let ok = GetOverlappedResult(self.handle, connect_ov.as_mut(), &mut ignored, 0);
                connect_terminal = ok != 0 || last_err() == ERROR_OPERATION_ABORTED;
                if connect_terminal {
                    drop(connect_ov);
                } else {
                    std::mem::forget(connect_ov);
                    leaked_slot = true;
                }
            } else {
                drop(connect_ov);
            }
            // ---- 读/写槽：恰好处置一次 ----
            let io_terminal;
            if self.io_outstanding && is_valid_handle(self.handle) {
                io_terminal = Self::drain_slot_bounded(self.handle, io_ov.as_mut());
                if io_terminal {
                    drop(io_ov);
                    drop(io_buf);
                    drop(io_reply);
                } else {
                    std::mem::forget(io_ov);
                    std::mem::forget(io_buf);
                    std::mem::forget(io_reply);
                    leaked_slot = true;
                }
            } else {
                drop(io_ov);
                drop(io_buf);
                drop(io_reply);
            }
            if leaked_slot {
                // 句柄也随槽泄漏：关闭句柄会在完成投递前撤销内核仍引用的对象。
                DROPPED_LEAKED_SLOTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            } else {
                if is_valid_handle(self.handle) {
                    DisconnectNamedPipe(self.handle);
                    CloseHandle(self.handle);
                }
                if is_valid_handle(self.event) {
                    CloseHandle(self.event);
                }
            }
        }
    }
}

/// 客户端一次交换：连接 → 写请求 → 读应答。
pub fn pipe_client_exchange(name: &str, request: &[u8]) -> Option<Vec<u8>> {
    let wide = to_wide(name);
    unsafe {
        let h = CreateFileW(
            wide.as_ptr(),
            (GENERIC_READ | GENERIC_WRITE) as u32,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        );
        if h.is_null() || h == -1isize as HANDLE {
            return None; // 管道不存在：sidecar 不在
        }
        let mut written = 0u32;
        let okw = WriteFile(
            h,
            request.as_ptr() as _,
            request.len() as u32,
            &mut written,
            std::ptr::null_mut(),
        );
        if okw == 0 {
            CloseHandle(h);
            return None;
        }
        let mut buf = [0u8; 1024];
        let mut got = 0u32;
        let okr = ReadFile(
            h,
            buf.as_mut_ptr() as _,
            buf.len() as u32,
            &mut got,
            std::ptr::null_mut(),
        );
        CloseHandle(h);
        if okr == 0 || got == 0 {
            return None;
        }
        Some(buf[..got as usize].to_vec())
    }
}

#[cfg(test)]
mod pipe_tests {
    use super::*;
    use std::sync::atomic::Ordering;

    fn unique_pipe_name() -> String {
        format!(
            r"\\.\pipe\mem-pipe-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        )
    }

    /// 边驱动服务端边等客户端完成交换。try_serve_one 的 true 也可能是
    /// 「放弃后重建」而非已服务——必须以客户端实际拿到应答为准。客户端经
    /// channel 回传：即使其内部阻塞，本函数也在时限内返回可见结果，测试
    /// 不被无超时的客户端读挂死。返回客户端收到的应答（None = 未得到）。
    fn drive_exchange(
        server: &mut PipeServer,
        name: &str,
        req: &[u8],
        reply: &[u8],
        secs: u64,
    ) -> Option<Vec<u8>> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn({
            let name = name.to_string();
            let req = req.to_vec();
            move || {
                let got = exchange_with_retry(&name, &req);
                let _ = tx.send(got);
            }
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        let mut got = None;
        while std::time::Instant::now() < deadline {
            server.try_serve_one(|| reply.to_vec());
            if let Ok(v) = rx.try_recv() {
                got = v;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        got
    }

    /// 客户端带重试：recreate 窗口（旧实例已断开、新实例未建）内连接会失败，
    /// 真实调用方（probe/新鲜度询问）本就按重试语义使用，测试客户端一致。
    fn exchange_with_retry(name: &str, req: &[u8]) -> Option<Vec<u8>> {
        for _ in 0..50 {
            if let Some(reply) = pipe_client_exchange(name, req) {
                return Some(reply);
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        None
    }

    #[test]
    fn pipe_server_serves_repeated_clients_across_instance_recreates() {
        // 每次服务后实例重建：I/O 槽位的所有权跨实例持续，多次重建中持续有效。
        let name = unique_pipe_name();
        let mut server = PipeServer::create(&name).unwrap();
        for _ in 0..5 {
            let got = drive_exchange(&mut server, &name, b"ping", b"pong", 5);
            assert_eq!(got.as_deref(), Some(&b"pong"[..]), "服务端应答超时");
        }
        assert!(!server.io_outstanding, "正常路径不得留下未确认槽位");
    }

    #[test]
    fn pipe_server_recovers_when_client_connects_but_never_sends_request() {
        // 客户端连接后保持沉默：读等待超时 → 放弃该次 I/O（取消 + 有界排干，
        // 取消完成按时投递 → 确认终态）→ 重建实例；重建后的新客户端必须照常服务。
        let name = unique_pipe_name();
        let mut server = PipeServer::create(&name).unwrap();
        let silent = std::thread::spawn({
            let name = name.clone();
            move || {
                let _c = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&name)
                    .expect("沉默客户端连接");
                std::thread::sleep(std::time::Duration::from_millis(2500));
            }
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        while std::time::Instant::now() < deadline {
            server.try_serve_one(|| b"pong".to_vec());
            if silent.is_finished() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        silent.join().unwrap();
        let got = drive_exchange(&mut server, &name, b"ping", b"pong", 5);
        assert_eq!(got.as_deref(), Some(&b"pong"[..]), "重建后新客户端应被服务");
        assert!(!server.io_outstanding, "取消完成按时投递时不进入暂停态");
    }

    #[test]
    fn pipe_server_abandoned_write_keeps_reply_alive_and_recovers() {
        // 写侧放弃窗口：客户端发请求后长时间不读，应答超过管道缓冲（4096）
        // 使 WriteFile 挂起至超时被放弃。应答缓冲由句柄持有（内核仍可能读取
        // 它）；放弃在原句柄上确认终态后重建，新客户端照常服务。
        let name = unique_pipe_name();
        let mut server = PipeServer::create(&name).unwrap();
        let quiet_reader = std::thread::spawn({
            let name = name.clone();
            move || {
                let mut c = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&name)
                    .expect("不读应答的客户端连接");
                use std::io::Write;
                c.write_all(b"ping").unwrap();
                std::thread::sleep(std::time::Duration::from_millis(4500));
            }
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(12);
        while std::time::Instant::now() < deadline {
            server.try_serve_one(|| vec![b'x'; 8192]);
            if quiet_reader.is_finished() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        quiet_reader.join().unwrap();
        let got = drive_exchange(&mut server, &name, b"ping", b"pong", 5);
        assert_eq!(
            got.as_deref(),
            Some(&b"pong"[..]),
            "写放弃重建后新客户端应被服务"
        );
    }

    #[test]
    fn unconfirmed_io_pauses_service_on_same_handle_until_confirmed() {
        // 故障注入（B1-2）：排干后仍 ERROR_IO_INCOMPLETE——服务在**同一句柄**上
        // 暂停（句柄与槽位保持存活），绝不重建换句柄、绝不复用槽位；人工确认
        // 完成后下一轮入口排干通过，服务恢复且全程零泄漏。
        const STATUS_PENDING: usize = 0x103;
        let name = unique_pipe_name();
        let mut server = PipeServer::create(&name).unwrap();
        server.io_outstanding = true;
        unsafe {
            **server.io_ov = std::mem::zeroed::<OVERLAPPED>();
            // hEvent 置 NULL：等待立即以 WAIT_FAILED 返回（不真实的完成事件
            // 源正是该防御分支针对的异常场景），排干走非阻塞读取。
            server.io_ov.Internal = STATUS_PENDING;
        }
        let handle_before = server.handle;
        assert!(
            !server.try_serve_one(|| b"pong".to_vec()),
            "未确认完成时本轮必须暂停服务"
        );
        assert_eq!(server.handle, handle_before, "暂停期间句柄不得更换");
        assert!(server.io_outstanding, "暂停态保持未确认标记");
        // 人工确认完成（终态）：下一轮入口排干通过 → 重建 → 服务恢复。
        unsafe {
            **server.io_ov = std::mem::zeroed::<OVERLAPPED>();
        }
        let got = drive_exchange(&mut server, &name, b"ping", b"pong", 5);
        assert_eq!(got.as_deref(), Some(&b"pong"[..]), "确认后服务必须恢复");
        assert!(!server.io_outstanding);
    }

    #[test]
    fn read_reap_incomplete_never_reuses_slot_for_reply() {
        // 故障注入（B1-1）：读收割返回 ERROR_IO_INCOMPLETE——请求是否读出不可
        // 确认，服务必须放弃本轮（不写应答、不复用槽位），按放弃纪律处理后
        // 重建；新客户端在新实例上照常得到应答。
        let name = unique_pipe_name();
        let mut server = PipeServer::create(&name).unwrap();
        #[cfg(debug_assertions)]
        {
            server.force_read_reap_incomplete = true;
        }
        let got = drive_exchange(&mut server, &name, b"ping", b"pong", 5);
        // 本轮实例被放弃：客户端在该实例上收不到应答（None）。
        assert_eq!(got, None, "INCOMPLETE 轮不得有应答写出");
        #[cfg(debug_assertions)]
        {
            server.force_read_reap_incomplete = false;
        }
        // 新实例：完整应答恢复。
        let got = drive_exchange(&mut server, &name, b"ping", b"pong", 5);
        assert_eq!(got.as_deref(), Some(&b"pong"[..]), "重建后新客户端应被服务");
        assert!(!server.io_outstanding, "真实已完成的读经排干应确认为终态");
    }

    #[test]
    fn dropping_server_with_pending_connect_reaps_before_close() {
        // 连接仍挂起时 Drop：取消并收割完成后才关句柄（OVERLAPPED 在 Box 上，
        // 地址稳定）；仍 INCOMPLETE 时整槽连同句柄泄漏。不挂不崩即通过。
        let name = unique_pipe_name();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let server = PipeServer::create(&name).unwrap();
            drop(server);
            let _ = tx.send(());
        });
        rx.recv_timeout(std::time::Duration::from_secs(2))
            .expect("挂起连接下的 Drop 应取消收割后及时返回");
    }

    #[test]
    fn drop_leaks_slot_when_completion_still_pending() {
        // 故障注入（B1 终点）：Drop 时取消完成仍未投递——整槽连同句柄泄漏给
        // 进程生命周期（迟到完成写入的仍是有效内存），并以进程级计数登记。
        // 析构验证（第三轮评审 B1）：drop 后经事前保存的裸指针读取 OVERLAPPED
        // ——字段必须仍存活且值稳定（释放后的 debug 堆会毒化填充，值改变或
        // 崩溃即暴露「句柄泄漏而内存照放」）。
        const STATUS_PENDING: usize = 0x103;
        let before = DROPPED_LEAKED_SLOTS.load(Ordering::Relaxed);
        let name = unique_pipe_name();
        let mut server = PipeServer::create(&name).unwrap();
        server.io_outstanding = true;
        unsafe {
            **server.io_ov = std::mem::zeroed::<OVERLAPPED>();
            server.io_ov.Internal = STATUS_PENDING;
        }
        let ov_ptr: *const OVERLAPPED = &**server.io_ov;
        drop(server);
        assert_eq!(
            DROPPED_LEAKED_SLOTS.load(Ordering::Relaxed),
            before + 1,
            "未确认完成的槽在 Drop 必须泄漏而非释放"
        );
        let internal = unsafe { (*ov_ptr).Internal };
        assert_eq!(
            internal as usize, STATUS_PENDING,
            "泄漏槽位的 OVERLAPPED 必须仍然有效且未被析构"
        );
    }
}
