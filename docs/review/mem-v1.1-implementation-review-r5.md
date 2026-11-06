# mem v1.1 实施评审——第五轮报告（R5，R4 修复重审）

- 评审对象：R4 所列 7 项阻断、2 项高优修复后的当前工作区
- 评审日期：2026-11-05
- 评审环境：Windows 11，非提权用户进程；生产快路径实测后端为 RDC
- 评审方式：逐项读码、完整构建测试、生产 CLI 对抗复现、维护原语确定性竞态注入
- 基线结果：`cargo check --workspace --all-targets` 零 Rust 警告；`cargo test --workspace` 的 **29 个测试套件、157 项测试全部通过**

## 评审结论（TL;DR）

**仍有 4 项发布阻断：B1、B3、B4、B7；本轮不批准进入发布准备。**

B2、B5、B6 及 H1、H2 通过本轮复核。B1 的普通过滤与空类型崩溃已经修复，但新增的“快路径与直读不一致”防御分支直接报错，没有执行注释和修复登记所称的直读兜底。B3 的重建后注入场景已修复，但最后订阅查询之后仍存在未覆盖窗口。B4 的事件链与原断链已修复，但 x-ref 的 payload 没有同步收敛。B7 的 FTS offset 已修复，但快/直读命中语义不等价，trace 的续读建议在预算上限处永久重复第一页。

因此，“157 项全绿”只能证明新增用例通过，不能证明 R4 验收面已经闭环。

## 逐项结论

| R4 项 | R5 结论 | 证据摘要 |
|---|---|---|
| B1 视图过滤/panic | **未通过：仍有阻断** | 复合视图只返回目标记录，空类型视图退出码 0；但索引行不一致时 `mem view` 退出码 1，而非直读返回真相 |
| B2 删除索引后自建空库 | 通过 | 读路径使用只读打开并核对 `index_epoch`；生产 e2e 覆盖删除索引后完整直读，现有测试通过 |
| B3 维护竞态提前确认 | **未通过：仍有阻断** | 在“最后 fresh 查询之后、检查点快照之前”注入保 stat 写入，原语仍返回 `Some(Checkpoint)`，索引摘要与磁盘摘要不同 |
| B4 merge 断链/事件空洞 | **未通过：仍有阻断** | 原 `verify` 断链和事件链空洞已消失；但内置 `memory.topic` 复现 payload 仍指向 attic dup，而 envelope 已指向 canonical |
| B5 create 崩溃原子性 | 通过（有验证边界） | Windows/macOS 均改为同目录 temp 写完并同步后原子独占发布；Windows 不覆盖既有目标，macOS 使用 `link(2)`；未做真实断电及 macOS 真机验收 |
| B6 USN FRN 覆盖失效 | 通过（保守降级） | 生产 sidecar 已停用 USN，仅启用 RDC；不可订阅时直读，不再让已知失覆盖的 USN 分支签发新鲜性证明 |
| B7 预算/续读/FTS 分页 | **未通过：仍有阻断** | FTS 两页不重复已有回归；但完整 ID 查询直读 1 条、indexed 0 条，trace 按返回建议续读仍返回同一节点 |
| H1 路径 get 双读 | 通过 | 路径分支一次读取同一字节缓冲并同时派生 Record 与 Token |
| H2 reconcile 不刷索引 | 通过（正确性） | CLI/MCP 均重建索引且不再自行发布无订阅证据的检查点；epoch 失配会使读路径直读 |

---

## B1 残留：索引不一致直接使 `mem view` 失败，直读兜底没有实现

**位置**：`crates/mem/src/commands/mod.rs:145-177`，直接报错点为 `:152-156`。

**代码事实**：快路径和直读 ID 不一致时，代码返回 `MemError::Corrupt`。错误文本和注释都写“转直读”，但没有进入 `None` 分支，也没有输出 `direct.items`。这使一个可丢弃、可损坏的派生索引能够阻断合法视图读取。

**生产复现**：

1. 建立一条 memory 和 `types=["memory"]` 的视图，启动 RDC sidecar，确认 `mem list --type memory` 为 `indexed`；
2. 只修改 `index.sqlite` 中该行的 ID，不改 records、检查点或 epoch；
3. 执行 `mem view <id>`：退出码 **1**，stdout 为空，stderr 为“索引快路径与直读结果不一致（B1 防御断言）——转直读”；
4. 真相记录仍完整存在，但调用方没有得到直读结果。

**影响**：索引从“可丢弃加速缓存”变成了读命令的单点故障，直接违反红线 8；同时修复登记中“不一致即报错转直读”的验收描述与实现不符。

**关闭条件**：不一致或索引查询失败时重新执行并返回直读结果，`read_mode` 如实标为 direct，不得返回错误阻断读取。增加“检查点仍新鲜、索引内容损坏”的生产 e2e；原复合视图、多类型、空类型用例继续保留。

## B3 残留：最后 fresh 查询之后、检查点快照之前仍可提前确认写入

**位置**：`crates/mem-core/src/freshness.rs:195-225`，关键顺序为 `:201-217`。

**代码事实**：每轮顺序是：

1. `maintain(store)` 物化索引；
2. `f()` 做最后订阅查询，得到 `Some(0)`；
3. `stat_snapshot(store)` 生成检查点；
4. `save_checkpoint(store, cp)` 发布。

外部写入可以落在步骤 2 和 3 之间。此时索引是旧内容，检查点却枚举新文件状态；如果写入保持尺寸和 mtime，后续 stat 比较无法发现它。维护后再查一次只覆盖了“maintain 之后、最后 fresh 之前”的窗口，没有覆盖最后 fresh 自身之后的窗口。

**确定性对抗结果**：临时测试使用生产 CLI 建店和记录、真实 `Index::rebuild + FTS` 物化器；fresh 回调模拟“OS 查询已经返回 0”后落入同尺寸写入并恢复 mtime，然后返回 `Some(0)`。结果：

```text
published=Some(Checkpoint { ..., index_epoch: 1 })
indexed=01743c94d7cf654f3be2bf7d9c9c41091d374495c176bb7a1a7f5d557a9ae376
disk=671f6e520d2be5119018dbf40c1215d5154b10b73a8da890340f22a5116515e6
```

临时测试以“不得发布检查点”为正确断言而失败，证明当前原语仍会签发错误证明。该测试源和二进制已清理，不属于当前项目测试失败。

**影响**：检查点、索引代际和订阅证明仍可能描述不同代状态。后续握手有时能再次发现事件并补救，但代码没有建立“补救发生在旧索引应答之前”的顺序保证，不能把概率性补救当作红线 21 的证明。

**关闭条件**：建立索引代际、stat 快照、订阅游标之间可重验的同代关系；发布后到应答前也必须能发现竞态，发现不了就直读。除现有“rebuild 后写入”用例外，新增“最后 fresh 查询后写入”的确定性回归，并断言第一次读取不返回旧摘要。

## B4 残留：x-ref 重挂使用了错误的类型声明，payload 与 envelope 分裂

**位置**：`crates/mem-core/src/ops.rs:829-835`、`:869-889`、`:912-926`。

**代码事实**：

- `dup_ref_fields` 从 **dup 的类型声明**提取 x-ref 字段；但待修改字段属于**引用方记录**，应从每个引用方自身的类型声明提取；
- canonical 收敛写只删除 `env_can.refs` 中指向 dup 的边，`fresh_can.payload` 原样写回，没有处理 canonical 自己的声明式 x-ref 字段。

**生产复现（只使用内置类型）**：

1. 建 topic D、topic C；
2. 建 memory M，payload 为 `{"title":"M","topic":D}`，此时 envelope 正确镜像 `topic→D`；
3. merge D→C 成功；
4. `mem show M` 显示：`payload.topic == D`，而 `envelope.refs == [{"rel":"topic","target":C}]`；
5. 携带 M 当前令牌、保持 `topic=D` 做普通 update，退出码 **1**，因为 D 已在 attic、引用目标不存在；
6. `mem verify` 仍输出 `ok:true`，因为当前 verify 只检查 envelope 断链，不检查 payload 与镜像的一致性。

**影响**：merge 成功后记录的两层公开引用语义立即互相矛盾；正常更新被自己的 merge 结果阻塞，后续若按 payload 再生成 envelope 还可能重新引入断链。R4 的“引用完整性”只修复了信封层，没有修复声明式写源。

**关闭条件**：按每个被改记录自身的类型声明处理 x-ref；canonical 的 payload 与 envelope 必须在同一次收敛写中一致处理。新增内置 `memory.topic`、canonical 自身 x-ref、第三方 x-ref 回归，断言 merge 后两层一致、无 dup 引用、原 payload 可继续 CAS update、事件链连续。verify 应增加镜像一致性检查，避免此类分裂继续显示全绿。

## B7 残留：FTS 命中面不等价，trace 续读不推进，brief 截断仍无续读

### 1. FTS 快路径不再忽略 offset，但与直读语义仍不一致

**位置**：`crates/mem-index/src/fts.rs:17-22`、`:63-112`；`crates/mem/src/commands/mod.rs:729-785`。

FTS 明确将 ID 设为 `UNINDEXED`，正文也不进 FTS；直读则声明并实现了 ID 前缀、别名、正文和 payload 子串匹配。因而两种模式不是同一查询的不同执行器，而是不同命中契约。

**生产复现**：对一条 memory 的**完整 ID**执行 `mem find <id>`：

```text
direct:  hits=1
indexed: hits=0, read_mode=indexed, via_subscription=rdc
```

此外 `idx.search(...).unwrap_or_default()`（`commands/mod.rs:736`）把查询错误转换为空命中，而不是直读兜底，同样会让索引故障表现为“没有结果”。

**关闭条件**：快路径实现 CLI 已声明的 ID 前缀等命中语义，或对 FTS 不支持的查询直读；查询失败不得伪装为空命中。用同一组记录和查询逐 ID 比较 direct/indexed，而不只比较分页数量。

### 2. trace 的 resume 在硬上限处重复同一页

**位置**：`crates/mem/src/commands/mod.rs:821-841`；预算钳制见 `crates/mem-core/src/budget.rs:22-25`。

**生产复现**：构造 A→B→C，合法配置 `trace_nodes=1`、`trace_depth=4`：

- 首次 trace 返回 B，建议 `--limit 2 --depth 5`；
- 按建议执行，limit/depth 被钳回 1/4，仍只返回 B；
- 再次输出的建议仍是 `--limit 2 --depth 5`，永远不推进到 C。

默认配置下只要图超过 200 个节点也会发生同类问题。“提高预算重跑”不能突破硬上限，也没有游标，因此不是可完成的续读路径。

**关闭条件**：提供真实遍历游标/偏移或不会重复已读节点的续读机制；回归必须在节点预算上限处执行返回的续读方式，并证明能读到后续节点且不会死循环。

### 3. brief 的字节截断只有标注，没有对应续读入口

**位置**：`crates/mem/src/commands/mod.rs:481-517`。

`brief_bytes` 现在会截断 items，但输出没有 offset/cursor；CLI 的 brief 也没有续读参数。`next` 只是通用的 session/diff/show 提示，无法从被截断的下一项继续。因此红线 9 的“截断必须给出续读路径”仍未闭环。

**关闭条件**：brief 的条数或字节截断都给出可执行的下一项游标，并增加“超过 `brief_bytes` 后沿 resume 读完且不重复”的用例。

---

## 已通过项的验证边界

- **B2/B5/B6/H1/H2**：本轮未发现继续阻断的正确性问题；现有 157 项测试全绿。
- **B5**：确认的是原子发布代码路径和唯一性测试，不是断电实测；macOS 的 `link(2)` 分支未在真机执行。
- **B6**：通过停用 USN 消除已知错误分支，不代表 USN 已修复；恢复前仍需 FRN 动态维护和提权验收。
- **H2**：CLI/MCP 实际是各自 rebuild 且不发布检查点，并非注释所称“全部调用统一收敛原语”；当前安全性来自不发布检查点，但注释应修正，避免后续维护误读。

## 验证覆盖与清理

- 已执行：`cargo check --workspace --all-targets && cargo test --workspace`，29 个套件、157/157 通过，零 Rust 警告。
- 已实测：RDC 快路径、损坏索引下 view、内置 x-ref merge、完整 ID 的 direct/indexed 对比、trace 按 resume 续读。
- 已注入：真实 SQLite 物化器下“最后 fresh 查询后、检查点快照前”的保 stat 写入。
- 未实测：macOS、真实断电/崩溃；USN 已停用，无提权 USN 运行结论。
- 临时 Rust 测试、Python 脚本、测试二进制和临时店均已删除；未修改实现代码。

## 自曝三处

1. **B3 使用原语注入，不是本轮真实 RDC 时序压测**。注入点精确位于生产函数允许的顺序之间，并使用真实索引和记录；若能补充机制证明“`f()` 返回后到检查点发布之间写入不可能发生，或任何读取前必然重验”，则可推翻本项。当前代码没有这项证明。
2. **B1 的索引损坏由评审直接写入派生库制造**。如果项目明确禁止任何外部因素损坏可丢弃索引，复现频率可重估；但设计已要求索引损坏/缺失时直读，因此该场景仍属于既定兜底契约。
3. **B7 的 trace 使用小预算缩短复现**。相同钳制逻辑在默认 200 节点上限处成立；若续读契约允许永远重复第一页并要求用户修改硬上限，则定级可重议，但这与当前“硬预算 + 可执行续读”表述不兼容。

## 待批准清单

**本轮请求批准**：B1、B3、B4、B7 仍未关闭及发布阻断定级；B2、B5、B6、H1、H2 的通过结论；在 4 项残留全部修复并补上本轮边界回归前，不进入发布准备。

**本轮明确不批**：具体修复方案选型、设计版本/修订行变更、恢复 USN、macOS/断电验收豁免、打包或发布。
