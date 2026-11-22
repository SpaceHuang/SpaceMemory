# 嵌入式下游 Rust API 契约（候选）

本清单只列下游可依赖的第一批入口；其余公开 crate 项均视为内部实现细节，升级时可能变化。首个稳定契约候选版本定为 `1.0.0`；当前工作树按此版本构建，但尚未发布稳定契约 tag。

以下签名按当前候选源码逐字核对。它们用于消费者编译和升级审查；在实际稳定 tag 发布前均属候选，签名或字段变更须同时更新本表、编译示例及行为回归。`MemResult<T>` 是 `Result<T, MemError>`。

## 候选 Rust 签名

```rust
Store::open(root: &Path, mode: OpenMode) -> MemResult<Store>
Store::supported_features() -> &'static [&'static str]
Store::read_compatible_features() -> &'static [&'static str]
Store::capability_manifest() -> serde_json::Value
Store::read_compatible_missing_features(&self) -> MemResult<Vec<String>>
Store::register_required_feature(&self, feature: &str) -> MemResult<()>
Store::register_required_features(&self, features: &[&str]) -> MemResult<()>
Store::migrate_format_v1(root: &Path, backup_identity: &str)
    -> MemResult<MigrationReport>
Store::create(root: &Path) -> MemResult<()>
WriteContext::new<'a>(
    store: &'a Store, types: &'a TypeSet, by: impl Into<String>,
) -> WriteContext<'a>
WriteContext::with_committer(self, committer: Option<&'a dyn CommitSink>) -> Self
WriteContext::with_session_id(self, session_id: Option<&'a str>) -> Self

ops::get(store: &Store, id_or_path: &str) -> MemResult<Scanned>
ops::scan_all(store: &Store) -> MemResult<ScanResult>
ops::add(ctx: &WriteContext<'_>, args: AddArgs) -> MemResult<Added>
ops::update(
    ctx: &WriteContext<'_>, id_or_path: &str, expect: &Token,
    payload: serde_json::Value, body: Option<Vec<u8>>,
) -> MemResult<Updated>
ops::link(
    ctx: &WriteContext<'_>, a_id: &str, b_id: &str, rel: &str,
    expect_a: &Token, expect_b: &Token,
) -> MemResult<(Token, Token)>
ops::delete_record(
    ctx: &WriteContext<'_>, id_or_path: &str, expect: &Token, force: bool,
) -> MemResult<Deleted>
ops::restore_record(
    ctx: &WriteContext<'_>, id_or_path: &str, expect: Option<&Token>,
) -> MemResult<Restored>
ops::restore_body(
    ctx: &WriteContext<'_>, id_or_path: &str, expect: &Token,
    source: &RestoreSource, history: Option<&dyn BodyHistory>,
) -> MemResult<RestoredBody>
EventLog::read_log(&self) -> MemResult<ReadLog>

Store::acquire_init_lock(root: &Path) -> MemResult<InitPermit>
Store::create_under_init_lock(root: &Path, permit: &InitPermit) -> MemResult<()>
Store::acquire_index_maintenance(&self) -> MemResult<IndexMaintenancePermit<'_>>
Index::open_store(store: &Store) -> MemResult<Index>
Index::open_store_under_maintenance(
    store: &Store, permit: &IndexMaintenancePermit<'_>,
) -> MemResult<Index>
Index::open_store_readonly(store: &Store) -> MemResult<Index>
```

输入字段也是契约的一部分：`AddArgs` 当前包含 `type_name: String`、`payload: Value`、`body: Vec<u8>`、`principal: Option<String>`；这些字段在 1.0 版本固定，增删或改型属于破坏性 API 变化，须升 MAJOR。`OpenMode` 的 `Auto` 与 `ForceReadOnly` 变体同样属于 1.0 契约，新增匹配变体按 MAJOR 处理。消费者编译示例构造 `AddArgs` 并核对公开签名。`WriteContext` 使用 `new` 创建，可用 builder 设置可选 Git `CommitSink` 和 session id；字段可读取，结构为 `non_exhaustive`，外部不能用结构体字面量构造。测试暂停钩子不属于正式写上下文，只能从 debug 集成测试辅助入口注入。结果 `Scanned` 提供 `rel/record/token`；创建结果 `Added` 提供这些字段及 `created/warnings`；更新 `Updated` 提供 `rel/record/token/warnings`；删除 `Deleted` 与恢复 `Restored` 提供新路径和令牌；正文恢复结果 `RestoredBody` 提供 `rel/record/token/source_echo`。这些输出结构均为 `non_exhaustive`，可读取列出的字段，但消费者须在解构时使用 `..`，不得直接用结构体字面量构造。`RestoreSource` 也是 `non_exhaustive`，消费者匹配必须保留通配分支，未来可通过兼容版本增加来源形态。`Token` 可用 `Display`/`FromStr` 表示为 `v<版本>@<完整64位SHA-256>`。`MemError` 和 `MigrationStatus` 是 `non_exhaustive`，下游匹配必须保留通配分支；当前承诺冲突错误的 `expected/current` 及兼容拒绝的完整特性 ID。

正文恢复使用 `RestoreSource::{Git, File, History}`，分别指定 Git 修订、店外备份文件或受限的历史自动锚定；这是 `non_exhaustive` 输入枚举，匹配时必须保留通配分支。`BodyHistory` 是只读来源接口。成功结果 `RestoredBody` 提供新记录、令牌和来源回显，属于 `non_exhaustive` 输出结构。执行时先声明 `event.restore_body/v1`，再改正文并追加事件；迁移历史店时也会从既有事件推导该标记。该特性被目录声明后，不支持它的构建不得打开为可写；因本候选未声明该特性的只读兼容性，缺少特性的构建也不能将此店按只读打开。

| 用途 | 稳定候选入口 | 保证与边界 |
|---|---|---|
| 打开数据目录 | `mem_core::store::{Store, OpenMode}` | `Auto` 探测自然可写性；`ForceReadOnly` 不作可写探测。未知格式在探测前失败。 |
| 机读能力 | `Store::capability_manifest()` | 返回基础格式、只读格式、支持特性和读兼容特性的 JSON。当前支持 `event.restore_body/v1`；只有实际执行正文恢复的店才要求此能力。 |
| 读取兼容性复核 | `Store::read_compatible_missing_features()` | 每次读磁盘上的格式和必需特性；不兼容缺项返回完整 `MissingFeatures`，只读兼容缺项作为返回值交给调用方降级到真相层直读。 |
| 登记店级写能力 | `Store::{register_required_feature, register_required_features}` | 只允许登记当前构建声明支持的机制特性；一次性在独占门闩中合并到店配置。成功返回后才可写入依赖数据；空集合是无副作用 no-op。批量请求含不支持能力时，以排序后的完整 `MissingFeatures` 列表拒绝且不改配置。正文恢复入口在同一独占许可中先登记 `event.restore_body/v1`，再写记录和事件。跨记录的“登记+依赖写”组合仍由具体机制入口负责在同一许可内完成，消费者不可将两个独立操作当作原子事务。 |
| 单记录查询 | `mem_core::ops::{get, scan_all}` | 读真相层；查询结果不授权后续写入。 |
| 单记录写 | `mem_core::ops::{add, update, delete_record, restore_record, link}` | 写前在跨进程门闩内重新校验磁盘兼容状态；CAS 使用完整版本与摘要令牌；错误通过 `MemResult` 返回。 |
| 正文恢复 | `mem_core::ops::restore_body` | 先在独占门闩内登记 `event.restore_body/v1`，再替换正文并追加相应事件；历史店迁移会按既有事件补齐此特性。正文恢复是可选能力，未执行时不改变店的特性集合。 |
| CAS 令牌 | `mem_core::token::Token` | 使用完整摘要；冲突 `MemError::Conflict` 携带当前令牌。 |
| 初始化租约 | `Store::{acquire_init_lock, create_under_init_lock}` 与 `InitPermit` | 许可不可伪造且绑定目标路径；CLI 在播种和自举前持有同一许可。`create` 是单步封装。 |
| 维护许可与索引 | `Store::acquire_index_maintenance`、`Index::{open_store, open_store_under_maintenance}` | 店索引创建须先验证磁盘兼容性并持有独占维护许可；已持许可路径传递同一许可，避免锁重入。只读打开使用 `open_store_readonly`。 |
| 事件查询 | `mem_core::event::EventLog::read_log` | 只读历史读取；修复/回填通过受管写入口执行。 |
| MCP 工具 | `mem_mcp::tools::tool_definitions()` | 运行时工具表是唯一来源，随候选提交固定。 |

| 已验证工具名 | 用途 |
|---|---|
| `mem_brief`, `mem_session_start`, `mem_session_flush` | 工作视图与会话 |
| `mem_list`, `mem_find`, `mem_show`, `mem_diff`, `mem_trace` | 只读发现、读取与引用/证据追踪 |
| `mem_add`, `mem_update`, `mem_link`, `mem_delete`, `mem_restore`, `mem_restore_body` | 记录写入与恢复 |
| `mem_reconcile`, `mem_verify` | 显式维护与核验 |

工具参数和描述以对应提交 `mem_mcp::tools::tool_definitions()` 的实际返回值为准；本表只给名字和用途类别。`*_locked`、索引内部物化 helper、平台锁原语和未列入上表的公开符号暂不承诺稳定。列出的输出结构可读取字段已列明，并通过 `non_exhaustive` 保留向后兼容的新增空间。若消费者直接调用未列入的符号，应固定产品提交并承担升级适配。实际发布时仍须由编译示例、API diff 和行为验收冻结具体承诺。

## 消费者示例

仓库内 `crates/mem-core/examples/embedded_contract.rs` 可通过 `cargo run -p mem-core --example embedded_contract -- <store-path>` 编译运行；它以只读模式打开指定店并读取机读能力清单。文件中的 `candidate_signatures_compile` 以函数指针赋值在编译期核对打开、兼容检查、查询、单记录写和事件读取的候选签名，不执行写操作。
