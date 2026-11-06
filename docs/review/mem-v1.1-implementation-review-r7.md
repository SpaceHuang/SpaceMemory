# mem v1.1 实施评审——第七轮报告（R7，R6 B7 修复重审）

- 评审对象：R6 唯一残留 B7 的修复
- 评审日期：2026-11-05
- 评审环境：Windows 11，非提权用户进程；生产快路径后端为 RDC
- 评审方式：修复读码、生产 CLI 对照直读/快路径、按返回 resume 实际续读、完整构建测试
- 基线结果：`cargo check --workspace --all-targets` 零 Rust 警告；`cargo test --workspace` 的 **29 个测试套件、161 项测试全部通过**

## 评审结论（TL;DR）

**B7 仍未关闭，保留 1 项发布阻断。本轮不批准进入发布准备。**

R6 的三个表面症状均有变化：原 payload/正文子串已命中，trace 深度截断已标注，brief offset 已推进；但修复没有同时满足完整契约：

1. LIKE 未转义通配符且不等价 Unicode 折叠，快/直读分页顺序也相反；
2. trace 建议提高 depth，但 CLI 立即钳回硬上限，续读原地重复；
3. brief 用强制保留首项消除死循环，却突破字节硬预算，并把实际 173 字节误报为 `bytes_used=2`。

因此修复登记中“R6 三个子项全部闭环”仍不成立。

## R6 场景复验

| 场景 | R7 结果 |
|---|---|
| 完整 ID，direct/indexed | 均命中同一条，保持通过 |
| payload 中段子串 `hared-al` | 均命中，原问题已修 |
| 正文子串 `ello` | 均命中，原问题已修 |
| trace 节点预算 1，按 skip/limit 续读 | 第一页 B、第二页 C，保持通过 |
| trace 深度预算 1 | 已改 `truncated:true`，但按建议 `--depth 2` 仍只返回 B |
| brief 字节预算 64 | offset 从 0→1→2，但每项实际超过预算，计数失真 |

---

## B7-1. LIKE 通配符、Unicode 折叠和排序仍破坏 find 等价性

**位置**：`crates/mem-index/src/fts.rs:76-123`；直读见 `crates/mem/src/commands/mod.rs:782-826`。

### 字面 `%` 被当作任意字符串

当前直接构造 `format!("%{}%", q)`，没有转义 LIKE 的 `%` 和 `_`。对不含 `%` 的记录库执行字面查询 `%`：

```text
direct:  0 条
indexed: 4 条（本例全部现存记录，含 principal）
```

快路径因此返回查询范围外记录，不只是漏命中。

### Unicode 大小写折叠不等价

记录标题为 `Ärger`，查询 `ärger`：

```text
direct:  1 条
indexed: 0 条
```

直读使用 NFC + Unicode 小写；SQLite LIKE 默认只处理 ASCII 大小写。当前仅把查询字符串转小写，没有对索引列做同样折叠，所以 `crate_fold` 的“与 paths::fold 对齐”注释不成立。

### 分页顺序相反

两条均匹配 `shared` 的 memory，limit=1：

```text
direct 第 1/2 页：A / B
indexed 第 1/2 页：B / A
```

直读按规范化路径排序，索引按 modified DESC、id DESC 排序。两种模式虽能在全量时得到相同集合，但同一 offset 页内容不同，不满足 R4 起要求的“两个模式逐条对齐”。

**关闭条件**：按字面语义转义 LIKE 特殊字符，或改用真正的子串实现；查询与索引内容采用相同的 NFC/小写折叠；direct/indexed 使用同一稳定排序和同一 offset 定义。新增 `%`、`_`、Unicode 大小写/NFC、两条记录两页逐 ID 对照测试。

## B7-2. trace 深度截断已有标注，但 resume 仍不可执行

**位置**：`crates/mem/src/commands/mod.rs:855-902`，钳制点为 `:863-865`；深度检测见 `crates/mem-core/src/refgraph.rs:178-261`。

**生产复现**：A→B→C，`trace_depth=1`、节点预算充足：

1. 第一次返回 B，正确标注深度截断，建议 `mem trace A --dir up --depth 2`；
2. 执行该建议时，`depth=2.min(max_depth=1)` 被钳回 1；
3. 仍返回 B，并再次给出完全相同的 `--depth 2` 建议，永远到不了 C。

默认配置下，深度超过 4 的图同样受硬上限约束。提高参数不能突破硬上限，因此它不是续读机制。

**关闭条件**：resume 携带可跨越深度边界的遍历 frontier/游标，或其他不会被预算钳制的真实续读方式。测试必须实际执行返回的 resume，并断言读到 C，而非只断言 `truncated:true`。

## B7-3. brief 以突破硬预算换取游标推进，且 bytes_used 失真

**位置**：`crates/mem/src/commands/mod.rs:492-533`，强制首项见 `:506-511`。

`cut_at.max(1)` 保证至少返回一项，但没有把该项计入 `used`，也没有控制该项大小。合法配置 `brief_bytes=64` 时，本轮实测：

```text
首项实际序列化大小：173 字节
bytes_budget：64
bytes_used：2
items：1
resume.offset：1；下一次为 2
```

续读死循环已消失，但 R4.2 的确定字节上限被破坏，输出中的预算记账也不真实。

**关闭条件**：不得返回超过硬预算的完整首项；使用项内字节截断/续读，或在配置入口拒收无法容纳最小项的预算。`bytes_used` 必须与实际输出记账一致。新增“首项超预算”测试，同时断言返回规模不超限、记账准确、游标推进。

---

## 验证覆盖与清理

- 已执行：`cargo check --workspace --all-targets && cargo test --workspace`，29 个套件、161/161 通过，零 Rust 警告。
- 已实测：R6 的原子串和 trace 标注已改善；本报告所有失败均来自生产 `target/debug/mem.exe`。
- 新增回归只覆盖普通子串和“深度截断被标注”，未覆盖 LIKE 通配符、Unicode、分页顺序、执行深度 resume、首项超预算后的实际字节数。
- 未变更 B1/B3/B4 等已通过代码；完整工作区测试保持通过。
- 临时 Python 脚本和临时店均已删除；未修改实现代码。

## 自曝三处

1. **LIKE 通配符可能是未声明的模式搜索特性**。若公开契约明确允许 `%/_` 模式，则该项应改写接口而非按字面子串验收；当前 CLI 与修复登记均声明子串查询，没有声明模式语义。
2. **Unicode 用例超出本轮新增 ASCII 测试**。但内置类型和路径规则公开支持 Unicode，find 没有 ASCII 限制，因此它属于现有输入域而非额外特性需求。
3. **小预算用例不是默认配置**。trace 默认值只改变触发规模，不改变钳制逻辑；brief 默认下首项通常不超预算，但 64 是实际接受的配置。若选择提高并强制最小预算，必须有配置校验证据，不能仅在代码中假定。

## 待批准清单

**本轮请求批准**：B7 继续作为唯一发布阻断；上述三组边界全部有回归并通过后，才可关闭 B7。

**本轮明确不批**：把 LIKE 模式设为公开新语义、放宽字节硬预算、允许超过 trace 硬上限、设计版本/修订行变更、打包或发布。
