//! 数据目录（唯一真相）布局与打开（设计文档 §3.1；规范 envelope-format.md §1/§6）。
//!
//! 用户级单店单副本（R8.11）；隔离靠视图不靠物理分区（D10）。
//!
//! **只读形态判定**（红线18 精神）：判定不得写任何真相层数据。探测只触碰运行时
//! 临时区 `.tmp/`（探针文件，非真相），失败即按只读处理——写类操作全部结构化拒绝，
//! 读类操作（brief/list/show/trace/diff）全部可用（§3.4 跨档只读行）。

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::error::{MemError, MemResult};

/// 默认预算（开放问题25 的 v1 默认值；规范 envelope-format.md §6）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Budgets {
    pub brief_items: u32,
    pub brief_bytes: u32,
    pub list_limit: u32,
    pub list_limit_max: u32,
    pub trace_depth: u32,
    pub trace_nodes: u32,
    pub diff_since_limit: u32,
    pub current_drift_limit: u32,
}

impl Default for Budgets {
    fn default() -> Self {
        Self {
            brief_items: 30,
            brief_bytes: 16_384,
            list_limit: 50,
            list_limit_max: 200,
            trace_depth: 4,
            trace_nodes: 200,
            diff_since_limit: 200,
            current_drift_limit: 100,
        }
    }
}

/// 变更日志订阅能力声明（D5/开放问题39）：supported=false → 永久直读形态。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubscriptionConfig {
    pub supported: bool,
    pub backend: Option<String>, // null | "usn" | "fsevents"
}

impl Default for SubscriptionConfig {
    fn default() -> Self {
        Self { supported: false, backend: None }
    }
}

/// `mem init` 播种配置（开放问题40 定名：`init.template_dir`——git init.templateDir
/// 的对应物）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InitConfig {
    pub template_dir: Option<String>,
}

impl Default for InitConfig {
    fn default() -> Self {
        Self { template_dir: None }
    }
}

/// 库级配置（mem.json）。未知键保留在 `extra`（R2.4 人手编辑合法）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoreConfig {
    pub format_version: u32,
    pub created: String,
    #[serde(default)]
    pub budgets: Budgets,
    #[serde(default)]
    pub doubt_rules: Vec<serde_json::Value>,
    #[serde(default = "default_idle")]
    pub session_idle_secs: u64,
    #[serde(default)]
    pub subscription: SubscriptionConfig,
    #[serde(default)]
    pub init: InitConfig,
    /// 未知键保留（再生成不丢失）。
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

fn default_idle() -> u64 {
    86_400
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            format_version: 1,
            created: crate::clock::now_rfc3339(),
            budgets: Budgets::default(),
            doubt_rules: Vec::new(),
            session_idle_secs: default_idle(),
            subscription: SubscriptionConfig::default(),
            init: InitConfig::default(),
            extra: serde_json::Map::new(),
        }
    }
}

/// 打开模式：Auto = 自然探测可写性；ForceReadOnly = 只读形态（含可写卷）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenMode {
    Auto,
    ForceReadOnly,
}

#[derive(Debug)]
pub struct Store {
    root: PathBuf,
    config: Mutex<StoreConfig>,
    writable: bool,
}

impl Store {
    /// 建店（mem init 用；已存在的店响亮拒绝）。
    pub fn create(root: &Path) -> MemResult<()> {
        let mem_json = root.join("mem.json");
        if mem_json.exists() {
            return Err(MemError::BadArgument(format!(
                "目标已是记忆库：{root:?}（mem.json 已存在）"
            )));
        }
        for d in [
            "records", "attic", "sessions", "agenda", "types", "conventions",
            "index", ".tmp", ".pending-commits",
        ] {
            std::fs::create_dir_all(root.join(d))
                .map_err(|e| MemError::io(root.join(d).display().to_string(), e))?;
        }
        let config = StoreConfig::default();
        let json = serde_json::to_string_pretty(&config).map_err(|e| {
            MemError::Corrupt { path: "mem.json".into(), reason: e.to_string() }
        })?;
        std::fs::write(&mem_json, json).map_err(|e| MemError::io(mem_json.display().to_string(), e))?;
        // 事件日志：空文件占位（追加式、序号=行序）。
        std::fs::write(root.join("events.jsonl"), b"")
            .map_err(|e| MemError::io(root.join("events.jsonl").display().to_string(), e))?;
        // 约定单一事实源：最小头（内容由 mem init 播种或用户书写，R7.4）。
        let conventions_header = concat!(
            "# SpaceMemory 约定（CONVENTIONS）\n\n",
            "- version: 1\n\n",
            "（由 mem init 播种或用户书写；类型角色段由两件套拼装，见 conventions-source.md）\n",
        );
        std::fs::write(
            root.join("conventions/CONVENTIONS.md"),
            conventions_header.as_bytes(),
        )
        .map_err(|e| MemError::io(root.join("conventions/CONVENTIONS.md").display().to_string(), e))?;
        Ok(())
    }

    /// 打开店（mem.json 缺失 → 响亮错误）。
    pub fn open(root: &Path, mode: OpenMode) -> MemResult<Store> {
        let mem_json = root.join("mem.json");
        if !mem_json.is_file() {
            return Err(MemError::Corrupt {
                path: mem_json.display().to_string(),
                reason: "不是记忆库：缺 mem.json（迁移 = 复制整个目录，R2.3）".into(),
            });
        }
        let bytes =
            std::fs::read(&mem_json).map_err(|e| MemError::io(mem_json.display().to_string(), e))?;
        let config: StoreConfig = serde_json::from_slice(&bytes).map_err(|e| {
            MemError::Corrupt { path: "mem.json".into(), reason: e.to_string() }
        })?;
        let writable = match mode {
            OpenMode::ForceReadOnly => false,
            OpenMode::Auto => probe_writable(root),
        };
        Ok(Store { root: root.to_path_buf(), config: Mutex::new(config), writable })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn writable(&self) -> bool {
        self.writable
    }

    /// 写类操作入口的统一闸门。
    pub fn check_writable(&self) -> MemResult<()> {
        if self.writable {
            Ok(())
        } else {
            Err(MemError::ReadOnly)
        }
    }

    pub fn config(&self) -> StoreConfig {
        self.config.lock().expect("配置锁").clone()
    }

    pub fn update_config(&self, f: impl FnOnce(&mut StoreConfig)) {
        let mut c = self.config.lock().expect("配置锁");
        f(&mut c);
    }

    /// 相对路径（`/` 分隔）→ 绝对路径。
    pub fn abs(&self, rel: impl AsRef<std::path::Path>) -> PathBuf {
        self.root.join(rel)
    }

    /// 追加事件前的目录保障（幂等；只读店写前必经 check_writable）。
    pub fn ensure_dir(&self, rel: &str) -> MemResult<()> {
        let p = self.abs(rel);
        if p.is_dir() {
            return Ok(());
        }
        std::fs::create_dir_all(&p).map_err(|e| MemError::io(p.display().to_string(), e))
    }
}

/// 可写性探测：尝试在运行时临时区 `.tmp/` 创建探针文件（非真相层数据；
/// 红线18 的"读路径零写盘"指真相层与派生缓存——探针只判定能力，不留存）。
/// M6a 归档裁决（v1.2 评审）：Store::open 的探针写在 `.tmp/`（运行时区，规范
/// §1.1 明示无规范意义）——不违反红线18；替代方案（ACL 查询）跨平台不可靠。
fn probe_writable(root: &Path) -> bool {
    static NEXT_PROBE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let tmp = root.join(".tmp");
    if !tmp.is_dir() {
        // .tmp 无法创建 → 只读（或损坏；两者都按只读降级，写类操作拒绝）。
        if std::fs::create_dir_all(&tmp).is_err() {
            return false;
        }
    }
    let probe = tmp.join(format!(
        ".probe-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
        NEXT_PROBE.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
    ));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}
