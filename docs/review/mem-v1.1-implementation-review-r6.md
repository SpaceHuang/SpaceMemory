# mem v1.1 实施评审——第六轮报告（R6，R5 修复重审）

- 评审对象：R5 残留 B1/B3/B4/B7 修复后的当前工作区
- 评审日期：2026-11-05
- 评审环境：Windows 11，非提权用户进程；生产快路径后端为 RDC
- 评审方式：逐项读码、生产 CLI 对抗复现、真实 RDC 查询后竞态注入、完整构建测试
- 基线结果：`cargo check --workspace --all-targets` 零 Rust 警告；`cargo test --workspace` 的 **29 个测试套件、159 项测试全部通过**

## 评审结论（TL;DR）

**R5 的 B1、B3、B4 已关闭；B7 仍未关闭，保留 1 项发布阻断。本轮不批准进入发布准备。**

B7 的三个原复现中，完整 ID 的 FTS 查找和 trace 节点数续读已经修复，brief 也增加了 offset；但以下边界仍由生产 CLI 复现：

1. payload/正文的任意子串在 direct 命中、indexed 漏命中；
2. 仅触及 trace 深度上限时，后继节点被静默省略，`truncated:false` 且无 resume；
3. 单条 brief 项超过字节预算时返回 0 项，`resume.offset` 不推进，续读死循环。

因此修复登记中的“R5 四项全部闭环”不成立；159 项全绿未覆盖这三条边界。

## 逐项结论

| R5 项 | R6 结论 | 证据摘要 |
|---|---|---|
| B1 索引不一致使 view 报错 | **通过** | 改坏索引 ID 后，`mem view` 退出码 0，返回正确记录并标注 `direct` |
| B3 fresh 后写入被检查点提前确认 | **通过** | 检查点使用索引同次扫描的 entries；真实 RDC 在最后查询后注入保 stat 写入，下一次读取证明得到未消化事件并转 direct |
| B4 merge 的 payload/envelope 分裂 | **通过** | 内置 `memory.topic` 在 merge 后两层均指向 canonical，原记录 CAS update 成功，`verify ok:true`；新增两项回归通过 |
| B7 FTS/trace/brief 契约 | **未通过：仍阻断** | 完整 ID 与 trace 节点续读已修复；子串、深度截断、brief 字节续读三项仍复现失败 |

---

## B7-a. FTS 仍不是直读子串语义的等价执行器

**位置**：`crates/mem-index/src/fts.rs:28-66`、`:76-124`；直读匹配见 `crates/mem/src/commands/mod.rs:777-821`。

**代码事实**：修复把 ID、别名、payload 和正文都放进 FTS 文本，解决了“字段未入索引”；但查询仍被转换为词项的 AND + 尾词前缀匹配。直读契约是大小写折叠后的任意子串，两者仍不等价。

**生产复现**：一条 memory 的标题为 `shared-alpha`，正文为 `prefix hello suffix`：

| 查询 | direct 命中 | indexed 命中 |
|---|---:|---:|
| 完整 ID（R5 原复现） | 1 | **1，已修复** |
| payload 子串 `hared-al` | 1 | **0** |
| 正文子串 `ello` | 1 | **0** |

这不是相关度或排序差异，而是同一公开 `mem find` 在快路径下漏掉已存在记录。

**关闭条件**：FTS 实现直读已声明的任意子串语义，或识别 FTS 无法表达的查询并转直读；不得仅扩大入索字段就宣称语义等价。回归须逐 ID 比较 direct/indexed，至少覆盖 ID 前缀、别名、payload 中间子串、正文中间子串、连字符和分页。

## B7-b. trace 深度上限仍静默丢节点，skip 不能续读深层边

**位置**：`crates/mem-core/src/refgraph.rs:182-230`，尤其是 `:212-213`；CLI 见 `crates/mem/src/commands/mod.rs:850-887`。

**代码事实**：`depth >= max_depth` 时直接 `continue`，不检查该节点是否还有未展开边，也不设置截断。节点数 `skip` 只跳过输出，无法在同一深度上限下到达更深节点。

**生产复现**：构造 A→B→C，配置 `trace_depth=1`、`trace_nodes=10`，排除节点数预算干扰：

```text
nodes=[B]
truncated=false
resume=null
```

C 被静默省略。R5 的节点数场景已另行复验：`trace_nodes=1` 时第一页 B，按 `resume.skip/limit` 第二页得到 C，**该部分通过**。

**关闭条件**：深度边界存在未展开边时必须显式标注截断，并给出能到达深层节点的 frontier/游标续读；仅返回相同 depth 加 skip 不构成续读。新增“节点预算充足、仅深度受限”的生产回归。

## B7-c. brief 单条项超过字节预算时，resume 永远停留在原 offset

**位置**：`crates/mem/src/commands/mod.rs:492-525`。

**代码事实**：若第一项已经超过 `brief_bytes`，`cut_at=0`，items 被截为空；resume 使用 `旧 offset + items.len()`，结果仍是旧 offset。

**生产复现**：合法配置 `brief_bytes=64`，存在多条记录：

```text
第一次：items=0, truncated=true, resume.offset=0
第二次：items=0, truncated=true, resume.offset=0
```

按照返回的续读方式永远无法读到任何记录。默认 16 KiB 不触发该现象，但当前配置没有更高下限校验，64 是实际接受的配置。

**关闭条件**：字节截断也必须保证续读推进；可分页到项内字节，或明确校验并拒收无法容纳最小项的配置，不能返回原地游标。新增“第一项即超预算”的回归，并沿 resume 验证能读完且不重复、不死循环。

---

## 已关闭项证据

### B1：直读兜底已实际执行

生产 sidecar 保持 RDC 快路径，修改派生索引中的 memory ID 后再执行单类型视图：返回码 0、ID 等于真相记录、`read_mode=direct`。不再出现 R5 的退出码 1 和空 stdout。

### B3：同代 entries + 每次读取的新鲜订阅查询形成闭环

检查点不再于最后 fresh 查询后重新枚举并吸收新 stat，而是使用 `MaintainOutcome.scan_entries`。临时对抗测试在真实 `RdcWatcher::fresh_undigested()` 返回 0 后，执行同尺寸写入并恢复 mtime，模拟最后查询刚结束的窗口；随后同一真实订阅返回未消化事件，`consult_with_hello` 判定 direct。测试通过。

这说明 R5 的“错误检查点被后续读取当作新值证明”路径已关闭；USN 仍处于停用状态，本结论不外推到 USN。

### B4：写源和镜像同步收敛

生产复现中，topic D→C merge 后，memory 的 `payload.topic` 与 `envelope.refs[target]` 均为 C；携带当前令牌继续 update 返回码 0，verify 全绿。canonical 自身 x-ref 置 null 和镜像审计也有新增测试覆盖。

## 验证覆盖与清理

- 已执行：`cargo check --workspace --all-targets && cargo test --workspace`，29 个套件、159/159 通过，零 Rust 警告。
- 已实测：R5 的 B1/B4/完整 ID/trace 节点续读均通过；B7 的子串、深度截断、brief 原地游标均失败。
- 已注入：真实 RDC 下“最后 fresh 查询后”的保 stat 写入，第一次读取证明正确转 direct。
- 未实测：macOS、真实断电/崩溃；USN 停用中。
- 临时 Rust 测试、Python 脚本、测试二进制、incremental 产物及临时店均已删除；未修改实现代码。

## 自曝三处

1. **FTS 定级依赖现有公开 find 契约**。如果项目决定把快路径定义为不同的“词项前缀搜索”，本项需要重新评审接口契约；但当前 CLI 明确声明 ID 前缀和正文字面子串，修复登记也承诺两种模式等价，不能静默改成两种语义。
2. **trace 深度可能被视为调用方主动限定的完整范围，而非截断**。但这里使用的是配置硬预算，且需求要求预算截断显式标注；若设计改为“深度上限不属于截断”，需先修改该契约，当前实现没有表达这种区别。
3. **brief 使用 64 字节小预算复现**。默认配置不受影响；若能证明配置层已经拒绝该值则可降级，但本轮生产 CLI 接受了该配置并实际返回原地游标。

## 待批准清单

**本轮请求批准**：B1/B3/B4 关闭；B7 仍作为唯一发布阻断，需覆盖本报告三条边界后方可关闭。

**本轮明确不批**：具体搜索实现选型、公开 find 语义变更、深度预算契约变更、设计版本/修订行变更、macOS/断电验收豁免、打包或发布。
