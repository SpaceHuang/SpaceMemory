//! Windows named pipe 单步服务原语（sidecar 单实例握手，D2）。
//!
//! `PipeServer::create` 持有 FILE_FLAG_FIRST_PIPE_INSTANCE 语义的服务端实例；
//! `try_serve_one` 非阻塞尝试：无客户端连接 → false；有 → 读请求、写应答、断开、
//! 重建监听实例。客户端见 `pipe_client_exchange`。
//!
//! ## 重叠 I/O 所有权纪律（评审 B1）
//!
//! 带 `FILE_FLAG_OVERLAPPED` 的操作在完成（含取消完成）之前，内核随时可能写其
//! `OVERLAPPED` 与数据缓冲区。因此：
//!
//! 1. 所有 OVERLAPPED 与读写缓冲都由句柄**持有**（Box 固定地址），绝不以栈局部
//!    承载挂起操作——栈帧可能先于完成消失（曾致 0xC0000005，CI runner 实测）；
//! 2. 放弃（超时）一个挂起操作时：取消 + 有界排干；若确认完成 → 槽位可复用；
//!    若仍 `ERROR_IO_INCOMPLETE` → **整个槽位泄漏给进程生命周期**（迟到完成写入
//!    的仍是有效内存），换新槽继续——泄漏是唯一既不用后释放也不无限阻塞的终点；
//! 3. 复用槽位前必须先收割上一次放弃的 I/O（正常路径是无等待的 no-op）。

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_BROKEN_PIPE, ERROR_IO_INCOMPLETE, ERROR_PIPE_CONNECTED, GetLastError,
    HANDLE, WAIT_OBJECT_0,
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
/// 握手读/写的重叠等待上限（客户端异常时防服务卡死；超时即放弃并重建）。
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
    connect_ov: Box<OVERLAPPED>,
    /// 读/写共享的 I/O 槽（操作串行）：OVERLAPPED、读缓冲、写应答缓冲全部由
    /// 句柄持有。放弃后未确认完成的操作仍被内核引用——槽位所有权持续有效，
    /// 复用前经 `quiesce_io_slot` 收割，仍 INCOMPLETE 则整槽泄漏换新。
    io_ov: Box<OVERLAPPED>,
    io_buf: Box<[u8; 256]>,
    io_reply: Vec<u8>,
    /// 读/写槽存在放弃后未确认完成的 I/O（下一次进入前必须收割）。
    io_outstanding: bool,
    /// 因放弃未完成而整槽泄漏的次数（防御路径，正常恒 0；测试断言用）。
    abandoned_leaked: usize,
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
            connect_ov: Box::new(unsafe { std::mem::zeroed() }),
            io_ov: Box::new(unsafe { std::mem::zeroed() }),
            io_buf: Box::new([0u8; 256]),
            io_reply: Vec::new(),
            io_outstanding: false,
            abandoned_leaked: 0,
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
            *self.connect_ov = std::mem::zeroed();
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
            // 复用读/写槽前收割上次放弃的 I/O（正常路径为 no-op）。
            self.quiesce_io_slot();
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
                self.abandon_io();
                return self.recreate();
            }
            let mut got = 0u32;
            let okr = GetOverlappedResult(self.handle, self.io_ov.as_mut(), &mut got, 0);
            if okr == 0 && last_err() == ERROR_IO_INCOMPLETE {
                // 事件已置位但结果读取仍报未完成（防御分支）：标记槽位未确认，
                // 交由 recreate 后的下一次进入收割。
                self.io_outstanding = true;
            }
            if okr == 0 && last_err() != ERROR_BROKEN_PIPE && last_err() != ERROR_IO_INCOMPLETE {
                return self.recreate();
            }
            // 只在请求已读完后构造应答，避免使用接受连接前的旧新鲜度快照。
            // 应答复制进句柄持有的缓冲：写挂起被放弃时内核仍可能读取它。
            self.io_reply = make_reply();
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
                    self.abandon_io();
                    return self.recreate();
                }
            }
            DisconnectNamedPipe(self.handle);
        }
        self.recreate()
    }

    /// 放弃一次挂起的读/写：取消该操作并**有界**等待完成投递。
    ///
    /// 完成已确认 → 槽位可直接复用；仍 `ERROR_IO_INCOMPLETE`（取消完成投递在
    /// 极端调度下可迟于有界等待——run 45310735000 的无界排干即因此被弃用）→
    /// 置 `io_outstanding`：槽位所有权由句柄**继续持有**（Box 地址稳定，迟到
    /// 完成写入的仍是有效内存），下一次进入前经 `quiesce_io_slot` 收割或泄漏。
    /// 无限阻塞排干与「超时即释放栈上结构」都曾被否决：前者挂死（2 小时），
    /// 后者用后释放（0xC0000005）。
    fn abandon_io(&mut self) {
        unsafe {
            CancelIoEx(self.handle, self.io_ov.as_ref());
            WaitForSingleObject(self.io_ov.hEvent, IO_DRAIN_TIMEOUT_MS);
            let mut ignored = 0u32;
            let ok = GetOverlappedResult(self.handle, self.io_ov.as_mut(), &mut ignored, 0);
            if ok == 0 && last_err() == ERROR_IO_INCOMPLETE {
                self.io_outstanding = true;
            }
        }
    }

    /// 复用读/写槽前的收割：对放弃后未确认完成的 I/O 再做一次有界排干。
    /// 仍 INCOMPLETE → 整槽泄漏给进程生命周期（迟到完成写入的仍是有效内存），
    /// 换新槽继续——泄漏只发生在取消完成投递异常迟延的防御路径，正常恒 0。
    fn quiesce_io_slot(&mut self) {
        if !self.io_outstanding {
            return;
        }
        if Self::drain_slot_bounded(self.handle, self.io_ov.as_mut()) {
            self.io_outstanding = false;
            return;
        }
        let io_ov = unsafe { std::ptr::read(&self.io_ov) };
        std::mem::forget(io_ov);
        let io_buf = unsafe { std::ptr::read(&self.io_buf) };
        std::mem::forget(io_buf);
        let io_reply = unsafe { std::ptr::read(&self.io_reply) };
        std::mem::forget(io_reply);
        self.io_ov = Box::new(unsafe { std::mem::zeroed() });
        self.io_buf = Box::new([0u8; 256]);
        self.io_reply = Vec::new();
        self.io_outstanding = false;
        self.abandoned_leaked += 1;
    }

    /// 对一个可能挂起的操作执行有界排干：取消 + 等待完成事件（至多
    /// `IO_DRAIN_TIMEOUT_MS`）+ 非阻塞读取结果。返回 true = 已确认完成
    ///（成功/失败/取消都是终态）；false = 仍 `ERROR_IO_INCOMPLETE`。
    /// 自由函数：调用点（含 Drop）同时借用句柄字段与槽位，避免 &mut self 冲突。
    fn drain_slot_bounded(handle: HANDLE, ov: &mut OVERLAPPED) -> bool {
        unsafe {
            CancelIoEx(handle, ov);
            WaitForSingleObject(ov.hEvent, IO_DRAIN_TIMEOUT_MS);
            let mut ignored = 0u32;
            let ok = GetOverlappedResult(handle, ov, &mut ignored, 0);
            !(ok == 0 && last_err() == ERROR_IO_INCOMPLETE)
        }
    }

    /// 关闭当前实例并重建监听（旧实例先行关闭——见函数级注释）。
    /// 读/写槽的所有权跨实例持续（挂起 I/O 的缓冲不因重建而释放）；
    /// 重建失败时句柄置空，Drop 的关闭按有效性跳过。
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
        unsafe {
            if self.pending_connect && is_valid_handle(self.handle) {
                // 连接仍挂起：取消并**有界**等待完成投递；仍 INCOMPLETE →
                // 连接槽泄漏给进程生命周期（迟到完成写入的仍是有效内存），
                // 严禁在完成前释放。
                CancelIoEx(self.handle, self.connect_ov.as_ref());
                WaitForSingleObject(self.connect_ov.hEvent, IO_DRAIN_TIMEOUT_MS);
                let mut ignored = 0u32;
                let ok =
                    GetOverlappedResult(self.handle, self.connect_ov.as_mut(), &mut ignored, 0);
                if ok == 0 && last_err() == ERROR_IO_INCOMPLETE {
                    let connect_ov = std::ptr::read(&self.connect_ov);
                    std::mem::forget(connect_ov);
                }
            }
            if self.io_outstanding && is_valid_handle(self.handle) {
                if !Self::drain_slot_bounded(self.handle, self.io_ov.as_mut()) {
                    let io_ov = std::ptr::read(&self.io_ov);
                    std::mem::forget(io_ov);
                    let io_buf = std::ptr::read(&self.io_buf);
                    std::mem::forget(io_buf);
                    let io_reply = std::ptr::read(&self.io_reply);
                    std::mem::forget(io_reply);
                }
            }
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

    /// 驱动服务端直到服务到一次连接或超时。
    fn serve_until(server: &mut PipeServer, reply: &[u8], secs: u64) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        loop {
            if server.try_serve_one(|| reply.to_vec()) {
                return true;
            }
            if std::time::Instant::now() > deadline {
                return false;
            }
        }
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
            let client = std::thread::spawn({
                let name = name.clone();
                move || exchange_with_retry(&name, b"ping")
            });
            assert!(serve_until(&mut server, b"pong", 5), "服务端应答超时");
            assert_eq!(client.join().unwrap().as_deref(), Some(&b"pong"[..]));
        }
        assert_eq!(server.abandoned_leaked, 0, "正常路径不得触发泄漏");
    }

    #[test]
    fn pipe_server_recovers_when_client_connects_but_never_sends_request() {
        // 客户端连接后保持沉默：读等待超时 → 放弃该次 I/O（取消 + 有界排干）
        // 并重建实例；重建后的新客户端必须照常服务，且取消完成在排干窗口内
        // 到达时不产生泄漏。
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
        let client = std::thread::spawn({
            let name = name.clone();
            move || exchange_with_retry(&name, b"ping")
        });
        assert!(
            serve_until(&mut server, b"pong", 5),
            "重建后新客户端应被服务"
        );
        assert_eq!(client.join().unwrap().as_deref(), Some(&b"pong"[..]));
        assert_eq!(server.abandoned_leaked, 0, "取消完成按时投递时不得泄漏");
    }

    #[test]
    fn pipe_server_abandoned_write_keeps_reply_alive_and_recovers() {
        // 写侧放弃窗口：客户端发请求后长时间不读，应答超过管道缓冲（4096）
        // 使 WriteFile 挂起至超时被放弃。应答缓冲由句柄持有（内核仍可能读取
        // 它），槽位标记未确认完成；后续收割/复用不得崩溃，新客户端照常服务。
        let name = unique_pipe_name();
        let mut server = PipeServer::create(&name).unwrap();
        let quiet_reader = std::thread::spawn({
            let name = name.clone();
            move || {
                let c = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&name)
                    .expect("不读应答的客户端连接");
                use std::io::Write;
                let mut c = c;
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
        let client = std::thread::spawn({
            let name = name.clone();
            move || exchange_with_retry(&name, b"ping")
        });
        assert!(
            serve_until(&mut server, b"pong", 5),
            "写放弃重建后新客户端应被服务"
        );
        assert_eq!(client.join().unwrap().as_deref(), Some(&b"pong"[..]));
        assert_eq!(server.abandoned_leaked, 0, "取消完成按时投递时不得泄漏");
    }

    #[test]
    fn quiesce_leaks_slot_when_abandoned_io_still_incomplete() {
        // 故障注入（B1）：排干后仍 ERROR_IO_INCOMPLETE 的防御分支——整槽泄漏
        // 给进程生命周期并换新槽，服务继续可用；绝不提前释放挂起 I/O 的存储。
        // 取消完成投递不会确定性迟到，故以私有状态构造该分支的入口条件：
        // OVERLAPPED.Internal = STATUS_PENDING 即内核视角「仍未完成」。
        const STATUS_PENDING: usize = 0x103;
        let name = unique_pipe_name();
        let mut server = PipeServer::create(&name).unwrap();
        server.io_outstanding = true;
        unsafe {
            *server.io_ov = std::mem::zeroed();
            // hEvent 置 NULL：等待立即以 WAIT_FAILED 返回（不真实的完成事件
            // 源正是该防御分支针对的异常场景），排干走非阻塞读取。
            server.io_ov.Internal = STATUS_PENDING;
        }
        server.quiesce_io_slot();
        assert_eq!(server.abandoned_leaked, 1, "未确认完成的槽位必须泄漏换新");
        assert!(!server.io_outstanding, "泄漏后槽位标记复位");
        // 换新后的槽位照常服务完整一轮。
        let client = std::thread::spawn({
            let name = name.clone();
            move || exchange_with_retry(&name, b"ping")
        });
        assert!(serve_until(&mut server, b"pong", 5));
        assert_eq!(client.join().unwrap().as_deref(), Some(&b"pong"[..]));
        assert_eq!(server.abandoned_leaked, 1);
    }

    #[test]
    fn dropping_server_with_pending_connect_reaps_before_close() {
        // 连接仍挂起时 Drop：取消并收割完成后才关句柄（OVERLAPPED 在 Box 上，
        // 地址稳定）；仍 INCOMPLETE 时整槽泄漏。不挂不崩即通过。
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
}
