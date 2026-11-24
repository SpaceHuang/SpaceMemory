# codex/safer-agent-memory-setup 代码评审

- 日期：2026-11-24
- 评审对象：`codex/safer-agent-memory-setup`，HEAD `7913e493027c6758eaff35c2b5a99b3ea3b63ef8`
- 差异基线：`origin/main` `a82ab5ffb37e8eb3adc93fc7720901ae8db59e12`（24 个提交）
- 重点范围：嵌入式下游兼容契约、写许可与事件恢复、索引物化及证明、Windows RDC/命名管道、CLI/MCP 读取路径
- 结论：**存在 2 项阻断，不建议按当前状态合并或发布。**

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
- CI 双平台复验：随本次推送执行，结果登记于嵌入式契约设计文档日志。
