//! sidecar 协作协议（D2 / 红线5）。
//!
//! sidecar 是机制核心库的第二前端：单实例握手懒拉起、空闲自退、版本握手自杀更新。
//! 两条不变量：
//! - 一个机制核心两个前端：sidecar 不在走嵌入模式（= 直读模式 + 无 FTS），只是
//!   降速不是故障；
//! - sidecar 对真相无所有权：任意时刻可 kill，冷启动重扫重建。
//!
//! sidecar 的独特价值 = 维持 OS 变更日志订阅（证明式新鲜性的覆盖来源，D5）与索引
//! 热维护。IPC 形态：Windows named pipe / macOS Unix domain socket。

use std::path::Path;
use std::time::{Duration, Instant};

use crate::error::{MemError, MemResult};
use crate::store::Store;

/// sidecar 版本（握手自杀更新用：客户端版本更高 → 服务端自退，D2）。
pub const SIDECAR_VERSION: u32 = 1;

/// 默认空闲自退时长（D2）。
pub const DEFAULT_IDLE: Duration = Duration::from_secs(900);

/// 端点名：单店确定（`\\.\pipe\mem-<hash16>` / `/tmp/mem-<hash>.sock`）。
pub fn endpoint_name(store_root: &Path) -> String {
    let key = crate::token::sha256_hex(store_root.to_string_lossy().as_bytes())[..16].to_string();
    #[cfg(windows)]
    {
        format!(r"\\.\pipe\mem-{key}")
    }
    #[cfg(not(windows))]
    {
        format!("/tmp/mem-{key}.sock")
    }
}

/// 握手应答（D2 单实例握手）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Hello {
    pub version: u32,
    /// 订阅后端（"usn" / "none"——none = 该卷不支持变更日志 → 永久直读形态）。
    pub backend: String,
    /// B3 接线：自检查点以来未消化的本店路径变更数（None = 无订阅/订阅失效
    /// ——不可证明）。读路径据此 + stat 检查点判定快路径（红线21）。
    pub undigested: Option<u64>,
    pub max_seq: u64,
    pub last_rebuild: String,
}

impl Hello {
    pub fn as_bytes(&self) -> Vec<u8> {
        let mut b = serde_json::to_vec(self).unwrap_or_default();
        b.push(b'\n');
        b
    }
}

/// 客户端探测：sidecar 在场则握手成功。
pub fn probe(store: &Store) -> Option<Hello> {
    let name = endpoint_name(store.root());
    #[cfg(windows)]
    {
        let reply = crate::platform::pipe_client_exchange(&name, b"hello\n")?;
        serde_json::from_slice(&reply).ok()
    }
    #[cfg(target_os = "macos")]
    {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixStream;
        let mut s = UnixStream::connect(&name).ok()?;
        s.write_all(b"hello\n").ok()?;
        let mut line = String::new();
        BufReader::new(s).read_line(&mut line).ok()?;
        serde_json::from_str(line.trim()).ok()
    }
}

/// 懒拉起（D2）：探测失败则分离启动自身 `mem serve`，短等后再探测。
///
/// 拉起失败无害（读路径本就直读）——懒拉起不是同步屏障。
pub fn ensure_running(store: &Store) -> MemResult<Option<Hello>> {
    if let Some(h) = probe(store) {
        return Ok(Some(h));
    }
    let exe = std::env::current_exe()
        .map_err(|e| MemError::BadArgument(format!("无法定位自身可执行文件：{e}")))?;
    std::process::Command::new(exe)
        .arg("serve")
        .arg("--store")
        .arg(store.root())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| MemError::BadArgument(format!("sidecar 拉起失败：{e}")))?;
    for _ in 0..20 {
        std::thread::sleep(Duration::from_millis(150));
        if let Some(h) = probe(store) {
            return Ok(Some(h));
        }
    }
    Ok(None)
}

/// sidecar 运行报告。
#[derive(Debug, Default)]
pub struct ServeReport {
    pub handshakes: u64,
    pub rebuilds: u64,
    pub idle_exited: bool,
}

/// serve 主循环（阻塞；对真相无所有权，任意时刻可 kill——红线5）。
///
/// 维护 = stat 快照轮询（自身维护调度，**不参与新鲜性证明**——证明只认 OS 变更
/// 日志订阅，红线21）+ USN 订阅消化。空闲自退 = 无握手且无变更超过 idle。
pub fn serve(store: &Store, idle: Duration, stop: impl Fn() -> bool) -> MemResult<ServeReport> {
    serve_with_maintainer(store, idle, stop, None)
}

/// 维护回调（B3 接线）：sidecar 检测到变更时调用——mem bin 注入 mem-index 物化器。
/// 返回本次物化的结果（代际 + **与索引同一次扫描**的 stat 集，r5 B3）——检查点
/// entries 采自该集合，物化与检查点恒同代。语义：回调**先于**检查点发布。
pub type Maintainer<'a> = &'a dyn Fn(&Store) -> MemResult<crate::freshness::MaintainOutcome>;

pub fn serve_with_maintainer(
    store: &Store,
    idle: Duration,
    stop: impl Fn() -> bool,
    maintainer: Option<Maintainer<'_>>,
) -> MemResult<ServeReport> {
    store.check_writable()?;
    // 单实例：serve.lock 独占（进程死亡自动释放——kill 安全）。
    let _single = {
        crate::paths::validate_store_root_lock(store, "serve.lock")?;
        let name = store.abs("serve.lock");
        match crate::platform::LockFile::try_acquire_exclusive(&name) {
            Ok(l) => l,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                return Err(MemError::LockTimeout {
                    lock_path: name.display().to_string(),
                    holder: None,
                })
            }
            Err(e) => return Err(MemError::io(name.display().to_string(), e)),
        }
    };
    let mut report = ServeReport::default();
    // 订阅先建立（收敛原语的新鲜查询需要它），再初始物化。
    let mut sub = attach_subscription(store);
    let mut last_stat = crate::freshness::stat_snapshot(store);
    if run_maintenance(store, maintainer, sub.as_mut())?.0 {
        report.rebuilds += 1;
    }
    let backend = sub.as_ref().map(|s| s.backend()).unwrap_or("none").to_string();
    store.update_config(|c| {
        c.subscription.supported = backend != "none";
        c.subscription.backend = if backend != "none" { Some(backend.clone()) } else { None };
    });
    let mut last_activity = Instant::now();
    let mut listener = Listener::bind(store)?;
    loop {
        if stop() {
            return Ok(report);
        }
        // 维护 tick：订阅**新鲜查询**（RDC 计数消化）+ 收敛发布（r4 B3）。
        let now_stat = crate::freshness::stat_snapshot(store);
        let dirty = now_stat != last_stat;
        match sub.as_mut().map(|s| s.fresh_undigested()) {
            Some(Some(n)) if n > 0 => {
                last_stat = now_stat.clone();
                if run_maintenance(store, maintainer, sub.as_mut())?.0 {
                    report.rebuilds += 1;
                }
            }
            Some(None) => {
                // 订阅失效（RDC 缓冲溢出/线程死亡）：回退 none（永久直读，安全侧）。
                sub = None;
                store.update_config(|c| {
                    c.subscription.supported = false;
                    c.subscription.backend = None;
                });
            }
            _ => {}
        }
        if dirty {
            last_stat = now_stat;
            if run_maintenance(store, maintainer, sub.as_mut())?.0 {
                report.rebuilds += 1;
            }
        }
        // 握手（非阻塞尝试）。B4：应答前**再次新鲜查询**——若仍有未消化变更则
        // 先经收敛原语维护再应答（Hello.undigested 恒为咨询时刻的新鲜值——r4 B3：
        // 收敛原语保证"维护后二次查询为 0"才发布检查点，维护期间二次写入不会被
        // 提前确认）。
        if listener.try_accept_and_reply(store, sub.as_mut(), maintainer) {
            report.handshakes += 1;
            last_activity = Instant::now();
        }
        // 空闲自退（D2）：无握手且无未处理变更（dirty = 维护欠账，欠账未清不得退）。
        if last_activity.elapsed() > idle && !dirty {
            report.idle_exited = true;
            return Ok(report);
        }
        std::thread::sleep(Duration::from_millis(400));
    }
}

/// 变更日志订阅抽象（r4 B6：**仅 RDC**——USN 保守停用）。
///
/// USN 停用原因：FRN 集合只在 attach 时快照且无调用方更新（`track()` 全工作区
/// 零调用），新记录与原子替换后的记录失去覆盖（r4 B6，提权验收缺失）——该分支
/// 不满足红线21。修复 FRN 维护并补提权验收后方可恢复（开放问题39）。
/// RDC 非提权可用、端到端已验证；两者皆不可用 → none（永久直读，正确性不受影响）。
enum Subscription {
    #[cfg(windows)]
    Rdc(Vec<crate::platform::RdcWatcher>), // records/ + attic/ 各一
}

impl Subscription {
    fn backend(&self) -> &'static str {
        match self {
            #[cfg(windows)]
            Subscription::Rdc(_) => "rdc",
            #[cfg(not(windows))]
            _ => "unavailable",
        }
    }

    /// 新鲜查询 + 消化（每次调用都直连 OS 源；None = 覆盖缺口/订阅失效）。
    fn fresh_undigested(&mut self) -> Option<u64> {
        match self {
            #[cfg(windows)]
            Subscription::Rdc(ws) => {
                let mut total = 0u64;
                for w in ws {
                    match w.fresh_undigested() {
                        Some(n) => total += n,
                        None => return None,
                    }
                }
                Some(total)
            }
            #[cfg(not(windows))]
            _ => None,
        }
    }

    /// M12：维护失败的信号回注（下次 fresh 查询返回 → tick 路径重试；
    /// 持续失败则 tick 路径的 run_maintenance Err 终止 serve——语义汇合）。
    fn reinject(&mut self, _n: u64) {
        match self {
            #[cfg(windows)]
            Subscription::Rdc(ws) => {
                // 回注到首个订阅者即可（合并计数语义）。
                if let Some(w) = ws.first() {
                    w.reinject(_n);
                }
            }
            #[cfg(not(windows))]
            _ => {},
        }
    }
}

fn attach_subscription(store: &Store) -> Option<Subscription> {
    #[cfg(windows)]
    {
        // r4 B6：仅 RDC。USN 保守停用（FRN 快照不随物化更新 → 新记录/原子替换
        // 失去覆盖；修复需提权验收，开放问题39）。H7：RDC 非提权可用；watch
        // 真相子树（records/ 与 attic/）；*.lock 机制锁文件不计（r13 B9-4 收紧：
        // 后缀匹配，且 .tmp- 不再过滤——多计数是安全侧，误吞记录事件违反红线21）。
        let mut watchers = Vec::new();
        for dir in ["records", "attic"] {
            let d = store.abs(dir);
            let _ = std::fs::create_dir_all(&d);
            match crate::platform::RdcWatcher::attach(&d, &[".lock"]) {
                Some(w) => watchers.push(w),
                None => return None, // 任一子树不可订阅 → 整体不可证明（安全侧）
            }
        }
        return Some(Subscription::Rdc(watchers));
    }
    #[cfg(not(windows))]
    {
        let _ = store;
        None
    }
}

/// 维护执行（r4 B3）：统一走 [`crate::freshness::publish_converged_checkpoint`]
/// ——维护 → 新鲜查询确认收敛 → 才发布检查点。
/// 返回 `(是否执行了维护, 是否收敛并发布检查点)`。未收敛/gap/无订阅证据 →
/// 索引可能已更新但检查点不发布（诚实降级，读路径直读）。
fn run_maintenance(
    store: &Store,
    maintainer: Option<Maintainer<'_>>,
    sub: Option<&mut Subscription>,
) -> MemResult<(bool, bool)> {
    let has_sub = sub.is_some();
    let noop: Maintainer<'static> = &|_s| {
        Ok(crate::freshness::MaintainOutcome {
            index_epoch: 0,
            scan_entries: Default::default(),
        })
    };
    let m: Maintainer = maintainer.unwrap_or(noop);
    let mut sub_cell = sub;
    let mut fresh_fn = move || -> Option<u64> { sub_cell.as_mut().and_then(|s| s.fresh_undigested()) };
    let fresh: Option<&mut dyn FnMut() -> Option<u64>> =
        if has_sub { Some(&mut fresh_fn) } else { None };
    let published = crate::freshness::publish_converged_checkpoint(
        store,
        &|s: &Store| m(s),
        fresh,
    )?;
    Ok((maintainer.is_some(), published.is_some()))
}

/// 平台监听器：Windows named pipe / macOS UDS（非阻塞单步语义）。
struct Listener {
    #[cfg(windows)]
    handle: crate::platform::PipeServer,
    #[cfg(target_os = "macos")]
    listener: std::os::unix::net::UnixListener,
}

impl Listener {
    fn bind(store: &Store) -> MemResult<Listener> {
        let name = endpoint_name(store.root());
        #[cfg(windows)]
        {
            Ok(Listener {
                handle: crate::platform::PipeServer::create(&name)
                    .map_err(|e| MemError::io(name.clone(), e))?,
            })
        }
        #[cfg(target_os = "macos")]
        {
            let _ = std::fs::remove_file(&name);
            let l = std::os::unix::net::UnixListener::bind(&name)
                .map_err(|e| MemError::io(name.clone(), e))?;
            let _ = l.set_nonblocking(true);
            Ok(Listener { listener: l })
        }
    }

    /// 非阻塞尝试接受一次连接：读请求 → 组装 Hello → 写回应答 → 断开。
    /// 返回是否发生了握手。
    ///
    /// B4：`undigested` 在**应答时刻**新鲜取得——仍有未消化变更时先执行维护
    ///（经收敛原语，r4 B3：维护后二次查询为 0 才算消化），Hello 恒反映咨询
    /// 时刻的真实订阅状态；未收敛/gap → 如实报告未消化（读路径直读）。
    fn try_accept_and_reply(
        &mut self,
        store: &Store,
        sub: Option<&mut Subscription>,
        maintainer: Option<Maintainer<'_>>,
    ) -> bool {
        let backend = sub.as_ref().map(|s| s.backend()).unwrap_or("none").to_string();
        let undigested = match sub {
            Some(s) => match s.fresh_undigested() {
                Some(n) if n > 0 => {
                    // 咨询时刻仍有未消化：立即消化（收敛原语维护）再应答。
                    match run_maintenance(store, maintainer, Some(&mut *s)) {
                        Ok((_, true)) => Some(0),
                        Ok((ran, false)) => {
                            // M12/r4 B3：维护执行了但未收敛（或不可证明）→ 信号
                            // 回注（不丢失）；如实报告未消化（读路径将直读）。
                            let _ = ran;
                            s.reinject(n);
                            Some(n)
                        }
                        Err(_) => {
                            s.reinject(n);
                            Some(n)
                        }
                    }
                }
                other => other,
            },
            None => None,
        };
        let hello = Hello {
            version: SIDECAR_VERSION,
            backend: backend.clone(),
            undigested,
            max_seq: crate::event::EventLog::new(store).max_seq().unwrap_or(0),
            last_rebuild: crate::freshness::load_checkpoint(store)
                .map(|c| c.ts)
                .unwrap_or_default(),
        };
        #[cfg(windows)]
        {
            self.handle.try_serve_one(&hello.as_bytes())
        }
        #[cfg(target_os = "macos")]
        {
            use std::io::{Read, Write};
            match self.listener.accept() {
                Ok((mut s, _)) => {
                    let mut buf = [0u8; 64];
                    let _ = s.read(&mut buf);
                    let _ = s.write_all(&hello.as_bytes());
                    true
                }
                Err(_) => false,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_is_deterministic_per_store() {
        let a = endpoint_name(Path::new("C:\\stores\\one"));
        let b = endpoint_name(Path::new("C:\\stores\\one"));
        let c = endpoint_name(Path::new("C:\\stores\\two"));
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn hello_roundtrip() {
        let h = Hello {
            version: 1,
            backend: "usn".into(),
            undigested: Some(0),
            max_seq: 42,
            last_rebuild: "2026-11-04T00:00:00Z".into(),
        };
        let bytes = h.as_bytes();
        let h2: Hello = serde_json::from_slice(&bytes[..bytes.len() - 1]).unwrap();
        assert_eq!(h2, h);
    }
}
