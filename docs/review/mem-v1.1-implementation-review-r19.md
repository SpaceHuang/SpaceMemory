# 第十九轮实施复审——Q18-1 索引资源边界修复

- 日期：2026-11-12。
- 对象：`main`，最终代码快照 `084efd9`（`fix: harden store paths and platform tests`）。复验开始时为 `2cc5ac2` 加未提交修复，收尾时这些修复已提交；当前已跟踪代码无额外工作区差异。包含新增的 `crates/mem-index/tests/index_paths.rs`。
- 环境：macOS / Darwin arm64。
- 范围：R18 唯一未关闭项 Q18-1，以及索引正常建立、只读回退和共同调用入口。本轮是定向复审。

## 结论与待批准清单

**Approve：Q18-1 可关闭。本轮未发现新的阻断或 Required 项。**

店级索引的数据库与 snapshot 锁已在任何创建或打开操作之前，统一经过公共路径校验。三类原始外链在 CLI、MCP 和 sidecar 下均不再产生店外副作用；只读入口验证失败后正确回退直读，仍可查到真相层记录。正常索引维护与读取通过。

**本轮确认范围**：关闭 R18 的 P2 / Required 项 Q18-1；本轮定向代码复审通过。

**明确不包含**：执行发布/合并、改写旧报告、Windows 重解析点实机验收、FSEvents 计划实施验收，以及重新逐行评审全库。

## 修复实现核对

- `crates/mem-index/src/sqlite.rs:235-243`：`open_store` 与 `open_store_readonly` 都先调用同一个 `validate_store_index_paths`。
- `crates/mem-index/src/sqlite.rs:599-602`：对 `index/index.sqlite` 与 `index/index.sqlite.lock` 分别使用 `validate_store_relative_path`，覆盖数据库文件、父目录和 snapshot 锁的静态链接。
- 检查成功后才进入通用 `open(Path)` / `open_readonly(Path)`，因此拒绝发生在 mkdir、锁文件打开、SQLite 初始化之前。显式独立数据库调用方的通用 Path API 保留原用途。
- `open_proved` 的既有错误处理将店级只读打开失败转换成带原因的 Direct，不使索引失效阻止真相层查询。
- CLI reconcile、MCP mem_reconcile 与 sidecar 物化器均调用店级入口，规则不在各前端重复实现。

该修复保持 Store 边界位于掌握 Store 的包装层，复用已有校验，未新增依赖或另一套路径判断。

## 原始反例：三类外链 × 三个前端

使用本轮 `target/debug/mem`，在每个全新临时店中分别设置原 R18 场景，再调用 CLI reconcile、MCP mem_reconcile 或启动 sidecar，共 **9 个场景**。

| 外链设置 | CLI reconcile | MCP mem_reconcile | sidecar | 店外保护结果 |
|---|---|---|---|---|
| `index.sqlite.lock` 指向店外不存在文件 | 退出 0，`index_rebuilt=false` | 结构化路径错误 | 退出 1，路径错误 | 未创建店外目标 |
| `index/` 指向店外空目录 | 退出 0，`index_rebuilt=false` | 结构化路径错误 | 退出 1，路径错误 | 店外目录仍为空 |
| `index.sqlite` 指向店外 SQLite 文件 | 退出 0，`index_rebuilt=false` | 结构化路径错误 | 退出 1，路径错误 | 数据库字节与 schema 不变 |

CLI 保留了回填与派生索引维护分开记账的既有行为：索引拒绝后如实标记 `index_rebuilt=false`。MCP 工具错误在 JSON-RPC 应答中返回，stdio 进程本身退出 0；该进程退出码没有被当作工具成功。

数据库探针仍使用含 `user_data(value)` 和值 `valuable` 的店外文件；本轮调用后文件字节不变、原表与值保留，且没有新增 `records`、`refs`、`meta`、`bad_records` 表。所有场景均比较了店外文件集合与全部字节。

## 只读回退与正常路径对照

独立库探针使用本轮当前编译产物。每个外链场景先建立有效索引和检查点，显式确认 `open_proved` 为 **Indexed / Some**，再把资源替换为外链，排除“从未具备索引证明而提前转直读”的假覆盖。

| 场景 | 实际结果 |
|---|---|
| snapshot 锁为指向既有文件的外链 | 店级只读拒绝；`open_proved` 明确以符号链接原因为 Direct / None |
| snapshot 锁为悬空外链 | 同上；未创建店外目标 |
| index 目录外链 | 同上；店外文件集合及字节不变 |
| 数据库文件外链 | 同上；店外文件集合及字节不变 |
| 上述回退后执行真实 find 执行器 | 每个场景仍命中真相层中的目标记录 |
| 绝对及相对店根 | 正常建立、只读打开与 Indexed 查询成功 |
| index 父目录完全缺失 | 只读入口不创建目录；维护入口正常建立，之后可只读及 Indexed 查询 |
| 正常只读快照 | 前后文件集合与字节不变；既有快照锁互斥回归通过 |

## 验证记录

1. 先审新增 `index_paths.rs` 四项行为测试，再核对共享校验实现；独立探针直接依赖或链接当前工作区，无旧源码副本、无兼容补丁。
2. `cargo check --workspace --all-targets`：通过，零警告。
3. `cargo test --workspace --no-fail-fast`：退出 0，**301 项通过 / 0 失败 / 0 ignored**，较 R18 增加 4 项测试。Windows 条件编译专属用例不计入本机通过数。
4. 定向 `cargo test -p mem-index --test index_paths --test index`：8 项通过，0 失败；包含新增路径测试和既有只读快照回归。它们属于全量测试的子集，不重复计入 301。
5. `git diff --check`：通过。本轮仅新增本报告，未修改实现、测试、旧报告或设计版本；未 commit、未 push。

## 自曝三处

1. **范围边界**：本轮复审 Q18-1 及直接相关索引路径，没有重新逐行检查全库；此前关闭项由当前全量回归承接。如果另有具体失败证据，应单独登记，不从本轮通过推定所有未知场景安全。
2. **平台与时序边界**：静态符号链接场景在 macOS 验证；Windows 重解析点实机、恶意并发换链未验证。若这些外部验收失败，应补充结论；本轮不据此维持已关闭的静态外链问题。
3. **证明边界**：独立只读探针用合法 Hello 模拟订阅证据，并先确认 Indexed，再验证明确路径拒绝导致的 Direct；未声称本机建立了实际 FSEvents/RDC 订阅。真实订阅链路仍属于对应平台验收范围。
