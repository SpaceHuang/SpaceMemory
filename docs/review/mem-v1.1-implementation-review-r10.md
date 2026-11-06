# mem v1.1 实施评审——第十轮报告（R10，R9 B7 修复重审）

- 评审对象：R9 唯一残留 B7 的本轮修复
- 评审日期：2026-11-06
- 评审环境：Windows 11，非提权用户进程；生产快路径后端为 RDC
- 评审方式：修复读码、生产 CLI/MCP 对照、双向 Unicode 与跨模式续读、实际执行 frontier、完整构建测试
- 基线结果：`cargo check --workspace --all-targets` 零 Rust 警告；`cargo test --workspace` 的 **29 个测试套件、165 项测试全部通过**

## 评审结论（TL;DR）

**B7 仍未关闭，保留 1 项发布阻断。本轮不批准进入发布准备。**

本轮有效修复包括：查询侧已复用 `paths::fold`，NFD 查询与 NFC 查询的 indexed 结果一致；brief 在 16/64 字节下不再返回超预算条目，记账准确且续读到终点；CLI trace 已返回包含 C 的 frontier，后续命令可执行。

仍有四个精确缺口：

1. 直读正文仍只做小写转换，NFD 正文在 direct 漏命中；
2. find 的 offset 坐标系完全未改，双向跨模式续读仍漏/重；
3. MCP `mem_trace` 把 frontier 丢弃，截断后没有续读路径；
4. brief 接受预算 1，但空 items 数组本身为 2 字节，仍突破硬预算。

因此修复登记中“R9 全部闭环”不成立。

## R9 场景复验

| 场景 | R10 结果 |
|---|---|
| `%/_`、非 ASCII 大小写、payload 键名、ID 前缀/中段 | 全部保持通过 |
| NFC/NFD 两种查询，direct/indexed | indexed 已对齐；两查询均为 direct 2、indexed 3，direct 漏 NFD 正文 |
| 两条 `shared` 各自同模式两页 | 页内 ID 顺序一致，通过 |
| direct→indexed | direct resume=5；indexed 第二页为空，漏第二条 |
| indexed→direct | indexed resume=1；direct 重复第一页 ID |
| CLI trace 深度 1，A→B→C | resume.frontier 包含 C；执行 frontier 后续命令正常结束，通过本链场景 |
| MCP trace 同一链 | 返回 B + `truncated:true`，无 resume/frontier，失败 |
| brief 16/64 | items actual=2、bytes_used=2、skipped=14；下一页到终点，通过 |
| brief 1 | items actual=2、bytes_used=2、bytes_budget=1，失败 |

---

## B7-1. 直读正文仍未采用统一 NFC 折叠

**位置**：`crates/mem/src/commands/mod.rs:817-828`；查询侧修复见 `crates/mem-index/src/fts.rs:145-151`。

`crate_fold` 已改为 `paths::fold`，该修复有效；但直读正文仍是：

```rust
String::from_utf8_lossy(&r.record.body).to_lowercase().contains(&q.to_lowercase())
```

生产店有两条等价 payload（NFC/NFD 各一）和一条 NFD 正文记录。无论查询 NFC `Café` 还是 NFD `Cafe\u0301`：

```text
direct:  2 条（仅 payload）
indexed: 3 条（payload + NFD 正文）
```

这不是查询侧问题，而是直读正文未 NFC。新增索引测试只覆盖索引执行器，fastpath 仍只查 `Ärger`，没有正文 NFC/NFD 对照。

**关闭条件**：直读正文也复用 `paths::fold`；对 payload 与正文分别执行 NFC 内容/查询、NFD 内容/查询矩阵，direct/indexed 逐 ID 相等。

## B7-2. find 跨模式游标未修复，根因不是命中集

**位置**：`crates/mem/src/commands/mod.rs:772-852`；索引 OFFSET 见 `crates/mem-index/src/fts.rs:113-124`。

修复登记称跨模式问题的“根因即折叠不同源”，但生产在 `shared` 命中集已完全一致时仍复现：

```text
direct 第一页 resume:  {"offset":5}
indexed 第一页 resume: {"offset":1}
```

- direct→indexed 使用 5：返回 0 条，漏掉第二条；
- indexed→direct 使用 1：重复返回第一页 ID。

原因是 indexed offset 为命中序号，direct 在 `:821-849` 使用全记录下标并在第二个命中处返回其全记录下标。本轮没有修改该代码，也没有增加双向交叉续读测试。

**关闭条件**：统一为命中序号或稳定游标；在命中集相同的条件下，分别执行 direct→indexed 和 indexed→direct 两页对照，断言第二页为另一 ID，无重复、无遗漏。

## B7-3. CLI frontier 已接通，MCP 仍明确丢弃续读信息

**位置**：CLI `crates/mem/src/commands/mod.rs:900-944`；MCP `crates/mem-mcp/src/protocol.rs:164-175`。

CLI 在 `trace_depth=1`、A→B→C 下返回 B，并在 `resume.frontier` 中返回 C 及下一条命令；实际执行该 frontier 后续命令正常结束，无需调大预算。本链场景的 CLI 续读已通过。

但 MCP 仍是：

```rust
let (nodes_out, truncated, _frontier) = ...;
```

响应只有 `nodes` 和 `truncated`。生产 stdio `tools/call mem_trace` 实测：

```json
{"nodes":[{"id":"B","rel":"derived_from","depth":1}],"truncated":true}
```

没有 resume/frontier，MCP 调用方无法获得 C 或续读方式。这也与修复登记“CLI/MCP resume 携带 frontier”的声明不符。

**关闭条件**：MCP 返回同等可执行 frontier/resume；新增真实 MCP 请求回归，执行后续路径并验证剩余链可达。CLI 还应补交会合/分叉图，避免 frontier 重复，但本轮结论不以该未实测扩展场景为依据。

## B7-4. brief 16/64 已修，最小合法预算 1 仍越界

**位置**：`crates/mem/src/commands/mod.rs:498-548`。

16/64 下，新“跳过 + skipped_over_budget”语义工作正常：items 为 `[]`，actual=bytes_used=2，14 项均登记，resume=14，下一页无剩余。R9 的两个主要预算场景通过。

但配置仍接受 `brief_bytes=1`，代码仅 `.max(1)`。生产结果：

```text
items：[]
实际 items 序列化：2 字节
bytes_used：2
bytes_budget：1
```

空数组是最小 JSON 表示，因此预算 1 在当前记账口径下不可满足，应明确拒收，而不是接受后越界。

**关闭条件**：配置入口要求 `brief_bytes>=2`（或更高的可续读最小值），并保留 16/64 的实际字节、记账和续读断言；新增 1 的拒收测试。

---

## 验证覆盖与清理

- 已执行：`cargo check --workspace --all-targets && cargo test --workspace`，29 个套件、165/165 通过，零 Rust 警告。
- 所有失败均来自生产 `target/debug/mem.exe`；快路径实测 `via_subscription=rdc`，MCP 失败来自真实 stdio JSON-RPC 请求。
- 本轮未修改实现代码，未重开 B1/B3/B4 等已通过项；完整工作区回归保持通过。
- 临时脚本、正文输入和临时店 `review_r10*` 均已删除；`target/tmp` 下既有历史目录未动。
- macOS 真机、真实断电、USN 提权分支仍未验证；USN 当前保持停用。

## 自曝三处

1. **CLI frontier 的后续命令返回空 nodes**。本轮仍判该链场景通过，因为 C 已作为 frontier 明确返回，后续命令正常收敛；没有把“nodes 必须再次包含 C”作为新门槛。MCP 则连 C/frontier 都没有，性质不同。
2. **brief 字节记账沿用前几轮的 items 数组口径**，未把 skipped 清单和外围元数据重新纳入 bytes_used；预算 1 的失败即使在这一较窄口径下仍成立。
3. **分叉/会合图、macOS、断电和 USN 未完成扩展实测**。本报告阻断仅依赖已实测的最小场景，不以这些未测边界作推定失败。

## 待批准清单

**本轮请求批准**：B7 继续作为唯一发布阻断；直读正文折叠、双向 find 游标、MCP frontier 和 brief 最小预算全部修复并有生产/回归证据后，才可关闭 B7。

**本轮明确不批**：把命中集相同当作游标相同、只验证 CLI 而宣称 MCP 同构、接受预算 1 后返回 2 字节、设计版本/修订行变更、打包或发布。
