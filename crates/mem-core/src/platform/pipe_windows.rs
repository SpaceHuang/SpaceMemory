//! Windows named pipe 单步服务原语（sidecar 单实例握手，D2）。
//!
//! `PipeServer::create` 持有 FILE_FLAG_FIRST_PIPE_INSTANCE 语义的服务端实例；
//! `try_serve_one` 非阻塞尝试：无客户端连接 → false；有 → 读请求、写应答、断开、
//! 重建监听实例。客户端见 `pipe_client_exchange`。


use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_BROKEN_PIPE, ERROR_PIPE_CONNECTED, HANDLE, WAIT_OBJECT_0,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED,
    FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
};
use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE};
use windows_sys::Win32::System::IO::{GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Pipes::{ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe};
use windows_sys::Win32::System::Threading::{
    CreateEventW, ResetEvent, WaitForSingleObject,
};

const PIPE_TYPE_BYTE: u32 = 0x0000_0000;
const PIPE_WAIT: u32 = 0x0000_0000;
const PIPE_UNLIMITED_INSTANCES: u32 = 255;
const ERROR_IO_PENDING: u32 = 997;
const ERROR_PIPE_LISTENING: u32 = 536;
const CONNECT_POLL_MS: u32 = 100;
/// 握手读/写的重叠等待上限（客户端异常时防服务卡死；超时即断开重建）。
const IO_TIMEOUT_MS: u32 = 2000;

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
            // 立即发起一次异步 connect，后续轮询其完成状态。
            let mut ov: windows_sys::Win32::System::IO::OVERLAPPED = std::mem::zeroed();
            ov.hEvent = event;
            let rc = ConnectNamedPipe(handle, &mut ov);
            let pending = rc == 0 && (last_err() == ERROR_IO_PENDING || last_err() == ERROR_PIPE_LISTENING);
            if rc == 0 && !pending && last_err() != ERROR_PIPE_CONNECTED {
                CloseHandle(handle);
                CloseHandle(event);
                return Err(std::io::Error::last_os_error());
            }
            let connected = rc == 0 && last_err() == ERROR_PIPE_CONNECTED;
            Ok(PipeServer { handle, event, name: name.to_string(), pending_connect: pending || connected })
        }
    }

    /// 非阻塞尝试服务一次连接：无连接 → false；有 → 读请求、写应答、断开并重建。
    ///
    /// 重建顺序（评审发现的失聪根因）：必须**先关闭旧实例再创建新实例**——
    /// `FILE_FLAG_FIRST_PIPE_INSTANCE` 要求同名管道无其他存活实例；旧句柄若在
    /// CreateNamedPipeW 之后才 Drop，重建必败 → 服务端永久失聪。
    /// 读/写全部走重叠 I/O（句柄带 FILE_FLAG_OVERLAPPED，空 OVERLAPPED 未定义）。
    pub fn try_serve_one(&mut self, reply: &[u8]) -> bool {
        unsafe {
            if self.pending_connect {
                // 等待连接完成（短轮询）。
                let w = WaitForSingleObject(self.event, CONNECT_POLL_MS);
                if w != WAIT_OBJECT_0 {
                    return false;
                }
                self.pending_connect = false;
            }
            // 读请求（小缓冲足够握手协议；重叠 I/O）。
            let mut buf = [0u8; 256];
            let mut ov: OVERLAPPED = std::mem::zeroed();
            ov.hEvent = self.event;
            ResetEvent(self.event);
            let ok = ReadFile(self.handle, buf.as_mut_ptr() as _, buf.len() as u32, std::ptr::null_mut(), &mut ov);
            if ok == 0 && last_err() != ERROR_IO_PENDING && last_err() != ERROR_BROKEN_PIPE {
                return self.recreate();
            }
            if WaitForSingleObject(self.event, IO_TIMEOUT_MS) != WAIT_OBJECT_0 {
                return self.recreate();
            }
            let mut got = 0u32;
            if GetOverlappedResult(self.handle, &ov, &mut got, 0) == 0 && last_err() != ERROR_BROKEN_PIPE {
                return self.recreate();
            }
            // 写应答（重叠 I/O）。
            ResetEvent(self.event);
            let mut written = 0u32;
            let okw = WriteFile(self.handle, reply.as_ptr() as _, reply.len() as u32, &mut written, &mut ov);
            if okw == 0 && last_err() == ERROR_IO_PENDING {
                if WaitForSingleObject(self.event, IO_TIMEOUT_MS) != WAIT_OBJECT_0 {
                    return self.recreate();
                }
            }
            DisconnectNamedPipe(self.handle);
        }
        self.recreate()
    }

    /// 关闭当前实例并重建监听（旧实例先行关闭——见函数级注释）。
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
                // fresh 的句柄已移交；阻止其 Drop 重复关闭。
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
        let okw = WriteFile(h, request.as_ptr() as _, request.len() as u32, &mut written, std::ptr::null_mut());
        if okw == 0 {
            CloseHandle(h);
            return None;
        }
        let mut buf = [0u8; 1024];
        let mut got = 0u32;
        let okr = ReadFile(h, buf.as_mut_ptr() as _, buf.len() as u32, &mut got, std::ptr::null_mut());
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
