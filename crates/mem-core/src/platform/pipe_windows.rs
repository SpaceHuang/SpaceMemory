//! Windows named pipe 单步服务原语（sidecar 单实例握手，D2）。
//!
//! `PipeServer::create` 持有 FILE_FLAG_FIRST_PIPE_INSTANCE 语义的服务端实例；
//! `try_serve_one` 非阻塞尝试：无客户端连接 → false；有 → 读请求、写应答、断开、
//! 重建监听实例。客户端见 `pipe_client_exchange`。

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_BROKEN_PIPE, ERROR_PIPE_CONNECTED, GetLastError, HANDLE, WAIT_OBJECT_0,
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
/// 握手读/写的重叠等待上限（客户端异常时防服务卡死；超时即断开重建）。
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
    /// 连接 OVERLAPPED 必须活到完成可收割：曾以栈局部承载挂起的异步
    /// ConnectNamedPipe，create 返回即释放栈帧，而内核在客户端真正连上时
    /// （任意未来时刻）仍写该结构的完成状态 → 踩已释放栈（0xC0000005，
    /// CI runner 实测崩溃）。Box 固定地址；完成/放弃/Drop 前一律先收割或取消。
    connect_ov: Box<OVERLAPPED>,
}

impl PipeServer {
    pub fn create(name: &str) -> std::io::Result<PipeServer> {
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
            // 立即发起一次异步 connect，后续轮询其完成状态；OVERLAPPED 上 Box
            //（连接可在任意未来时刻完成，完成时内核仍写该结构）。
            let mut connect_ov: Box<OVERLAPPED> = Box::new(std::mem::zeroed());
            connect_ov.hEvent = event;
            let rc = ConnectNamedPipe(handle, connect_ov.as_mut());
            let pending =
                rc == 0 && (last_err() == ERROR_IO_PENDING || last_err() == ERROR_PIPE_LISTENING);
            if rc == 0 && !pending && last_err() != ERROR_PIPE_CONNECTED {
                CloseHandle(handle);
                CloseHandle(event);
                return Err(std::io::Error::last_os_error());
            }
            let connected = rc == 0 && last_err() == ERROR_PIPE_CONNECTED;
            Ok(PipeServer {
                handle,
                event,
                name: name.to_string(),
                pending_connect: pending || connected,
                connect_ov,
            })
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
                // 收割连接完成：内核写完 OVERLAPPED 才算完成，随后才可重用该结构。
                let mut connected_bytes = 0u32;
                let _ = GetOverlappedResult(
                    self.handle,
                    self.connect_ov.as_mut(),
                    &mut connected_bytes,
                    0,
                );
                self.pending_connect = false;
            }
            // 读请求（小缓冲足够握手协议；重叠 I/O）。
            let mut buf = [0u8; 256];
            let mut ov: OVERLAPPED = std::mem::zeroed();
            ov.hEvent = self.event;
            ResetEvent(self.event);
            let ok = ReadFile(
                self.handle,
                buf.as_mut_ptr() as _,
                buf.len() as u32,
                std::ptr::null_mut(),
                &mut ov,
            );
            if ok == 0 && last_err() != ERROR_IO_PENDING && last_err() != ERROR_BROKEN_PIPE {
                return self.recreate();
            }
            if WaitForSingleObject(self.event, IO_TIMEOUT_MS) != WAIT_OBJECT_0 {
                // 放弃挂起的读：取消并阻塞收割，buf/ov 仍在本调用栈上存活；
                // 此前直接关句柄，完成通知可能写入已释放栈。
                self.abandon_io(&mut ov);
                return self.recreate();
            }
            let mut got = 0u32;
            if GetOverlappedResult(self.handle, &ov, &mut got, 0) == 0
                && last_err() != ERROR_BROKEN_PIPE
            {
                return self.recreate();
            }
            // 只在请求已读完后构造应答，避免使用接受连接前的旧新鲜度快照。
            let reply = make_reply();
            // 写应答（重叠 I/O）。
            ResetEvent(self.event);
            let mut written = 0u32;
            let okw = WriteFile(
                self.handle,
                reply.as_ptr() as _,
                reply.len() as u32,
                &mut written,
                &mut ov,
            );
            if okw == 0 && last_err() == ERROR_IO_PENDING {
                if WaitForSingleObject(self.event, IO_TIMEOUT_MS) != WAIT_OBJECT_0 {
                    self.abandon_io(&mut ov);
                    return self.recreate();
                }
            }
            DisconnectNamedPipe(self.handle);
        }
        self.recreate()
    }

    /// 放弃一次挂起的重叠 I/O：先取消**该次**操作，再**有界**等待其完成事件
    /// 并读取一次结果——此时缓冲区与 OVERLAPPED 仍在本调用栈上存活，之后才
    /// 允许 recreate 关闭句柄。与 RDC 的 `cancel_and_drain_rdc` 同一纪律
    /// （Cancel 只发请求，完成必须收割），但不采用 `GetOverlappedResult`
    /// 的无限阻塞形态：等待源异常时它曾把 CI runner 的测试进程挂死
    /// （run 45310735000 卡 2 小时）；超时后句柄关闭会兜底取消残余 I/O。
    fn abandon_io(&mut self, ov: &mut OVERLAPPED) {
        unsafe {
            CancelIoEx(self.handle, ov);
            WaitForSingleObject(ov.hEvent, IO_DRAIN_TIMEOUT_MS);
            let mut ignored = 0u32;
            let _ = GetOverlappedResult(self.handle, ov, &mut ignored, 0);
        }
    }

    /// 关闭当前实例并重建监听（旧实例先行关闭——见函数级注释）。
    /// 调用前提：无未收割的挂起 I/O（挂起连接已在收割后置位，挂起读/写已
    /// 经 `abandon_io` 放弃）。
    fn recreate(&mut self) -> bool {
        unsafe {
            CloseHandle(self.handle);
            CloseHandle(self.event);
        }
        match Self::create(&self.name) {
            Ok(fresh) => {
                self.handle = fresh.handle;
                self.event = fresh.event;
                self.pending_connect = fresh.pending_connect;
                // PipeServer 实现 Drop，字段只能按位接管（不能 move）：
                // 连接 OVERLAPPED 的 Box 与重复的 name String 读出后，
                // forget 其余（全部 Copy），阻止 fresh 的 Drop 关闭已移交句柄。
                self.connect_ov = unsafe { std::ptr::read(&fresh.connect_ov) };
                let fresh_name: String = unsafe { std::ptr::read(&fresh.name) };
                drop(fresh_name);
                std::mem::forget(fresh);
                true
            }
            Err(_) => false,
        }
    }
}

impl Drop for PipeServer {
    fn drop(&mut self) {
        unsafe {
            if self.pending_connect {
                // 连接仍挂起：取消并**有界**等待完成后才关句柄（OVERLAPPED 在
                // Box 上，地址稳定）；无限阻塞形态曾挂死 runner 测试进程。
                CancelIoEx(self.handle, self.connect_ov.as_ref());
                WaitForSingleObject(self.connect_ov.hEvent, IO_DRAIN_TIMEOUT_MS);
                let mut ignored = 0u32;
                let _ = GetOverlappedResult(self.handle, self.connect_ov.as_mut(), &mut ignored, 0);
            }
            DisconnectNamedPipe(self.handle);
            CloseHandle(self.handle);
            CloseHandle(self.event);
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

/// 抑制未用导入告警（FILE_FLAG_OVERLAPPED 在重叠 connect 中使用）。
#[allow(dead_code)]
const _USED: u32 = FILE_FLAG_OVERLAPPED;

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
        // 每次服务后实例重建：连接 OVERLAPPED 必须在多次重建中持续有效。
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
    }

    #[test]
    fn pipe_server_recovers_when_client_connects_but_never_sends_request() {
        // 客户端连接后保持沉默：读等待超时 → 放弃该次 I/O 并重建实例；
        // 此前路径不收割挂起读就关句柄，完成通知可写已释放栈。
        // 重建后的新客户端必须照常服务。
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
    }

    #[test]
    fn dropping_server_with_pending_connect_reaps_before_close() {
        // 连接仍挂起时 Drop：取消并收割完成后才关句柄（OVERLAPPED 在 Box 上，
        // 地址稳定）；曾直接关句柄，完成通知写入已释放栈。不挂不崩即通过。
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
