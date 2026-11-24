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
        Self {
            supported: false,
            backend: None,
        }
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
    pub format_version: String,
    #[serde(default)]
    pub required_features: Vec<String>,
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

type RawConfig = std::collections::BTreeMap<String, String>;

fn parse_raw_config(bytes: &[u8]) -> MemResult<RawConfig> {
    fn corrupt(reason: impl Into<String>) -> MemError {
        MemError::Corrupt {
            path: "mem.json".into(),
            reason: reason.into(),
        }
    }
    fn skip_ws(bytes: &[u8], at: &mut usize) {
        while bytes.get(*at).is_some_and(u8::is_ascii_whitespace) {
            *at += 1;
        }
    }
    fn scan_string(bytes: &[u8], at: &mut usize) -> Result<(), MemError> {
        if bytes.get(*at) != Some(&b'"') {
            return Err(corrupt("配置对象键必须是 JSON 字符串"));
        }
        *at += 1;
        while let Some(byte) = bytes.get(*at) {
            match byte {
                b'\\' => {
                    *at += 2;
                }
                b'"' => {
                    *at += 1;
                    return Ok(());
                }
                _ => *at += 1,
            }
        }
        Err(corrupt("配置 JSON 字符串未闭合"))
    }
    fn scan_value(bytes: &[u8], at: &mut usize) -> Result<(), MemError> {
        let Some(first) = bytes.get(*at).copied() else {
            return Err(corrupt("配置 JSON 值缺失"));
        };
        if first == b'"' {
            return scan_string(bytes, at);
        }
        if first == b'{' || first == b'[' {
            let mut closing = vec![if first == b'{' { b'}' } else { b']' }];
            *at += 1;
            let mut in_string = false;
            let mut escaped = false;
            while let Some(byte) = bytes.get(*at).copied() {
                *at += 1;
                if in_string {
                    if escaped {
                        escaped = false;
                    } else if byte == b'\\' {
                        escaped = true;
                    } else if byte == b'"' {
                        in_string = false;
                    }
                    continue;
                }
                match byte {
                    b'"' => in_string = true,
                    b'{' => closing.push(b'}'),
                    b'[' => closing.push(b']'),
                    b'}' | b']' => {
                        if closing.pop() != Some(byte) {
                            return Err(corrupt("配置 JSON 容器不匹配"));
                        }
                        if closing.is_empty() {
                            return Ok(());
                        }
                    }
                    _ => {}
                }
            }
            return Err(corrupt("配置 JSON 容器未闭合"));
        }
        let start = *at;
        while bytes
            .get(*at)
            .is_some_and(|byte| !byte.is_ascii_whitespace() && !matches!(byte, b',' | b'}' | b']'))
        {
            *at += 1;
        }
        if *at == start {
            return Err(corrupt("配置 JSON 值缺失"));
        }
        Ok(())
    }

    serde_json::from_slice::<serde::de::IgnoredAny>(bytes).map_err(|e| corrupt(e.to_string()))?;
    let mut at = 0;
    skip_ws(bytes, &mut at);
    if bytes.get(at) != Some(&b'{') {
        return Err(corrupt("顶层配置必须是对象"));
    }
    at += 1;
    let mut result = RawConfig::new();
    loop {
        skip_ws(bytes, &mut at);
        if bytes.get(at) == Some(&b'}') {
            at += 1;
            break;
        }
        let key_start = at;
        scan_string(bytes, &mut at)?;
        let key: String =
            serde_json::from_slice(&bytes[key_start..at]).map_err(|e| corrupt(e.to_string()))?;
        skip_ws(bytes, &mut at);
        if bytes.get(at) != Some(&b':') {
            return Err(corrupt("配置 JSON 对象缺少冒号"));
        }
        at += 1;
        skip_ws(bytes, &mut at);
        let value_start = at;
        scan_value(bytes, &mut at)?;
        let value = std::str::from_utf8(&bytes[value_start..at])
            .map_err(|e| corrupt(e.to_string()))?
            .to_owned();
        serde_json::from_str::<serde::de::IgnoredAny>(&value)
            .map_err(|e| corrupt(e.to_string()))?;
        result.insert(key, value);
        skip_ws(bytes, &mut at);
        match bytes.get(at) {
            Some(b',') => at += 1,
            Some(b'}') => {
                at += 1;
                break;
            }
            _ => return Err(corrupt("配置 JSON 对象缺少逗号或结束括号")),
        }
    }
    skip_ws(bytes, &mut at);
    if at != bytes.len() {
        return Err(corrupt("配置 JSON 对象后含有多余内容"));
    }
    Ok(result)
}

fn raw_value<T: serde::Serialize>(value: &T) -> MemResult<String> {
    serde_json::to_string(value).map_err(|e| MemError::Corrupt {
        path: "mem.json".into(),
        reason: e.to_string(),
    })
}

fn raw_config_value(raw: &RawConfig) -> MemResult<serde_json::Value> {
    serde_json::from_str(&serialize_raw_config(raw, false)).map_err(|e| MemError::Corrupt {
        path: "mem.json".into(),
        reason: e.to_string(),
    })
}

fn serialize_raw_config(raw: &RawConfig, pretty: bool) -> String {
    let mut output = String::from("{");
    for (index, (key, value)) in raw.iter().enumerate() {
        if pretty {
            output.push_str(if index == 0 { "\n  " } else { ",\n  " });
        } else if index > 0 {
            output.push(',');
        }
        output.push_str(&serde_json::to_string(key).expect("字符串键可序列化"));
        output.push(':');
        if pretty {
            output.push(' ');
        }
        output.push_str(value);
    }
    if pretty && !raw.is_empty() {
        output.push('\n');
    }
    output.push('}');
    output
}

fn validate_feature_id(feature: &str) -> MemResult<()> {
    let invalid = || MemError::Corrupt {
        path: "mem.json".into(),
        reason: format!("required_features 标识非法: {feature}"),
    };
    let Some((name, version)) = feature.rsplit_once("/v") else {
        return Err(invalid());
    };
    let Some((namespace, capability)) = name.split_once('.') else {
        return Err(invalid());
    };
    let valid_part = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
    };
    if !valid_part(namespace)
        || !valid_part(capability)
        || version.is_empty()
        || !version.bytes().all(|b| b.is_ascii_digit())
        || version.parse::<u32>().ok().filter(|v| *v > 0).is_none()
    {
        return Err(invalid());
    }
    Ok(())
}

fn validate_registered_feature_id(feature: &str) -> MemResult<()> {
    let valid = feature.rsplit_once("/v").is_some_and(|(name, version)| {
        let Some((namespace, capability)) = name.split_once('.') else {
            return false;
        };
        let valid_part = |part: &str| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
        };
        valid_part(namespace)
            && valid_part(capability)
            && !version.is_empty()
            && version.bytes().all(|b| b.is_ascii_digit())
            && version.parse::<u32>().ok().is_some_and(|v| v > 0)
    });
    if valid {
        Ok(())
    } else {
        Err(MemError::InvalidFeature(feature.to_owned()))
    }
}

fn validate_required_features(features: &[String]) -> MemResult<()> {
    let mut seen = std::collections::BTreeSet::new();
    for feature in features {
        validate_feature_id(feature)?;
        if !seen.insert(feature.as_str()) {
            return Err(MemError::Corrupt {
                path: "mem.json".into(),
                reason: format!("required_features 重复: {feature}"),
            });
        }
    }
    Ok(())
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            format_version: "spacememory/2".into(),
            required_features: Vec::new(),
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

/// 绑定初始化目标路径的独占许可；私有字段确保无法伪造。
pub struct InitPermit {
    root: PathBuf,
    _lock: crate::platform::LockFile,
}

impl InitPermit {
    pub(crate) fn check_root(&self, root: &Path) -> MemResult<()> {
        let parent = effective_parent(root);
        let leaf = root.file_name().ok_or_else(|| {
            MemError::BadArgument(format!("初始化路径必须有目录名: {}", root.display()))
        })?;
        let canonical = std::fs::canonicalize(parent)
            .map_err(|e| MemError::io(parent.display().to_string(), e))?
            .join(leaf);
        if canonical == self.root {
            Ok(())
        } else {
            Err(MemError::BadArgument("初始化许可属于另一目标路径".into()))
        }
    }

    /// 播种只允许在仍持有 CLI 初始化标记的自举阶段发生。
    pub(crate) fn check_initializing(&self, root: &Path) -> MemResult<()> {
        self.check_root(root)?;
        if !root.join(Store::INIT_IN_PROGRESS).is_file() {
            return Err(MemError::BadArgument(
                "初始化播种许可不处于活动的 mem init 自举阶段".into(),
            ));
        }
        Ok(())
    }
}

fn effective_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

#[derive(Debug)]
pub struct Store {
    root: PathBuf,
    config: Mutex<StoreConfig>,
    writable: bool,
    supported_features: Vec<String>,
}

impl Store {
    const INIT_IN_PROGRESS: &'static str = ".init-in-progress";
    /// 首个受防护的数据目录基础格式标识。
    pub const CURRENT_FORMAT: &'static str = "spacememory/2";
    /// 当前仍可显式只读打开的历史基础格式。
    pub const READ_ONLY_FORMATS: &'static [&'static str] = &["1"];

    /// 构建时能力表。正文恢复写入专属事件语义，启用时整店记录该依赖。
    pub fn supported_features() -> &'static [&'static str] {
        &["event.restore_body/v1"]
    }

    /// 当前构建声明可按真相层直读的特性。未列出的缺项必须在打开阶段拒绝。
    /// 发布构建恒为声明值；debug 构建可被 [`set_read_compatible_features_for_test`]
    /// 临时覆盖（仅测试构造前向兼容场景——评审 B2 要求受控能力表覆盖降级直读
    /// 路径；覆盖不得进入发布构建，对外承诺不可绕过）。
    pub fn read_compatible_features() -> &'static [&'static str] {
        #[cfg(debug_assertions)]
        if let Some(leaked) = READ_COMPATIBLE_OVERRIDE.lock().unwrap().as_ref() {
            return leaked;
        }
        &[]
    }

    /// 将当前格式和构建能力导出为稳定机读契约。
    pub fn capability_manifest() -> serde_json::Value {
        serde_json::json!({
            "schema_version": 1,
            "product_version": env!("CARGO_PKG_VERSION"),
            "formats": {
                "read_write": [Self::CURRENT_FORMAT],
                "read_only": Self::READ_ONLY_FORMATS,
            },
            "features": {
                "supported": Self::supported_features(),
                "read_compatible": Self::read_compatible_features(),
            }
        })
    }

    /// 持有贯穿整个 CLI 初始化流程的路径级互斥租约。
    pub fn acquire_init_lock(root: &Path) -> MemResult<InitPermit> {
        let parent = effective_parent(root);
        std::fs::create_dir_all(parent)
            .map_err(|e| MemError::io(parent.display().to_string(), e))?;
        let leaf = root.file_name().ok_or_else(|| {
            MemError::BadArgument(format!("初始化路径必须有目录名: {}", root.display()))
        })?;
        let lock_path = parent.join(format!(".{}.init.lock", leaf.to_string_lossy()));
        let lock = crate::platform::LockFile::try_acquire_exclusive(&lock_path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::WouldBlock {
                MemError::LockTimeout {
                    lock_path: lock_path.display().to_string(),
                    holder: None,
                }
            } else {
                MemError::io(lock_path.display().to_string(), e)
            }
        })?;
        let canonical_parent = std::fs::canonicalize(parent)
            .map_err(|e| MemError::io(parent.display().to_string(), e))?;
        Ok(InitPermit {
            root: canonical_parent.join(leaf),
            _lock: lock,
        })
    }

    /// 标记 CLI 完整自举边界；若上次在 marker 下中断，先将半成品原样隔离到同级目录。
    pub fn begin_cli_init(root: &Path, permit: &InitPermit) -> MemResult<Option<PathBuf>> {
        permit.check_root(root)?;
        let marker = root.join(Self::INIT_IN_PROGRESS);
        let mut quarantined = None;
        if marker.exists() {
            let parent = effective_parent(root);
            let leaf = root.file_name().ok_or_else(|| {
                MemError::BadArgument(format!("初始化路径必须有目录名: {}", root.display()))
            })?;
            let quarantine = parent.join(format!(
                ".{}.incomplete-{}",
                leaf.to_string_lossy(),
                crate::envelope::new_uuid_v7()
            ));
            std::fs::rename(root, &quarantine)
                .map_err(|e| MemError::io(root.display().to_string(), e))?;
            quarantined = Some(quarantine);
        } else if root.join("mem.json").exists() {
            return Err(MemError::BadArgument(format!(
                "目标已是记忆库：{}（mem.json 已存在）",
                root.display()
            )));
        }
        std::fs::create_dir_all(root).map_err(|e| MemError::io(root.display().to_string(), e))?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&marker)
            .map_err(|e| MemError::io(marker.display().to_string(), e))?;
        use std::io::Write;
        file.write_all(b"CLI initialization in progress\n")
            .and_then(|_| file.sync_all())
            .map_err(|e| MemError::io(marker.display().to_string(), e))?;
        Ok(quarantined)
    }

    /// 完整自举成功后清除中断标记；之后普通 Store::open 才会接受该店。
    pub fn finish_cli_init(root: &Path, permit: &InitPermit) -> MemResult<()> {
        permit.check_root(root)?;
        let marker = root.join(Self::INIT_IN_PROGRESS);
        std::fs::remove_file(&marker).map_err(|e| MemError::io(marker.display().to_string(), e))
    }

    /// 在独占门闩内追加一个基础格式内的必需特性声明。
    /// 调用者必须在成功返回后才写入依赖该特性的任何数据。
    pub fn register_required_features(&self, features: &[&str]) -> MemResult<()> {
        if features.is_empty() {
            return Ok(());
        }
        self.check_writable()?;
        let barrier = crate::lock::barrier_exclusive(self)?;
        self.register_required_features_under_exclusive(features, &barrier)
    }

    /// 调用方已持同一店独占门闩时登记特性，供“登记后立即写依赖数据”的组合操作使用。
    pub(crate) fn register_required_features_under_exclusive(
        &self,
        features: &[&str],
        _barrier: &crate::lock::ExclusiveBarrierGuard,
    ) -> MemResult<()> {
        if features.is_empty() {
            return Ok(());
        }
        let mut requested = features.iter().map(|v| (*v).to_owned()).collect::<Vec<_>>();
        requested.sort();
        requested.dedup();
        let mut unsupported = Vec::new();
        for feature in &requested {
            validate_registered_feature_id(feature)?;
            if !Self::supported_features().contains(&feature.as_str()) {
                unsupported.push(feature.clone());
            }
        }
        if !unsupported.is_empty() {
            return Err(MemError::MissingFeatures(unsupported));
        }
        self.check_writable()?;
        self.check_compatible_for_write()?;
        let path = self.abs("mem.json");
        let bytes =
            std::fs::read(&path).map_err(|e| MemError::io(path.display().to_string(), e))?;
        let mut raw = parse_raw_config(&bytes)?;
        let mut config = Self::parse_disk_config_value(raw_config_value(&raw)?)?;
        if config.format_version != Self::CURRENT_FORMAT {
            return Err(MemError::RequiresMigration {
                current: config.format_version,
                target: Self::CURRENT_FORMAT.into(),
            });
        }
        let before = config.required_features.len();
        config.required_features.extend(requested);
        config.required_features.sort();
        config.required_features.dedup();
        if config.required_features.len() != before {
            raw.insert(
                "required_features".into(),
                raw_value(&config.required_features)?,
            );
            // 未修改字段以原始 JSON 文本保存，避免先解析再写回时改变未知值。
            config = Self::parse_disk_config_value(raw_config_value(&raw)?)?;
            let updated = serialize_raw_config(&raw, true);
            crate::platform::atomic_write(&path, updated.as_bytes())
                .map_err(|e| MemError::io(path.display().to_string(), e))?;
        }
        *self.config.lock().expect("配置锁") = config;
        Ok(())
    }

    /// 停机迁移数字 1 配置到首个防护格式。
    /// 调用者必须先完成方案 §5 的服务停机、句柄关闭与店外完整备份；锁只协调遵守
    /// 新协议的进程，不能证明历史二进制已退出。
    pub fn migrate_format_v1(root: &Path, backup_identity: &str) -> MemResult<MigrationReport> {
        Self::migrate_format_v1_inner(root, backup_identity, None)
    }

    /// 仅供 debug 集成测试在原子替换边界暂停；不进入 release 发布构建。
    #[doc(hidden)]
    #[cfg(all(debug_assertions, not(test)))]
    pub fn migrate_format_v1_test_with_pause(
        root: &Path,
        backup_identity: &str,
        hook: impl Fn(&str),
    ) -> MemResult<MigrationReport> {
        Self::migrate_format_v1_inner(root, backup_identity, Some(&hook))
    }

    fn migrate_format_v1_inner(
        root: &Path,
        backup_identity: &str,
        pause_hook: Option<&dyn Fn(&str)>,
    ) -> MemResult<MigrationReport> {
        if backup_identity.trim().is_empty() {
            return Err(MemError::BadArgument("迁移必须记录非空的备份身份".into()));
        }
        if root.join(Self::INIT_IN_PROGRESS).exists() {
            return Err(MemError::MigrationState(format!(
                "店初始化未完成：{}；先通过 mem init 隔离并恢复半成品",
                root.display()
            )));
        }
        let root =
            std::fs::canonicalize(root).map_err(|e| MemError::io(root.display().to_string(), e))?;
        let lock_path = root.join("migration.lock");
        crate::paths::validate_store_root_lock(
            &Store {
                root: root.to_path_buf(),
                config: Mutex::new(StoreConfig::default()),
                writable: true,
                supported_features: Vec::new(),
            },
            "migration.lock",
        )?;
        let _lock = crate::platform::LockFile::try_acquire_exclusive(&lock_path)
            .map_err(|e| MemError::io(lock_path.display().to_string(), e))?;
        let migration_store = Store {
            root: root.to_path_buf(),
            config: Mutex::new(StoreConfig::default()),
            writable: true,
            supported_features: Vec::new(),
        };
        let _barrier = crate::lock::try_barrier_exclusive(&migration_store)?;
        let config_path = root.join("mem.json");
        let bytes = std::fs::read(&config_path)
            .map_err(|e| MemError::io(config_path.display().to_string(), e))?;
        let mut migration_raw = parse_raw_config(&bytes)?;
        let migrated_features;
        let version: Option<serde_json::Value> = migration_raw
            .get("format_version")
            .map(|raw| serde_json::from_str(raw))
            .transpose()
            .map_err(|e| MemError::Corrupt {
                path: "mem.json".into(),
                reason: e.to_string(),
            })?;
        match version.as_ref() {
            Some(serde_json::Value::Number(n)) if n.as_u64() == Some(1) => {
                // 基线版本的配置不能携带受保护格式字段；非空清单不是安全可忽略信息。
                if let Some(raw_features) = migration_raw.get("required_features") {
                    let features: serde_json::Value =
                        serde_json::from_str(raw_features).map_err(|e| MemError::Corrupt {
                            path: "mem.json".into(),
                            reason: e.to_string(),
                        })?;
                    if features.as_array().is_none_or(|items| !items.is_empty()) {
                        return Err(MemError::MigrationState(
                            "旧格式含不可安全迁移的必需特性".into(),
                        ));
                    }
                }
                migrated_features = validate_legacy_store(&root)?;
                // 目标配置原子替换是唯一完成点：替换失败保留源标记，核验备份后可重试。
            }
            Some(serde_json::Value::String(v)) if v == "spacememory/2" => {
                // 已迁移判断必须与 Store::open 共用完整磁盘配置校验；只看
                // required_features 会把 created=null 等正式打开拒绝的店报为成功。
                Self::parse_disk_config_value(raw_config_value(&migration_raw)?)?;
                return Ok(MigrationReport {
                    status: MigrationStatus::AlreadyMigrated,
                    backup_identity: backup_identity.into(),
                });
            }
            Some(serde_json::Value::Number(n)) => {
                return Err(MemError::UnsupportedFormat {
                    found: n.to_string(),
                    supported: vec!["1".into(), "spacememory/2".into()],
                });
            }
            Some(serde_json::Value::String(v)) => {
                return Err(MemError::UnsupportedFormat {
                    found: v.clone(),
                    supported: vec!["1".into(), "spacememory/2".into()],
                });
            }
            _ => {
                return Err(MemError::Corrupt {
                    path: "mem.json".into(),
                    reason: "缺少或非法的 format_version".into(),
                });
            }
        }
        migration_raw.insert("format_version".into(), raw_value(&"spacememory/2")?);
        migration_raw.insert("required_features".into(), raw_value(&migrated_features)?);
        let updated = serialize_raw_config(&migration_raw, true);
        if let Some(hook) = pause_hook {
            hook("before_format_switch");
        }
        crate::platform::atomic_write(&config_path, updated.as_bytes())
            .map_err(|e| MemError::io(config_path.display().to_string(), e))?;
        if let Some(hook) = pause_hook {
            hook("after_format_switch");
        }
        Ok(MigrationReport {
            status: MigrationStatus::Migrated,
            backup_identity: backup_identity.into(),
        })
    }

    /// 获取专用于派生索引收敛的独占维护许可。
    pub fn acquire_index_maintenance(&self) -> MemResult<IndexMaintenancePermit<'_>> {
        self.check_writable()?;
        let barrier = crate::lock::barrier_exclusive(self)?;
        self.check_compatible_for_write()?;
        Ok(IndexMaintenancePermit {
            store: self,
            barrier,
        })
    }

    /// 将 sidecar 已取得的独占门闩包装为派生索引维护许可。
    pub fn index_maintenance_from_guard(
        &self,
        barrier: crate::lock::ExclusiveBarrierGuard,
    ) -> MemResult<IndexMaintenancePermit<'_>> {
        self.check_writable()?;
        barrier.check_store(self)?;
        Ok(IndexMaintenancePermit {
            store: self,
            barrier,
        })
    }

    /// 在独占门闩内登记本版本实现的整店必需特性。
    pub fn register_required_feature(&self, feature: &str) -> MemResult<()> {
        self.register_required_features(&[feature])
    }

    /// 在独占门闩内更新调用者配置键，保留其余原始 JSON 字段。
    pub fn set_config_value(&self, key: &str, value: serde_json::Value) -> MemResult<()> {
        if matches!(key, "format_version" | "required_features") {
            return Err(MemError::BadArgument(format!(
                "{key} 必须通过专用格式迁移或特性登记操作更新"
            )));
        }
        self.check_writable()?;
        let _barrier = crate::lock::barrier_exclusive(self)?;
        self.check_compatible_for_write()?;
        let path = self.abs("mem.json");
        let bytes =
            std::fs::read(&path).map_err(|e| MemError::io(path.display().to_string(), e))?;
        let mut raw = parse_raw_config(&bytes)?;
        raw.insert(key.to_owned(), raw_value(&value)?);
        let config = Self::parse_disk_config_value(raw_config_value(&raw)?)?;
        let updated = serialize_raw_config(&raw, true);
        crate::platform::atomic_write(&path, updated.as_bytes())
            .map_err(|e| MemError::io(path.display().to_string(), e))?;
        *self.config.lock().expect("配置锁") = config;
        Ok(())
    }

    /// 建店（mem init 用；已存在的店响亮拒绝）。
    pub fn create(root: &Path) -> MemResult<()> {
        let permit = Self::acquire_init_lock(root)?;
        Self::create_under_init_lock(root, &permit)
    }

    /// 调用者持有 [`Self::acquire_init_lock`] 返回的初始化租约时建店。
    pub fn create_under_init_lock(root: &Path, permit: &InitPermit) -> MemResult<()> {
        Self::create_under_init_lock_inner(root, permit, None)
    }

    /// 仅供 debug 集成验收在配置发布前暂停；发布构建不暴露此入口。
    #[doc(hidden)]
    #[cfg(all(debug_assertions, not(test)))]
    pub fn create_under_init_lock_test_with_pause(
        root: &Path,
        permit: &InitPermit,
        hook: impl Fn(),
    ) -> MemResult<()> {
        Self::create_under_init_lock_inner(root, permit, Some(&hook))
    }

    fn create_under_init_lock_inner(
        root: &Path,
        permit: &InitPermit,
        before_config_publish: Option<&dyn Fn()>,
    ) -> MemResult<()> {
        permit.check_root(root)?;
        let mem_json = root.join("mem.json");
        if mem_json.exists() {
            return Err(MemError::BadArgument(format!(
                "目标已是记忆库：{root:?}（mem.json 已存在）"
            )));
        }
        for d in [
            "records",
            "attic",
            "sessions",
            "agenda",
            "types",
            "conventions",
            "index",
            ".tmp",
            ".pending-commits",
        ] {
            std::fs::create_dir_all(root.join(d))
                .map_err(|e| MemError::io(root.join(d).display().to_string(), e))?;
        }
        let config = StoreConfig::default();
        let json = serde_json::to_string_pretty(&config).map_err(|e| MemError::Corrupt {
            path: "mem.json".into(),
            reason: e.to_string(),
        })?;
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
        .map_err(|e| {
            MemError::io(
                root.join("conventions/CONVENTIONS.md")
                    .display()
                    .to_string(),
                e,
            )
        })?;
        // mem.json 是公开 open 的唯一完成标记；其余布局准备好后才原子发布。
        if let Some(hook) = before_config_publish {
            hook();
        }
        crate::platform::atomic_write(&mem_json, json.as_bytes())
            .map_err(|e| MemError::io(mem_json.display().to_string(), e))?;
        Ok(())
    }

    /// 打开店（mem.json 缺失或 CLI 初始化中断 → 响亮错误）。
    pub fn open(root: &Path, mode: OpenMode) -> MemResult<Store> {
        if root.join(Self::INIT_IN_PROGRESS).exists() {
            return Err(MemError::MigrationState(format!(
                "店初始化未完成：{}；请重新运行 mem init 以隔离半成品并恢复",
                root.display()
            )));
        }
        Self::open_unchecked(root, mode)
    }

    /// CLI 自举期间在持有目标路径初始化许可时打开尚未发布的店。
    pub fn open_under_init_lock(
        root: &Path,
        mode: OpenMode,
        permit: &InitPermit,
    ) -> MemResult<Store> {
        permit.check_root(root)?;
        Self::open_unchecked(root, mode)
    }

    fn open_unchecked(root: &Path, mode: OpenMode) -> MemResult<Store> {
        let mem_json = root.join("mem.json");
        if !mem_json.is_file() {
            return Err(MemError::Corrupt {
                path: mem_json.display().to_string(),
                reason: "不是记忆库：缺 mem.json（迁移 = 复制整个目录，R2.3）".into(),
            });
        }
        let bytes = std::fs::read(&mem_json)
            .map_err(|e| MemError::io(mem_json.display().to_string(), e))?;
        let mut raw: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|e| MemError::Corrupt {
                path: "mem.json".into(),
                reason: e.to_string(),
            })?;
        let version_value =
            raw.get("format_version")
                .cloned()
                .ok_or_else(|| MemError::Corrupt {
                    path: "mem.json".into(),
                    reason: "缺少或非法的 format_version".into(),
                })?;
        let version = if let Some(n) = version_value.as_u64() {
            n.to_string()
        } else if let Some(s) = version_value.as_str() {
            s.to_owned()
        } else {
            return Err(MemError::Corrupt {
                path: "mem.json".into(),
                reason: "缺少或非法的 format_version".into(),
            });
        };
        if version != "1" && version != "spacememory/2" {
            return Err(MemError::UnsupportedFormat {
                found: version,
                supported: vec!["1 (只读)".into(), "spacememory/2".into()],
            });
        }
        raw["format_version"] = serde_json::Value::String(version);
        let mut read_only_for_features = false;
        if raw["format_version"] == "spacememory/2" {
            let required = raw
                .get("required_features")
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| MemError::Corrupt {
                    path: "mem.json".into(),
                    reason: "spacememory/2 缺少 required_features 数组".into(),
                })?;
            if required.iter().any(|v| v.as_str().is_none()) {
                return Err(MemError::Corrupt {
                    path: "mem.json".into(),
                    reason: "required_features 必须为字符串数组".into(),
                });
            }
        }
        // serde(flatten) 会把任意精度数字交给 typed Map visitor，serde_json 会以
        // u128 访问器拒绝超过 u128 的未知数。将未知键从 Value 中分离、独立保留，
        // 只反序列化本程序认识的字段。
        let config = Self::parse_disk_config_value(raw)?;
        validate_required_features(&config.required_features)?;
        if config.format_version == "1" && mode != OpenMode::ForceReadOnly {
            return Err(MemError::RequiresMigration {
                current: "1".into(),
                target: "spacememory/2".into(),
            });
        }
        let supported_features: Vec<String> = Self::supported_features()
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        let mut missing: Vec<String> = config
            .required_features
            .iter()
            .filter(|feature| !supported_features.contains(feature))
            .cloned()
            .collect();
        missing.sort();
        if !missing.is_empty() {
            let can_read = missing
                .iter()
                .all(|f| Self::read_compatible_features().contains(&f.as_str()));
            if mode == OpenMode::ForceReadOnly && can_read {
                read_only_for_features = true;
            } else {
                return Err(MemError::MissingFeatures(missing));
            }
        }
        let writable = match mode {
            OpenMode::ForceReadOnly => false,
            OpenMode::Auto if read_only_for_features => false,
            OpenMode::Auto => probe_writable(root),
        };
        Ok(Store {
            root: root.to_path_buf(),
            config: Mutex::new(config),
            writable,
            supported_features,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn physical_root(&self) -> MemResult<PathBuf> {
        std::fs::canonicalize(&self.root)
            .map_err(|e| MemError::io(self.root.display().to_string(), e))
    }

    pub(crate) fn same_physical_store(&self, other: &Store) -> MemResult<bool> {
        Ok(self.physical_root()? == other.physical_root()?)
    }

    pub fn writable(&self) -> bool {
        self.writable
    }

    /// 写类操作入口的统一闸门。
    pub fn check_writable(&self) -> MemResult<()> {
        if !self.writable {
            return Err(MemError::ReadOnly);
        }
        self.check_compatible_for_write()
    }

    /// 在调用方已持有店级门闩时重新核对磁盘格式与整店必需特性。
    pub fn check_compatible_for_write(&self) -> MemResult<()> {
        let disk_config = self.read_disk_config()?;
        if disk_config.format_version != "spacememory/2" {
            return Err(MemError::RequiresMigration {
                current: disk_config.format_version,
                target: "spacememory/2".into(),
            });
        }
        let missing: Vec<String> = disk_config
            .required_features
            .iter()
            .filter(|feature| !self.supported_features.contains(feature))
            .cloned()
            .collect();
        let mut missing = missing;
        missing.sort();
        if !missing.is_empty() {
            return Err(MemError::MissingFeatures(missing));
        }
        Ok(())
    }

    /// 重读磁盘并判定当前句柄可读的缺项。若任一缺项未声明只读兼容则拒绝；
    /// 返回非空缺项时调用方必须走真相层直读，不得使用可能含新语义的派生索引。
    pub fn read_compatible_missing_features(&self) -> MemResult<Vec<String>> {
        read_compatible_missing_features_at(self.root())
    }

    fn read_disk_config(&self) -> MemResult<StoreConfig> {
        let path = self.abs("mem.json");
        let bytes =
            std::fs::read(&path).map_err(|e| MemError::io(path.display().to_string(), e))?;
        let raw: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|e| MemError::Corrupt {
                path: "mem.json".into(),
                reason: e.to_string(),
            })?;
        Self::parse_disk_config_value(raw)
    }

    fn parse_disk_config_value(mut raw: serde_json::Value) -> MemResult<StoreConfig> {
        let version = raw
            .get("format_version")
            .and_then(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .or_else(|| v.as_u64().map(|n| n.to_string()))
            })
            .ok_or_else(|| MemError::Corrupt {
                path: "mem.json".into(),
                reason: "缺少或非法的 format_version".into(),
            })?;
        if version != "1" && version != "spacememory/2" {
            return Err(MemError::UnsupportedFormat {
                found: version,
                supported: vec!["1 (只读)".into(), "spacememory/2".into()],
            });
        }
        raw["format_version"] = serde_json::Value::String(version);
        if raw["format_version"] == "spacememory/2" {
            let required = raw
                .get("required_features")
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| MemError::Corrupt {
                    path: "mem.json".into(),
                    reason: "spacememory/2 缺少 required_features 数组".into(),
                })?;
            if required.iter().any(|v| v.as_str().is_none()) {
                return Err(MemError::Corrupt {
                    path: "mem.json".into(),
                    reason: "required_features 必须为字符串数组".into(),
                });
            }
        }
        let known = [
            "format_version",
            "required_features",
            "created",
            "budgets",
            "doubt_rules",
            "session_idle_secs",
            "subscription",
            "init",
        ];
        let object = raw.as_object_mut().expect("配置顶层对象已校验");
        let extras = object
            .iter()
            .filter(|(key, _)| !known.contains(&key.as_str()))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<serde_json::Map<_, _>>();
        object.retain(|key, _| known.contains(&key.as_str()));
        let mut config: StoreConfig =
            serde_json::from_value(raw).map_err(|e| MemError::Corrupt {
                path: "mem.json".into(),
                reason: e.to_string(),
            })?;
        config.extra = extras;
        validate_required_features(&config.required_features)?;
        Ok(config)
    }

    /// 读取当前落盘的基础格式标记，不使用 Store 打开时缓存的配置副本。
    pub fn current_format_version(&self) -> MemResult<String> {
        Ok(self.read_disk_config()?.format_version)
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
}

/// 测试专用的只读兼容表覆盖（仅 debug 构建存在）。集成测试用它构造
/// 「未来真实特性被声明只读兼容」的前向兼容场景（评审 B2 要求受控能力表
/// 覆盖降级直读路径）。表内容按调用泄漏为 'static——每次设置生效至进程
/// 结束，各集成测试二进制进程隔离，互不可见。
#[cfg(debug_assertions)]
static READ_COMPATIBLE_OVERRIDE: std::sync::Mutex<Option<&'static [&'static str]>> =
    std::sync::Mutex::new(None);

/// 设置只读兼容覆盖表（仅 debug 构建暴露：集成测试经 dev profile 可见，
/// `--release` 构建不存在该符号——与既有 debug-only fixture 纪律一致）。
#[cfg(debug_assertions)]
pub fn set_read_compatible_features_for_test(features: &'static [&'static str]) {
    let leaked: &'static [&'static str] = Box::leak(features.to_vec().into_boxed_slice());
    *READ_COMPATIBLE_OVERRIDE.lock().unwrap() = Some(leaked);
}

/// 磁盘兼容复核的无句柄形态（评审 B2）：索引等长活对象在**查询返回边界**
/// 重读磁盘配置用——构造期检查不能撤权，另一进程可在句柄存续期间登记本构建
/// 不认识的特性。语义与 [`Store::read_compatible_missing_features`] 逐行一致
///（双入口单源）：未声明只读兼容的缺项 → `Err(MissingFeatures)`；仅有只读
/// 兼容缺项 → `Ok(missing)`（调用方须转真相层直读）；无缺项 → `Ok(vec![])`。
pub fn read_compatible_missing_features_at(root: &Path) -> MemResult<Vec<String>> {
    let path = root.join("mem.json");
    let bytes = std::fs::read(&path).map_err(|e| MemError::io(path.display().to_string(), e))?;
    let raw: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| MemError::Corrupt {
        path: "mem.json".into(),
        reason: e.to_string(),
    })?;
    let disk_config = Store::parse_disk_config_value(raw)?;
    let supported = Store::supported_features();
    let mut missing: Vec<String> = disk_config
        .required_features
        .iter()
        .filter(|feature| !supported.contains(&feature.as_str()))
        .cloned()
        .collect();
    missing.sort();
    let unreadable: Vec<String> = missing
        .iter()
        .filter(|feature| !Store::read_compatible_features().contains(&feature.as_str()))
        .cloned()
        .collect();
    if !unreadable.is_empty() {
        return Err(MemError::MissingFeatures(unreadable));
    }
    Ok(missing)
}

fn validate_legacy_store(root: &Path) -> MemResult<Vec<String>> {
    let store = Store {
        root: root.to_path_buf(),
        config: Mutex::new(StoreConfig::default()),
        writable: false,
        supported_features: Vec::new(),
    };
    let scan = crate::ops::scan_all(&store)?;
    if !scan.warnings.is_empty() {
        return Err(MemError::MigrationState(format!(
            "记录扫描存在 {} 条告警，需先在旧版本修复后再迁移",
            scan.warnings.len()
        )));
    }
    for rel in crate::event::enumerate_md(&store, "attic")? {
        let path = store.abs(&rel);
        let bytes =
            std::fs::read(&path).map_err(|e| MemError::io(path.display().to_string(), e))?;
        crate::record::Record::from_bytes(&bytes)
            .map_err(|e| MemError::MigrationState(format!("attic 墓碑无法解析（{rel}）：{e}")))?;
    }
    let log = crate::event::EventLog::new(&store).read_log()?;
    let mut seqs = std::collections::HashSet::new();
    let duplicate_seq = log.events.iter().any(|event| !seqs.insert(event.seq));
    let contiguous = log
        .events
        .iter()
        .enumerate()
        .all(|(i, event)| event.seq == i as u64 + 1);
    if log.torn_tail
        || !crate::event::chain_audit(&log.events).is_empty()
        || duplicate_seq
        || !contiguous
    {
        return Err(MemError::MigrationState(
            "事件日志存在损坏或链空洞，需先在旧版本修复后再迁移".into(),
        ));
    }
    let mut migrated_features = Vec::new();
    if log
        .events
        .iter()
        .any(|event| event.op == crate::event::Op::RestoreBody)
    {
        migrated_features.push("event.restore_body/v1".to_owned());
    }
    // 预检必须覆盖 records、attic 与事件历史 ID 的并集。deep_digest_audit 只
    // 遍历可见记录，故在此逐 ID 比较事件最终状态与店内现状，缺记录也会被拒绝。
    let candidates = crate::event::enumerate_candidates(&store, &log)?;
    let mut state_drift = Vec::new();
    for candidate in candidates {
        let current_path =
            crate::event::current_path_for_id(&store, &candidate.lock_rel, &candidate.id)?;
        let current =
            crate::event::current_state_for_id(&store, &candidate.lock_rel, Some(&candidate.id))?;
        let last = log
            .events
            .iter()
            .rev()
            .find(|event| event.id == candidate.id);
        let last_state = last.map(crate::event::triple_from_event);
        let path_drift = match (last, current_path.as_deref()) {
            (Some(event), Some(path)) => event.path != path,
            (Some(event), None) => event.op != crate::event::Op::ExternalDelete,
            (None, _) => true,
        };
        if last_state.as_ref() != Some(&current) || path_drift {
            state_drift.push(candidate.id);
        }
    }
    let drift = crate::ops::deep_digest_audit(&store)?;
    if !drift.is_empty() || !state_drift.is_empty() {
        return Err(MemError::MigrationState(format!(
            "记录与事件状态存在 {} 项待对账差异，需先在旧版本修复",
            drift.len() + state_drift.len()
        )));
    }
    let pending = crate::ops::list_pending_commits(&store)?;
    if !pending.is_empty() {
        return Err(MemError::MigrationState(format!(
            "存在 {} 项 Git 提交待办，需先完成或记录恢复方案",
            pending.len()
        )));
    }
    let plans = crate::agenda::list_plans(&store)?;
    if plans.iter().any(|p| {
        matches!(
            p.status,
            crate::agenda::PlanStatus::Executing | crate::agenda::PlanStatus::Failed
        )
    }) {
        return Err(MemError::MigrationState(
            "存在未收口的维护计划，需先完成恢复或回滚".into(),
        ));
    }
    let agenda_dir = root.join("agenda");
    if agenda_dir.is_dir() {
        for entry in std::fs::read_dir(&agenda_dir)
            .map_err(|e| MemError::io(agenda_dir.display().to_string(), e))?
        {
            let entry = entry.map_err(|e| MemError::io(agenda_dir.display().to_string(), e))?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let bytes =
                std::fs::read(&path).map_err(|e| MemError::io(path.display().to_string(), e))?;
            let plan: crate::agenda::Plan = serde_json::from_slice(&bytes).map_err(|e| {
                MemError::MigrationState(format!("{} 维护计划无法解析: {e}", path.display()))
            })?;
            if path.file_stem().and_then(|s| s.to_str()) != Some(plan.id.as_str()) {
                return Err(MemError::MigrationState(format!(
                    "{} 维护计划身份与文件名不符",
                    path.display()
                )));
            }
        }
    }
    let tmp = root.join(".tmp");
    if tmp.is_dir()
        && std::fs::read_dir(&tmp)
            .map_err(|e| MemError::io(tmp.display().to_string(), e))?
            .next()
            .is_some()
    {
        return Err(MemError::MigrationState(
            ".tmp 含未清理临时产物，需先人工核对".into(),
        ));
    }
    migrated_features.sort();
    migrated_features.dedup();
    Ok(migrated_features)
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct MigrationReport {
    pub status: MigrationStatus,
    pub backup_identity: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MigrationStatus {
    Migrated,
    AlreadyMigrated,
}

/// 绑定到一个 Store 的派生索引维护许可；持有期间其他受管写者被门闩阻塞。
pub struct IndexMaintenancePermit<'a> {
    store: &'a Store,
    barrier: crate::lock::ExclusiveBarrierGuard,
}

impl IndexMaintenancePermit<'_> {
    pub fn store(&self) -> &Store {
        self.store
    }

    /// 校验许可确实绑定到目标店，防止跨店复用独占租约。
    pub fn check_store(&self, store: &Store) -> MemResult<()> {
        if self.store.same_physical_store(store)? {
            store.check_writable()?;
            Ok(())
        } else {
            Err(MemError::BadArgument("索引维护许可属于另一记忆库".into()))
        }
    }

    /// 每次调用前重新校验磁盘能力，避免许可持有期间的配置变化被缓存掩盖。
    pub fn check_compatible(&self) -> MemResult<()> {
        self.store.check_compatible_for_write()
    }

    pub fn is_held(&self) -> bool {
        let _ = &self.barrier;
        true
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
