//! 锁协议：门闩 → 记录锁 → 序列锁，锁序全库统一（D3 / D17 / 红线20）。
//!
//! - **门闩（barrier）**：barrier.lock 读写门闩——普通写共享锁贯穿全程，维护独占锁；
//!   单次探测不构成屏障（红线20）；读路径不受门闩影响；
//! - **记录锁**：`<record>.lock`，**锁文件独立于记录文件存在**——记录不存在（待创建/
//!   已删除/待恢复/absent）时同样可锁；create/restore/fix 与缺失记录回填共用同一
//!   规范路径锁（D12 / 红线24）；陈旧锁判定：PID 死亡即抢、存活永不抢、超时只许
//!   等待方放弃（红线22）；
//! - **序列锁（events.lock）**：全库事件序号的唯一分配点，全库串行、叶子锁（红线24）；
//!   每记录锁不得分配序号。
//!
//! 双层互斥：跨进程 = 平台字节范围锁（Windows LockFileEx 按句柄 / macOS fcntl 按
//! 进程）；进程内 = 全局注册表（Windows 句柄语义本身互斥，但 macOS fcntl 按进程
//! 不互斥，注册表补足；也避免无谓的系统调用）。字段锁序：进程内先于平台锁。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Condvar, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, Instant};

use crate::error::{MemError, MemResult};
use crate::platform;
use crate::store::Store;

fn hostname() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "unknown".into())
}

// ---------- 进程内注册表（每店一份，进程生命周期内常驻） ----------

type Map<K, V> = HashMap<K, V>;

static BARRIERS: std::sync::OnceLock<Mutex<Map<PathBuf, &'static RwLock<()>>>> =
    std::sync::OnceLock::new();
static RECORD_STATES: std::sync::OnceLock<Mutex<Map<PathBuf, &'static (Mutex<Holder>, Condvar)>>> =
    std::sync::OnceLock::new();
static SEQUENCES: std::sync::OnceLock<Mutex<Map<PathBuf, &'static Mutex<()>>>> =
    std::sync::OnceLock::new();

#[derive(Debug, Default)]
struct Holder {
    held: bool,
}

fn barrier_rw(root: &std::path::Path) -> &'static RwLock<()> {
    let m = BARRIERS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = m.lock().unwrap();
    map.entry(root.to_path_buf()).or_insert_with(|| Box::leak(Box::new(RwLock::new(()))))
}

fn record_state(key: &std::path::Path) -> &'static (Mutex<Holder>, Condvar) {
    let m = RECORD_STATES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = m.lock().unwrap();
    map.entry(key.to_path_buf()).or_insert_with(|| {
        Box::leak(Box::new((Mutex::new(Holder { held: false }), Condvar::new())))
    })
}

fn seq_mutex(root: &std::path::Path) -> &'static Mutex<()> {
    let m = SEQUENCES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = m.lock().unwrap();
    map.entry(root.to_path_buf()).or_insert_with(|| Box::leak(Box::new(Mutex::new(()))))
}

// ---------- 门闩 ----------

/// 门闩 guard：`file` 先于 `inproc` 声明 → Drop 先释平台锁再放进程内槽位。
pub struct BarrierGuard {
    file: Option<platform::LockFile>,
    /// 进程内槽位（Drop 顺序：先平台锁后进程内槽位——字段序即释放序）。
    #[allow(dead_code)] // 仅为持有 RAII guard；读取无意义
    inproc: BarrierSlot,
}

#[allow(dead_code)] // 同上：两变体仅承载 guard 所有权
enum BarrierSlot {
    Read(RwLockReadGuard<'static, ()>),
    Write(RwLockWriteGuard<'static, ()>),
}

impl std::fmt::Debug for BarrierGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BarrierGuard").finish_non_exhaustive()
    }
}

impl Drop for BarrierGuard {
    fn drop(&mut self) {
        self.file = None; // 先释放平台锁（字节范围）
        // inproc 随字段顺序释放。
    }
}

fn barrier_path(store: &Store) -> PathBuf {
    store.abs("barrier.lock")
}

/// 普通写路径：共享门闩，贯穿写路径全程（红线20）。
pub fn barrier_shared(store: &Store) -> MemResult<BarrierGuard> {
    crate::paths::validate_store_root_lock(store, "barrier.lock")?;
    let inproc = BarrierSlot::Read(barrier_rw(store.root()).read().unwrap());
    let file = platform::LockFile::acquire_shared(&barrier_path(store))
        .map_err(|e| MemError::io(barrier_path(store).display().to_string(), e))?;
    Ok(BarrierGuard { file: Some(file), inproc })
}

/// 维护/merge/apply：独占门闩（阻塞直到所有共享持有者退出）。
pub fn barrier_exclusive(store: &Store) -> MemResult<BarrierGuard> {
    crate::paths::validate_store_root_lock(store, "barrier.lock")?;
    let inproc = BarrierSlot::Write(barrier_rw(store.root()).write().unwrap());
    let file = platform::LockFile::acquire_exclusive(&barrier_path(store))
        .map_err(|e| MemError::io(barrier_path(store).display().to_string(), e))?;
    Ok(BarrierGuard { file: Some(file), inproc })
}

/// 非阻塞尝试（测试与快失败场景）。
pub fn try_barrier_exclusive(store: &Store) -> MemResult<BarrierGuard> {
    crate::paths::validate_store_root_lock(store, "barrier.lock")?;
    let inproc = match barrier_rw(store.root()).try_write() {
        Ok(g) => BarrierSlot::Write(g),
        Err(_) => {
            return Err(MemError::LockTimeout { lock_path: "barrier.lock".into(), holder: None })
        }
    };
    let file = platform::LockFile::try_acquire_exclusive(&barrier_path(store)).map_err(|_| {
        MemError::LockTimeout { lock_path: "barrier.lock".into(), holder: None }
    })?;
    Ok(BarrierGuard { file: Some(file), inproc })
}

/// 非阻塞尝试共享。
pub fn try_barrier_shared(store: &Store) -> MemResult<BarrierGuard> {
    crate::paths::validate_store_root_lock(store, "barrier.lock")?;
    let inproc = match barrier_rw(store.root()).try_read() {
        Ok(g) => BarrierSlot::Read(g),
        Err(_) => {
            return Err(MemError::LockTimeout { lock_path: "barrier.lock".into(), holder: None })
        }
    };
    let file = platform::LockFile::try_acquire_shared(&barrier_path(store)).map_err(|_| {
        MemError::LockTimeout { lock_path: "barrier.lock".into(), holder: None }
    })?;
    Ok(BarrierGuard { file: Some(file), inproc })
}

// ---------- 记录锁 ----------

#[derive(Debug)]
pub struct RecordLockGuard {
    file: Option<platform::LockFile>,
    state: &'static (Mutex<Holder>, Condvar),
}

impl RecordLockGuard {
    pub fn rel_path(&self) -> &str {
        // 由获取处记录；此处不需要——保留接口给上层诊断。
        ""
    }
}

impl Drop for RecordLockGuard {
    fn drop(&mut self) {
        self.file = None; // 平台锁释放
        let (m, cv) = self.state;
        let mut h = m.lock().unwrap();
        h.held = false;
        cv.notify_all();
    }
}

fn owner_json() -> String {
    format!(
        "{{\"pid\":{},\"host\":\"{}\",\"ts\":\"{}\"}}",
        std::process::id(),
        hostname(),
        crate::clock::now_rfc3339()
    )
}

#[derive(Debug, serde::Deserialize)]
struct OwnerFile {
    pid: u32,
    #[serde(default)]
    host: String,
}

fn read_owner(path: &std::path::Path) -> Option<OwnerFile> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn write_owner(path: &std::path::Path, via: &platform::LockFile) -> MemResult<()> {
    via.overwrite(owner_json().as_bytes())
        .map_err(|e| MemError::io(path.display().to_string(), e))
}

fn ensure_parent(path: &std::path::Path) -> MemResult<()> {
    let Some(dir) = path.parent() else { return Ok(()) };
    if dir.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(dir).map_err(|e| MemError::io(dir.display().to_string(), e))
}

fn lock_file_path(store: &Store, record_rel: &str) -> PathBuf {
    store.abs(crate::paths::record_lock_rel(record_rel))
}

/// 获取记录锁（阻塞 + 超时；缺失记录同规范路径可锁，D12/红线24）。
///
/// 陈旧锁判定（红线22）：PID 死亡即抢（OS 锁随进程死亡自动释放，残留锁文件被
/// 覆盖）；存活永不抢；超时只许等待方放弃（`LockTimeout` 携带持有者信息）。
pub fn record_lock(
    store: &Store,
    record_rel: &str,
    timeout: Duration,
) -> MemResult<RecordLockGuard> {
    let deadline = Instant::now() + timeout;
    crate::paths::validate_store_relative_path(store, record_rel)?;
    let path = lock_file_path(store, record_rel);
    crate::paths::validate_store_relative_path(
        store,
        &crate::paths::record_lock_rel(record_rel),
    )?;
    ensure_parent(&path)?;
    let state = record_state(&path);
    loop {
        // 1) 进程内槽位。
        {
            let (m, cv) = state;
            let mut h = m.lock().unwrap();
            while h.held {
                let now = Instant::now();
                if now >= deadline {
                    return Err(timeout_err(&path));
                }
                let (g, _t) = cv.wait_timeout(h, deadline.saturating_duration_since(now)).unwrap();
                h = g;
            }
            h.held = true;
        }
        // 2) 跨进程平台锁。
        match platform::LockFile::try_acquire_exclusive(&path) {
            Ok(f) => {
                write_owner(&path, &f)?;
                return Ok(RecordLockGuard { file: Some(f), state });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // 3) 别人持有：读 owner 判陈旧。
                {
                    let (m, cv) = state;
                    let mut h = m.lock().unwrap();
                    h.held = false;
                    cv.notify_all();
                }
                let owner = read_owner(&path);
                // H6：跨主机持有者不可用本机 PID 判活——owner.host ≠ 本机 → 视为存活
                //（永不抢，只等待；多机同步是 D4 既定场景，误抢 = 并发写灾难）。
                let foreign = owner
                    .as_ref()
                    .map(|o| !o.host.is_empty() && o.host != hostname())
                    .unwrap_or(false);
                let alive = if foreign {
                    true
                } else {
                    owner
                        .as_ref()
                        .map(|o| platform::process_alive(o.pid))
                        .unwrap_or(false)
                };
                if !alive {
                    // PID 死亡即抢：OS 锁理应已释放；短暂退避避免紧密自旋
                    //（他者抢在中间的真实竞争窗口）。
                    std::thread::sleep(Duration::from_millis(25));
                    continue;
                }
                if Instant::now() >= deadline {
                    let holder = owner.map(|o| format!("pid={} host={}", o.pid, o.host));
                    return Err(MemError::LockTimeout {
                        lock_path: path.display().to_string(),
                        holder,
                    });
                }
                std::thread::sleep(Duration::from_millis(15));
            }
            Err(e) => {
                {
                    let (m, cv) = state;
                    let mut h = m.lock().unwrap();
                    h.held = false;
                    cv.notify_all();
                }
                return Err(MemError::io(path.display().to_string(), e));
            }
        }
    }
}

fn timeout_err(path: &std::path::Path) -> MemError {
    let holder = read_owner(path).map(|o| format!("pid={} host={}", o.pid, o.host));
    MemError::LockTimeout { lock_path: path.display().to_string(), holder }
}

/// 非阻塞尝试：`Ok(None)` = 忙。
pub fn try_record_lock(
    store: &Store,
    record_rel: &str,
) -> MemResult<Option<RecordLockGuard>> {
    crate::paths::validate_store_relative_path(store, record_rel)?;
    let path = lock_file_path(store, record_rel);
    crate::paths::validate_store_relative_path(
        store,
        &crate::paths::record_lock_rel(record_rel),
    )?;
    ensure_parent(&path)?;
    let state = record_state(&path);
    {
        let (m, _cv) = state;
        let mut h = m.lock().unwrap();
        if h.held {
            return Ok(None);
        }
        h.held = true;
    }
    match platform::LockFile::try_acquire_exclusive(&path) {
        Ok(f) => {
            write_owner(&path, &f)?;
            Ok(Some(RecordLockGuard { file: Some(f), state }))
        }
        Err(e) => {
            {
                let (m, cv) = state;
                let mut h = m.lock().unwrap();
                h.held = false;
                cv.notify_all();
            }
            if e.kind() == std::io::ErrorKind::WouldBlock {
                Ok(None)
            } else {
                Err(MemError::io(path.display().to_string(), e))
            }
        }
    }
}

// ---------- 序列锁（叶子） ----------

/// 全库事件序号唯一分配点（红线24）：闭包内完成"读最后事件 → 分配序号 → 追加"。
/// 叶子锁：闭包内不得再取门闩/记录锁。
pub fn with_sequence_lock<T>(store: &Store, f: impl FnOnce() -> T) -> MemResult<T> {
    crate::paths::validate_store_root_lock(store, "events.lock")?;
    let inproc = seq_mutex(store.root());
    let _g: MutexGuard<'static, ()> = inproc.lock().unwrap();
    let _file = platform::LockFile::acquire_exclusive(&store.abs("events.lock"))
        .map_err(|e| MemError::io(store.abs("events.lock").display().to_string(), e))?;
    Ok(f())
}
