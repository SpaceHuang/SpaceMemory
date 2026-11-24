# codex/safer-agent-memory-setup 代码评审

- 日期：2026-11-24
- 评审对象：`codex/safer-agent-memory-setup`，HEAD `7913e493027c6758eaff35c2b5a99b3ea3b63ef8`
- 差异基线：`origin/main` `a82ab5ffb37e8eb3adc93fc7720901ae8db59e12`（24 个提交）
- 重点范围：嵌入式下游兼容契约、写许可与事件恢复、索引物化及证明、Windows RDC/命名管道、CLI/MCP 读取路径
- 结论：**存在 2 项阻断，不建议按当前状态合并或发布。**
- 最新重审：HEAD `9f22754` 的 B1/B2 均已关闭，**本轮未发现阻断**。详见文末第四轮重审记录。

## B1 · 排干超时后仍释放挂起 I/O 的缓冲区，重新引入访问违例风险

**位置**：`crates/mem-core/src/platform/pipe_windows.rs:123-164`、`:178-204`、`:214-228`。

`try_serve_one` 的读请求使用栈上 `buf` 和 `ov`，写应答另使用 `reply` 的堆缓冲区。读/写超时后，`abandon_io` 执行以下操作：

1. `CancelIoEx(self.handle, ov)`；
2. 最多等待完成事件 5 秒；
3. 以 `bWait = 0` 调用一次 `GetOverlappedResult`，但忽略返回值和错误；
4. 返回 `recreate()`，关闭句柄并重建，随后本次调用的栈上 `OVERLAPPED` 和读写缓冲区失效。

第 3 步在操作仍未完成时会返回 `FALSE / ERROR_IO_INCOMPLETE`。当前代码没有区分这一结果与已完成/已取消，因此 5 秒超时恰好发生后，内核仍可能访问已经失效的 `OVERLAPPED` 或数据缓冲区。`Drop` 对挂起的 `ConnectNamedPipe` 也采用同样的“有界等待 + 非阻塞读取 + 忽略结果 + 关闭并释放”顺序，连接路径不能例外。

这与 Win32 生命周期要求直接冲突：

- [CancelIoEx](https://learn.microsoft.com/en-us/windows/win32/api/ioapiset/nf-ioapiset-cancelioex) 明确说明取消请求不等待操作完成，并且在操作完成前不得释放或重用关联的 `OVERLAPPED`。
- [GetOverlappedResult](https://learn.microsoft.com/en-us/windows/win32/api/ioapiset/nf-ioapiset-getoverlappedresult) 明确说明 `bWait = FALSE` 且操作仍挂起时返回 `FALSE / ERROR_IO_INCOMPLETE`。

因此，提交 `7b37f2a` 为消除无界等待而加入的超时分支，在异常等待源真正出现时重新打开了本系列修复原本要消除的释放后使用/访问违例窗口。CI 的 30 分钟任务上限只能让挂死可见，不能使该内存生命周期合法。

现有三条管道回归只覆盖正常客户端、正常取消和正常 Drop；没有强制“排干 5 秒后仍为 `ERROR_IO_INCOMPLETE`”的分支，所以全量测试通过不能覆盖此问题。

**修复门槛**：挂起操作完成前，必须继续拥有并保持其 `OVERLAPPED`、事件和读写缓冲区；不能以超时后关闭句柄代替完成确认，也不能让栈帧或 `PipeServer` 先释放这些存储。若要有界返回，需要把未完成操作的所有权转移到能存活到实际完成的结构中，而不是仅缩短等待。补连接、读、写三类故障注入回归，强制覆盖排干超时和迟到完成，验证既不提前释放/重用，也不重新引入无界 serve 挂死。

## B2 · 索引查询和 CLI brief 缺少返回前特性复核，可在新特性登记后照旧输出

**位置**：`crates/mem-index/src/proved.rs:104-126`、`:46-64`，`crates/mem-index/src/sqlite.rs:677-718`，`crates/mem/src/commands/mod.rs:623-638`、`:676-695`。

本分支契约要求防护格式读取在读取前后各检查一次磁盘格式与特性；出现不兼容缺项时拒绝返回，出现只读兼容缺项时改用真相层直读（`docs/develop/embedded-downstream-contract-design.md:107-109`）。当前 `open_proved` 只在构造 `ProvedIndex` 前做后置复核，构造完成后的 `query_by_type` / `query_all` 只执行 SQL，不再读取 `mem.json`。直接公开的 `Index::{query_by_type,query_all,neighbors}` 也没有 `Store` 参数或等效复核。

可用以下确定性调用序列说明公开 API 缺口：

1. 在配置兼容时取得 `ProvedIndex`，或用稳定的 `Index::open_store_readonly` 取得只读索引；
2. 另一进程向磁盘 `required_features` 登记当前构建不认识的特性；
3. 调用既有句柄的 `query_all` / `query_by_type`；
4. 当前实现仍返回索引行，而不是 `MissingFeatures`，也不可能按新缺项降级直读。

CLI `brief` 也存在对应的实际竞态：`open_proved` 和 `scan_ledgers` 之后，无视图快路径直接调用 `idx.query_all(...)`，再把结果标为 `indexed` 输出。若新特性在 `scan_ledgers` 的后置检查之后、`query_all` 之前完成登记，该命令在默认 `since = None` 路径上没有任何返回前复核。`find::execute` 已在最终结果前复核，但这条快路径没有经过它。

这会违反本分支新增的共店撤权语义：旧读者在磁盘已声明其无法解释的特性后，仍可能返回派生索引结果；对未来声明为只读兼容的特性，也无法保证改用真相层。

**修复门槛**：把兼容复核放到实际查询结果的返回边界，不能只放在句柄构造边界；CLI `brief` 输出快路径结果前也必须复核并按结果拒绝或直读。补两类回归：既有只读索引句柄在磁盘特性变化后必须拒绝；用受控交错让特性登记落在 `scan_ledgers` 与 `query_all` 之间，验证 `brief` 不返回 `indexed` 结果。`query_by_type`、`query_all`、`neighbors` 及两前端同类入口应统一覆盖。

## 验证结果

在本评审 HEAD 上实际执行：

- `cargo check --workspace --all-targets`：通过，未报告编译警告；
- `cargo test --workspace`：全部通过，包括 28 项 `embedded_contract_process`、12 项 `index_paths` 及 Windows 管道单元测试；
- `git diff --check origin/main...HEAD`：通过。

这些结果说明现有测试面全绿，但 B1 所需的迟到完成故障注入和 B2 所需的查询后特性竞态均未被现有断言覆盖。评审仅新增本报告，未修改产品代码或测试。

---

# 修复记录（2026-11-24）

两项阻断均已修复并配套回归。修复不改变上文对评审时点（HEAD `7913e49`）的结论记录；以下为后续提交的处置。

## B1 处置（提交 `971b6e2`）

`PipeServer` 重构为**持有式 I/O 槽位**：连接 OVERLAPPED、读写 OVERLAPPED、读缓冲与写应答缓冲全部由句柄拥有（Box/字段固定地址），不再有任何栈局部承载挂起操作。放弃（超时）路径：取消 + 有界排干；确认完成（成功/失败/取消都是终态）→ 槽位复用；仍 `ERROR_IO_INCOMPLETE` → 置未确认标记，下一次进入前再做一次有界收割，仍 INCOMPLETE → **整槽泄漏给进程生命周期**并换新槽——迟到完成写入的永远是有效内存。所有权转移发生在确认未完成之时，不以超时关闭句柄代替完成确认；重建失败路径句柄置空，Drop 按有效性关闭；Drop 对挂起连接与未确认读/写槽执行同样的「有界排干 → 仍 INCOMPLETE 则泄漏」。

回归覆盖：①多客户端跨重建循环（含 `abandoned_leaked == 0` 断言）；②沉默客户端读超时 → 放弃 → 重建 → 新客户端恢复；③写侧放弃——应答超过管道缓冲（4096）且客户端不读使 WriteFile 真实挂起至超时，应答缓冲由句柄持有，重建后恢复；④故障注入 `quiesce_leaks_slot_when_abandoned_io_still_incomplete`：以 `OVERLAPPED.Internal = STATUS_PENDING` 构造未完成态入口，断言整槽泄漏、标记复位、换新槽后服务继续（泄漏分支执行真实 `ptr::read/forget` 代码路径）；⑤挂起连接下 Drop。注：取消完成投递在真实内核中不会确定性迟到，④以私有状态构造分支入口条件。

## B2 处置（提交 `f0a04b2`）

新增 `mem_core::store::read_compatible_missing_features_at(root)`（无句柄形态，与 `Store::read_compatible_missing_features` 单源）；`Index` 依据已持有的 `store_root` 在 `query_by_type` / `query_all` / `neighbors` 的**返回边界**（行已收集、返回之前）重读磁盘配置：不兼容缺项 → `Err(MissingFeatures)`；仅有只读兼容缺项 → 同样拒绝（调用方依既有 `.ok()` 兜底转真相层直读，如 brief/view 快路径）。`brief` 与 view 单类型快路径经 `ProvedIndex` 自动获得该复核；裸索引（无店上下文，仅测试构造）无从复核，跳过并在代码注释声明。

回归覆盖：①`readonly_index_query_rejects_after_disk_feature_registered`——只读句柄存续期间磁盘登记 `index.future/v1`，三类查询逐一断言拒绝（即评审所指「另一进程登记 → 既有句柄查询」的确定性受控序列）；②`brief_never_returns_indexed_results_when_disk_features_incompatible`——端到端：磁盘含未知特性时 brief 拒绝并报完整缺项。本构建 `read_compatible_features = []`，不存在可只读兼容的真实特性样本（与 F4 记录一致），故「只读兼容缺项 → 降级直读」仅由代码路径与调用方兜底结构保证，未伪造特性验收。

## 验证（修复后 HEAD）

- `cargo check --workspace --all-targets`：通过。
- `cargo test --workspace`：全量通过（Windows 开发机，含新增 3 项回归与既有全部测试面）。
- 触及文件 `rustfmt` 与 `git diff --check`：通过。
- CI 双平台复验：run 45420241205 全绿（windows-latest 4m56s / macos-latest 2m51s）。首跑（run 45413814333）Windows job 在 30 分钟上限被终止——取证显示 lib 测试二进制 54/56 项已正常打印完成，未完成两项的代码路径全部等待 ≤5 秒有界；与 run 3 同签名、中间隔同代码全绿（run 5），判读为 runner agent 偶发僵死而非产品代码问题（详见嵌入式契约设计文档日志）。

---

# 重审记录（2026-11-24）

- 评审对象：HEAD `b16214d`，重点复核修复提交 `971b6e2`（B1）与 `f0a04b2`（B2）。
- 结论：**B1 未关闭；B2 的三类直接索引查询已修复，但 `find` 的只读兼容降级仍有缺口。两项仍按阻断处理。**
- 本节只评价修复后的代码；首轮结论与修复提交的自述保留原文，不以修复记录代替复核结论。

## B1 仍未关闭：两条路径都能在旧 I/O 未完成时复用槽位

### 1. 读取返回 `ERROR_IO_INCOMPLETE` 后仍立即复用同一 OVERLAPPED 写应答

位置：`crates/mem-core/src/platform/pipe_windows.rs:193-218`。

`GetOverlappedResult` 对读请求返回 `ERROR_IO_INCOMPLETE` 时，代码只设置 `io_outstanding = true`，没有返回、重建或更换槽位。控制流随后执行 `make_reply()`，并把同一个 `io_ov` 交给 `WriteFile`。因此该标记没有阻止下一次重叠 I/O 复用仍在挂起读操作中的 `OVERLAPPED`；写应答缓冲也成为同一槽位的一部分。

这正是取消纪律禁止的“完成前复用”。该分支虽然属于异常等待源的防御路径，但代码已经显式承认它可发生，就不能在检测到后继续复用。

### 2. 重建后用新管道句柄收割旧 I/O，其他错误还会被当作完成证明

位置：`crates/mem-core/src/platform/pipe_windows.rs:249-280`、`:287-305`、`:327-335`。

实际顺序如下：

1. 旧管道句柄 H 上的读/写超时，`abandon_io` 将旧 `io_ov` 标为未完成；
2. `recreate` 关闭 H 和旧事件，再创建新句柄 H2 并覆盖 `self.handle` / `self.event`；
3. 下一次服务在 `quiesce_io_slot` 中调用 `drain_slot_bounded(self.handle, self.io_ov)`，实际组合是 **H2 + 旧 H 的 OVERLAPPED**；
4. `drain_slot_bounded` 只把 `ERROR_IO_INCOMPLETE` 视为未完成，其他错误一律返回 `true`，随后清除 `io_outstanding` 并复用槽位。

微软对 [GetOverlappedResult](https://learn.microsoft.com/en-us/windows/win32/api/ioapiset/nf-ioapiset-getoverlappedresult) 的要求是：`hFile` 必须为启动该重叠操作时使用的同一句柄。H2 不能提供旧 H 上操作的结果；参数错误或句柄错误也不是旧 I/O 已完成/已取消的证明。当前“除 INCOMPLETE 外均算终态”的判定会把这些错误变成复用许可。Drop 在未确认槽位上同样使用当前句柄，因而不能修复该问题。

新增故障注入 `quiesce_leaks_slot_when_abandoned_io_still_incomplete` 在任何 recreate 之前直接调用 `quiesce_io_slot`，此时 `self.handle` 仍是原句柄，没有覆盖上述真实顺序；现有测试也没有覆盖“读结果 INCOMPLETE 后不得 WriteFile”。所以全绿不能关闭这两项内存生命周期风险。

**修复门槛**：检测到读 I/O 仍未完成时不得继续写应答或复用该槽位。每个未确认操作必须保留能合法取得其完成结果的原管道句柄及完成上下文，直到确认终态；不能用新实例句柄代替。只有确认完成后才可释放或复用 `OVERLAPPED`、事件和缓冲区。回归须覆盖真实的 `abandon → recreate → 迟到完成/再次收割` 顺序，以及读结果 INCOMPLETE 后不发起写入；不能只在 recreate 前手工设置 `OVERLAPPED.Internal`。

## B2 部分修复：`find` 最终复核丢弃只读兼容缺项，仍返回 indexed

位置：`crates/mem-core/src/find.rs:501-531`，`crates/mem-index/src/proved.rs:104-122`、`:129-225`。

已确认修复的部分：`query_all` / `query_by_type` / `neighbors` 已在收集行之后、返回之前重读磁盘配置；CLI brief 的无视图快路径会因查询失败转直读，不兼容缺项最终会被拒绝。新增既有只读句柄回归也验证了三类查询对不兼容缺项的拒绝。

但统一检索入口仍有以下序列：

1. `open_proved` 构造时磁盘兼容，取得索引句柄；
2. 另一进程登记一个**声明为只读兼容**、当前构建不支持的特性；
3. `find::execute` 通过 `candidates` / `bad_records` 完成 indexed 查询；这两个直接 SQL 查询本身没有返回边界复核；
4. 最终复核执行 `store.read_compatible_missing_features()?`，但只传播 `Err`，把表示只读兼容缺项的 `Ok(non_empty)` 直接丢弃；
5. 原 `result` 原样返回，仍可为 indexed。

契约要求“只读兼容但需要降级时走真相层直读并重新核验”（`docs/develop/embedded-downstream-contract-design.md:107-109`），不是允许继续使用索引。当前构建的只读兼容特性表为空，所以现有真实特性样本只能覆盖拒绝分支，不能覆盖此降级分支；但本分支正在实现这项前向兼容契约，空表不能作为保留该结构缺口的理由。

**修复门槛**：`find::execute` 的最终返回边界必须检查缺项列表；若结果使用索引且列表非空，应放弃该结果并重跑直读，直读结果再按既有规则复核。也可让索引预取在返回边界拒绝任何缺项以触发现有直读兜底，但最终复核仍不能忽略 `Ok(non_empty)`。补受控能力表与交错回归，验证登记落在证明构造之后、最终返回之前时 CLI/MCP 统一检索不返回 indexed。

## 重审验证

在 HEAD `b16214d` 上实际执行：

- `cargo check --workspace --all-targets`：通过，未报告编译警告；
- `cargo test --workspace`：全部通过，包括 56 项 mem-core 单元测试、13 项 `index_paths`、14 项 CLI 测试及全部新增回归；
- `git diff --check origin/main...HEAD`：通过。

上述结果仅说明现有测试面全绿。B1 的“读 INCOMPLETE 后继续写”和“recreate 后以新句柄收割旧 I/O”、B2 的只读兼容缺项降级均未被现有回归覆盖。本轮重审未修改产品代码或测试，结论来自所引代码路径与 Win32 API 约束的静态核验。

# 第二轮阻断处置记录（2026-11-24）

## B1 处置（提交 `cb5e442`）

按两项阻断点重写为**暂停-收割模型**：①读收割返回 `ERROR_IO_INCOMPLETE` 时不再落入写应答——请求是否读出不可确认，直接走放弃纪律（取消 + 原句柄有界排干），确认终态才重建，绝不复用该槽位构造应答；②放弃未确认完成时**保留原句柄并暂停服务**（`io_outstanding` 标记 + 下一轮入口对同一仍存活句柄再排干），排干与收割永远对准提交该 I/O 的句柄——重建只发生在确认终态之后，新旧句柄错配不复存在；③终态判定收紧：仅成功或 `ERROR_OPERATION_ABORTED` 算完成证明，其余错误一律按未完成处理（保守走暂停/泄漏终点）；④Drop 终点保留「泄漏整槽连同句柄给进程生命周期」并以进程级计数登记。注释中的所有权纪律一节同步改写。

回归覆盖：①`read_reap_incomplete_never_reuses_slot_for_reply`——按实例注入强制读收割 INCOMPLETE（debug 专用字段，进程级标志曾造成并行串扰已改），断言被放弃实例上无应答写出、新实例恢复应答；②`unconfirmed_io_pauses_service_on_same_handle_until_confirmed`——注入未完成态，断言暂停轮返回 false、句柄不变、人工确认终态后入口排干通过、恢复服务且零泄漏；③`drop_leaks_slot_when_completion_still_pending`——Drop 注入未完成态，断言整槽连同句柄泄漏并计数；④既有行为回归（跨重建循环、沉默读放弃、写侧放弃、挂起连接 Drop）全部保留，测试驱动改写为「边驱动边以 channel 等客户端」（修复了驱动器把放弃重建误当已服务、客户端无超时读挂死测试进程的装置缺陷）。

## B2 处置（提交 `1d59c25`）

`find::execute` 最终复核不再丢弃 `Ok(non-empty)`：只读兼容缺项出现在返回边界时，**整趟改走真相层直读**并返回带缺项标识的直读结果（`FindParams` 加 `Clone` 以支持复核后重执行）。受控能力表：`Store::read_compatible_features()` 增加 **debug 构建专用**覆盖入口 `set_read_compatible_features_for_test`（发布构建恒为声明值，符号不存在），专用测试二进制 `find_read_compatible_fallback.rs`（进程隔离，避免并行串扰）覆盖前向兼容路径：索引句柄存活期间登记只读兼容特性 → 基线 indexed → 登记后同一句柄查询降级直读、reason 含缺项标识、命中与纯直读逐 ID 等价 → 再登记不兼容特性仍结构化拒绝。回归有效性已验证：撤除修复后该测试变红。

## 验证（修复后 HEAD）

- `cargo check --workspace --all-targets`：0 警告 0 错误。
- `cargo test --workspace`：全量通过（Windows 开发机；含新增 4 项回归与既有全部测试面；pipe 测试并行与串行各三轮全绿）。
- `git diff --check`：通过。
- CI 双平台复验：随本次推送执行，结果登记于嵌入式契约设计文档日志。

---

# 第三轮重审记录（2026-11-24）

- 评审对象：HEAD `42b1cb1`，复核修复提交 `cb5e442`（B1）与 `1d59c25`（B2）。
- 结论：**B2 已关闭；B1 的服务中暂停/收割路径已修复，但 Drop 的“泄漏整槽”仍会释放原字段，保留 1 项阻断。**

## 已确认修复

B1 的两项服务中路径已按上轮门槛修改：入口在未确认 I/O 上保留并使用原句柄排干，未确认则直接返回，不重建、不换槽（`crates/mem-core/src/platform/pipe_windows.rs:162-175`）；读收割为 `ERROR_IO_INCOMPLETE` 时转入放弃流程，不再落入 `WriteFile`（`pipe_windows.rs:216-255`）。终态判定也不再接受任意错误作为完成证明。

B2 的最终复核现在显式处理 `Ok(non_empty)`：发现只读兼容缺项后以带缺项标识的 Direct 模式重跑真相层查询，不再返回 indexed 结果（`crates/mem-core/src/find.rs:531-546`）。新增独立测试二进制通过 debug 专用能力表构造只读兼容特性，验证既有索引句柄降级、结果与直读等价及不兼容特性拒绝；本轮实际运行通过。

## B1 剩余阻断：`ptr::read + forget` 没有阻止原 Box/Vec 字段析构

位置：`crates/mem-core/src/platform/pipe_windows.rs:318-360`，测试见 `pipe_windows.rs:620-639`。

Drop 在未确认连接或读/写完成时采用以下形式：

```rust
let io_ov = std::ptr::read(&self.io_ov);
std::mem::forget(io_ov);
```

[std::ptr::read](https://doc.rust-lang.org/std/ptr/fn.read.html) 的语义是读取一个按位副本且**保持源位置不变**，不是从 `self` 移出字段。[std::mem::forget](https://doc.rust-lang.org/std/mem/fn.forget.html) 只遗忘作为参数传入的那个副本，不会遗忘仍留在 `self.connect_ov`、`self.io_ov`、`self.io_buf`、`self.io_reply` 中的原所有者。

`PipeServer` 实现了 `Drop`，但 Rust 会在其 `Drop::drop` 方法返回后继续自动析构各字段。因此当前顺序实际是：

1. 复制 Box/Vec 的所有权位形并遗忘副本；
2. `leaked_slot = true` 只跳过 `CloseHandle`，并不禁止字段析构；
3. `Drop::drop` 返回，原 Box/Vec 字段自动析构并释放 `OVERLAPPED` 与读写缓冲区；
4. 尚未完成的 `ConnectNamedPipe` / `ReadFile` / `WriteFile` 仍可能访问这些已释放地址。

所以这里不是“整槽连同句柄泄漏”，而是“句柄泄漏、内存仍释放”，原 B1 的释放后使用风险在 Drop 终点依旧存在。`drop_leaks_slot_when_completion_still_pending` 只断言自定义原子计数加一；该计数在字段真正析构之前更新，且注入的伪 `OVERLAPPED` 没有真实迟到写入，因此测试全绿不能证明存储仍存活。

**修复门槛**：真正移出并遗忘原所有者，或用 `ManuallyDrop` 等结构禁止原字段自动析构；若用替换/取出方式，必须确保留在 `self` 中、随后自动析构的是不再拥有这些分配的替代值。连接与读/写槽均须覆盖，并继续保持句柄和事件有效。回归应直接验证原所有者没有析构，而不是只检查手工计数；同时保留未确认完成时有界返回及确认完成后正常释放的行为。

## 第三轮验证

在 HEAD `42b1cb1` 上实际执行：

- `cargo check --workspace --all-targets`：通过，未报告编译警告；
- `cargo test --workspace`：全部通过，包括 58 项 mem-core 单元测试及新增 `find_read_compatible_fallback`；
- `git diff --check origin/main...HEAD`：通过。

本轮未修改产品代码或测试。剩余阻断由 Rust 所有权/析构语义与所引 Drop 实现直接确定；现有全绿结果未覆盖原字段是否实际泄漏。

---

# 第四轮重审记录（2026-11-24）

- 评审对象：HEAD `9f22754`，复核第三轮 B1 修复提交 `9f22754`。
- 结论：**B1/B2 均已关闭，本轮未发现阻断。**

## B1 关闭确认

`connect_ov`、`io_ov`、`io_buf`、`io_reply` 已改为 `ManuallyDrop` 字段（`crates/mem-core/src/platform/pipe_windows.rs:62-88`）。Drop 开始即通过 `ManuallyDrop::take` 将四份原所有者从 `self` 移出，之后由局部所有者显式处置（`pipe_windows.rs:330-343`）：

- 连接或读/写操作已确认终态：对应 Box/Vec 被显式 `drop`，正常释放；
- 操作仍未确认完成：已移出的 Box/Vec 被 `forget`，不再存在会于 `Drop::drop` 返回后自动释放同一分配的原字段；
- 任一组槽位泄漏时，管道句柄与事件也保持有效，不执行关闭；其他已终态槽位仍正常释放。

因此第三轮“只遗忘副本、原字段仍析构”的问题已消除，连接与读/写路径均满足每份存储恰好显式处置一次。既有暂停/原句柄收割、读 INCOMPLETE 不写应答、正常重建及 B2 降级代码未出现回退。增强后的 Drop 回归还会在 drop 后读取事前保存的 OVERLAPPED 地址，本轮实际运行通过。

## 第四轮验证

在 HEAD `9f22754` 上实际执行：

- `cargo check --workspace --all-targets`：通过，未报告编译警告；
- `cargo test --workspace`：全部通过，包括 58 项 mem-core 单元测试及既有 B1/B2 回归；
- `cargo check --workspace --release`：通过；
- `git diff --check origin/main...HEAD`：通过。

验证环境为本机 Windows。迟到完成仍由故障注入构造，未另行制造真实内核确定性迟到；关闭结论同时基于 `ManuallyDrop` 的所有权与析构语义核验。本轮未修改产品代码或测试，仅更新评审记录。

# 第四轮重审确认（2026-11-24，评审记录更新）

重审结论：**B1/B2 均已关闭，未发现阻断。** B1 确认要点：槽位字段 `ManuallyDrop` 化，`Drop` 入口 `take` 移出原所有者；确认终态的槽位显式释放，未确认的槽位真正泄漏且原字段不会再次自动析构；泄漏时句柄与事件保持有效；正常路径、暂停收割、读 INCOMPLETE 分支均无回退；B2 只读兼容降级直读保持有效。验证：`cargo check --workspace --all-targets`、`cargo test --workspace`、`cargo check --workspace --release`、`git diff --check` 全部通过。

CI 复验矩阵（提交 `9f22754`，含第三轮修复）：最终 run 45510914421 **双平台全绿**（windows-latest 3m6s / macos-latest 3m9s，含 release 消费者示例检查——cfg 影子绑定修复生效）。此前两次 macos-latest 因「hosted runner 容量不足未获取」失败（job 未运行，GitHub 容量告警注记在案，属基础设施排队而非产品问题）；其间 windows-latest 单平台已先行全绿（run 45436571297，3m35s）。
