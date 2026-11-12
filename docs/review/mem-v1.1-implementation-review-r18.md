# 第十八轮实施复审——R17 路径边界修复验证

- 日期：2026-11-12。
- 对象：`main`，HEAD `2cc5ac2` 加当前未提交修复；开始复审时 24 个已跟踪文件发生修改。
- 环境：macOS / Darwin arm64。
- 范围：R17 B17-1 的记录目标、移动目标及锁文件边界，新增公共校验的正常路径兼容性，以及直接相关的索引资源调用完整性。先审新增测试，再审实现与运行探针。

## 结论与待批准清单

**R17 的 P1 阻断 B17-1 可关闭；本轮另确认 1 项 P2 / Required，整体结论仍为 Request changes。**

四类原反例均已被正确拒绝：add 的 records 外链、delete 的 attic 外链、restore 的 records 外链、update 的锁文件外链。源记录、店外文件与事件日志保持不变；merge 目的地、门闩锁、序列锁及会话锁的相邻外链场景也取得正确拒绝证据。正常路径通过。

新的 P2 是同一资源边界在缓存层的调用遗漏：店索引打开入口直接打开数据库及 snapshot 锁，未接入新增校验，reconcile 仍可经链接创建店外文件或改变店外 SQLite 的 schema。该项与原记录面 P1 分开登记，见 Q18-1。

**本轮待确认范围**：关闭 B17-1；处理 Q18-1 的店索引资源边界后复验。

**明确不包含**：发布/合并操作、改写 R15–R17、认定 Windows 重解析点实测完成、认定 FSEvents 计划已实施。

## B17-1 逐项关闭证据

| 场景 | 本轮实际结果 | 保护结果 |
|---|---|---|
| add：records 子目录指向店外 | CLI 退出 1，BadArgument | 店外无记录、无锁文件；events 不变 |
| delete：attic 子目录指向店外 | CLI 退出 1，BadArgument | 原 records 字节保留；店外、events 不变 |
| restore：records 子目录指向店外 | CLI 退出 1，BadArgument | 原 attic 字节保留；店外、events 不变 |
| update：记录 `.lock` 指向店外普通文本 | CLI 退出 1，BadArgument | 店外哨兵、原记录、events 字节均不变 |
| merge：attic 目的目录外链 | CLI 退出 1，BadArgument | dup、canonical、events 不变；店外目录为空 |
| 记录锁：现存目标外链、悬空外链、父目录外链 | 阻塞及 try 入口均拒绝 | 不覆写哨兵、不创建店外目标 |
| 根机制锁：barrier/events 外链 | 门闩各入口及序列锁拒绝 | 店外哨兵不变 |
| 会话 `.json.lock` 外链 | 拒绝 | 店外哨兵不变 |

CLI 使用当前 `target/debug/mem`，临时店与店外目录均位于系统临时目录；核心探针直接依赖当前工作区，没有旧副本或兼容补丁。

实现核对：

- `paths::validate_store_record_path` / `validate_store_relative_path` 统一执行词法与逐组件链接检查，再核对最近存在祖先的实际位置。
- add、delete、restore、merge 在目标锁、建目录、发布或移动之前检查生成的记录路径。
- `record_lock` / `try_record_lock` 在创建父目录与覆写 owner 前同时检查记录和 `.lock` 路径；barrier、events、serve 的固定锁入口也接入校验。
- fix 保留路径拒绝错误，继续经公共校验；ID/别名扫描的链接隔离规则保持。

正常路径独立探针通过：绝对店根、相对店根、用户明确选择的店根链接别名；记录多级父目录缺失；attic 根缺失后的 delete；records 根缺失后的 restore；正常 add/update/delete/restore；新会话目录锁及 start/trail/flush/close；门闩 → 记录锁 → 序列锁顺序。

## Q18-1 [P2 / Required] 店索引及 snapshot 锁应接入同一资源边界校验

**位置**：`crates/mem-index/src/sqlite.rs:127-138,145-160,235-241`。

`Index::open_store` 直接把 `store.abs("index/index.sqlite")` 传给通用 `open(Path)`。后者先创建目录、打开 `index.sqlite.lock`，再打开 SQLite。数据库路径、其父目录与 snapshot 锁均未调用新增的 store 资源校验。只读的店级包装入口也未核对链接边界。

本轮用真实 CLI reconcile 复现三种预先存在的静态链接，全部退出 0、`index_rebuilt=true`、`warnings=[]`：

| 设置 | 店外实际变化 |
|---|---|
| `index/index.sqlite.lock` 链接到店外不存在的文件 | 店外创建长度 0 的锁目标文件 |
| `index/` 链接到店外空目录 | 店外创建 `index.sqlite`、`index.sqlite.lock` |
| `index/index.sqlite` 链接到已有店外 SQLite | 店外数据库字节改变，新增 `records`、`refs`、`meta`、`bad_records` 表 |

第三种探针的店外数据库初始只有 `user_data(value)` 表，内有字符串 `valuable`。reconcile 后原表与该值仍保留，但该数据库被额外初始化成了 mem 索引，确认存在越界数据库修改。前两种情形也经独立真实库探针复现。

这是一项缓存资源调用遗漏；分级依据是已证实的店外文件创建与 SQLite 初始化副作用。本轮没有把它与已经修好的记录删除/恢复丢失混为同一 P1。

**修复方向**：在掌握 Store 边界的 `open_store` 与 `open_store_readonly` 入口，统一核对数据库及 snapshot 锁的资源路径，再进入通用的 `open(Path)` / `open_readonly(Path)`。写入口必须在 mkdir、锁文件打开和 SQLite 打开之前拒绝不合法落点；读入口验证失败沿现有逻辑回退 Direct。通用的任意 Path API 可以继续供独立数据库调用方使用，不必凭空推断店根。

**关闭条件**：

1. 上述锁文件、目录、数据库文件三种外链全部在副作用前拒绝；店外文件集合与字节保持不变。
2. 店级只读打开同样拒绝外链资源，查询通过现有回退路径仍可读取真相层。
3. 正常索引首次建立、维护重建、缺失父目录、只读快照锁互斥与零写盘回归继续通过。
4. 相同规则用于 CLI/MCP reconcile 与 sidecar 的公共店级入口，避免各前端单独修补。

下钻：R17 对实际资源落点的约束；需求 R2/R6；设计 D1 的本地存储边界。

## 验证记录

1. `cargo check --workspace --all-targets`：通过，零警告。
2. `cargo test --workspace --no-fail-fast`：退出 0，**297 项通过 / 0 失败 / 0 ignored**，较 R17 增加 4 项外链回归；Windows 条件编译专属用例不计入本机通过数。
3. 先读并实跑新增的四项 `r17_` 操作回归；独立 CLI 重放这四类加 merge 目的地外链，均核对源记录、店外字节和事件日志。
4. 独立锁探针覆盖阻塞/非阻塞记录锁、barrier 各入口、events 锁、会话锁及正常锁序。`locks` 12 项与 `session_view` 13 项通过。
5. 正常路径探针覆盖缺失父目录和相对店根；只读索引原回归 `readonly_open_never_creates_and_epoch_advances` 通过。Q18-1 来自另外的真实 CLI/库探针，其三个静态场景未被现有索引测试覆盖。
6. `git diff --check`：通过。本轮仅新增本报告，未修改用户源码、测试、旧评审或设计版本，未 commit、未 push。

## 自曝三处

1. **平台边界**：原 B17-1 由 macOS 实测关闭；Windows junction/重解析点尚无本轮实机证据。如果 Windows 不能拒绝对应输入，需要补充平台结论，不能由 297 项本机通过推定。
2. **链接时序边界**：本轮验证预先存在的静态链接；未验证恶意并发换链。B17-1 原报告同样限定此范围，关闭不额外承诺抵御任意文件系统攻击。
3. **严重性边界**：Q18-1 已确认店外创建和 SQLite schema 修改，原用户表值保留；若所有店级索引资源在打开前均经公共校验、三种外链不再产生副作用，即可关闭该项，不以未知风险维持阻断。
