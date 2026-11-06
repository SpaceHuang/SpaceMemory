# mem v1.1 实施评审——第十一轮报告（R11，R10 B7 修复重审）

- 评审对象：R10 唯一残留 B7 的本轮修复
- 评审日期：2026-11-06
- 评审环境：Windows 11，非提权用户进程；生产快路径后端为 RDC
- 评审方式：修复读码、生产 CLI/MCP 对照、Unicode/ID/跨值命中面、双向跨模式续读、完整构建测试
- 基线结果：`cargo check --workspace --all-targets` 零 Rust 警告；`cargo test --workspace` 的 **30 个测试套件、168 项测试全部通过**

## 评审结论（TL;DR）

**B7 仍未关闭，保留 1 项发布阻断。本轮不批准进入发布准备。**

本轮已确认：直读正文 NFC/NFD 对齐、CLI/MCP 深度 frontier、brief 16 字节续读以及 brief/show 预算 1 的响亮拒收均通过。

但 find/trace 仍有四个生产失败：

1. 双向跨模式 find 游标仍漏命中/重复命中，offset 代码未修；
2. 大写 ID 前缀在 direct 不命中、indexed 命中；
3. `sync_fts` 用旧空格拼接覆盖新逐值折叠列，跨值假命中仍在生产路径；
4. MCP 节点预算截断返回 `resume:null`，没有续读路径。

因此修复登记中“R10 全部闭环”不成立。

## R10 场景复验

| 场景 | R11 结果 |
|---|---|
| 正文 NFC/NFD 两种查询 | direct/indexed 均命中同 3 个 ID，已修复 |
| `%/_`、payload 键名、ID 中段、普通 ID 前缀 | 全部保持通过 |
| 大写 ID 前缀 | direct 0、indexed 1，失败 |
| 跨值查询 `inbo` | 两模式均 0；但含分隔空格的 `n b` 为 direct 0、indexed 1 |
| direct→indexed / indexed→direct 两页 | 分别漏第二条、重复第一条，失败 |
| CLI/MCP 深度 frontier | 两端均返回 C 的 frontier，通过 |
| MCP 节点预算 1 | B + truncated:true + resume:null，失败 |
| brief 16 / 预算 1 | 16 下续读收敛；1 下 brief/show 均退出 1 并提示最小为 2，通过 |

---

## B7-1. find 跨模式 offset 仍是两个坐标系

**位置**：`crates/mem/src/commands/mod.rs:781-860`；索引 OFFSET 见 `crates/mem-index/src/fts.rs:113-124`。

两条 `shared` 的命中集和排序已一致，但第一页仍返回：

```text
direct resume:  {"offset":5}
indexed resume: {"offset":1}
```

direct→indexed 使用 5 返回空页，遗漏第二条；indexed→direct 使用 1 重复第一条。direct 仍在 `commands/mod.rs:830-859` 枚举全记录并以全记录下标续读，indexed 以命中行数续读。

R10 修复登记把该项改写为“跨值假命中”，没有处理原报告的 offset 失败。即使命中面完全对齐，这两个游标仍不能互换。

**关闭条件**：统一为命中序号或稳定游标；保留 direct→indexed、indexed→direct 双向生产回归，断言第二页为另一 ID，无重复、无遗漏。

## B7-2. ID 前缀的 ASCII 大小写语义仍不同

**位置**：直读 `crates/mem/src/commands/mod.rs:832`；索引 `crates/mem-index/src/fts.rs:108-115`。

直读使用大小写敏感的 `starts_with`；SQLite LIKE 默认 ASCII 大小写不敏感。将同一小写 ID 前缀转为大写：

```text
direct:  0 条
indexed: 1 条
```

**关闭条件**：明确沿用直读的大小写敏感前缀语义，并让索引执行器一致（或同步修改公开契约）；新增同 ID 小写/大写前缀对照。

## B7-3. 新逐值折叠函数被 sync_fts 的旧逻辑覆盖

**位置**：新函数 `crates/mem-index/src/sqlite.rs:40-56`；最终覆盖见 `crates/mem-index/src/fts.rs:51-67`。

`set_body_texts` 虽使用 `payload_values_folded`（逐值 fold + LF），但随后 `sync_fts` 仍执行：

```rust
let payload_values = crate::sqlite::values_text(...); // 空格连接
let payload_folded = mem_core::paths::fold(&payload_values);
UPDATE records SET payload_folded = ...;
```

因此生产最终物化结果仍是空格连接，不是新函数的 LF 逐值表示。payload 值为 `"in"`、`"bo"` 时：

```text
查询 inbo：direct 0、indexed 0（新增测试只覆盖此形）
查询 n b： direct 0、indexed 1（跨值假命中仍存在）
```

**关闭条件**：所有物化路径共用同一逐值表示，禁止后续旧逻辑覆盖；测试直接覆盖最终 `sync_fts` 结果，并包含跨值边界及其分隔符查询，direct/indexed 逐 ID 相等。若采用 LF 连接，还需定义含 LF 查询的边界；更稳妥的是保留独立值域而非拼接后模拟。

## B7-4. MCP 深度续读已修，节点截断仍无 resume

**位置**：`crates/mem-mcp/src/protocol.rs:164-203`。

MCP 深度截断现在返回 C 的 frontier，生产通过。但将 `trace_nodes=1`、A→B→C 后，真实 stdio 响应为：

```json
{"nodes":[{"id":"B","rel":"derived_from","depth":1}],"truncated":true,"resume":null}
```

`trace_from` 在节点数达到上限时提前返回，frontier 为空；`protocol.rs:176-195` 中节点截断使 `depth_limited=false`，最终落入 `else => Null`。这与修复登记“节点预算截断给 skip/limit”不符。

**关闭条件**：MCP 节点截断返回与 CLI 等价的 skip/limit（或 frontier）续读；新增 `trace_nodes=1` 的真实 MCP 回归，实际续读并验证后续节点可达。

---

## 验证覆盖与清理

- 已执行：`cargo check --workspace --all-targets && cargo test --workspace`，30 个套件、168/168 通过，零 Rust 警告。
- 所有失败均来自生产 `target/debug/mem.exe`；快路径实测 `via_subscription=rdc`，MCP 来自真实 stdio JSON-RPC 请求。
- 本轮未修改实现代码，未重开 B1/B3/B4 等已通过项；完整工作区回归保持通过。
- 临时脚本、正文输入和临时店 `review_r11*` 均已删除；`target/tmp` 下既有历史目录未动。
- macOS 真机、真实断电、USN 提权分支仍未验证；USN 当前保持停用。

## 自曝三处

1. **大写 ID 与带空格跨值查询是本轮补充边界**。它们分别落在既有 ID 前缀和逐值 contains 契约内，不要求新搜索特性；且生产两执行器结果相反。
2. **MCP 节点预算 1 是非默认配置**。该值被配置和遍历正常接受，红线9对所有截断生效，不能只保证默认 200 的场景。
3. **复杂 DAG frontier、macOS、断电和 USN 未作本轮扩展实测**。本报告只依据上述最小已复现失败，不从这些未测场景推定新问题。

## 待批准清单

**本轮请求批准**：B7 继续作为唯一发布阻断；find 游标/ID 大小写/最终物化命中面及 MCP 节点续读全部修复并有生产回归后，才可关闭 B7。

**本轮明确不批**：用其他子项修复替代原 offset 失败、只测不含分隔符的跨值查询、只验证 MCP 深度截断而忽略节点截断、设计版本/修订行变更、打包或发布。
