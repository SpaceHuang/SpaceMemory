# mem v1.1 实施评审——第十二轮报告（R12，R11 修复与主动优化重审）

- 评审对象：R11 残留 B7 修复及本轮主动自检优化
- 评审日期：2026-11-06
- 评审环境：Windows 11，非提权用户进程；生产快路径后端为 RDC
- 评审方式：修复读码、生产 CLI/MCP 对照、双向分页、真实 resume 重放、完整与重复工作区测试
- 验证概况：`cargo check --workspace --all-targets` 零 Rust 警告；测试存在**间歇性 B4 失败**，详见下文，不能记为稳定全绿

## 评审结论（TL;DR）

**保留两项发布阻断：B7 尚未完全关闭；B8（红线21 新鲜性间歇失效）重新打开。本轮不批准进入发布准备。**

R11 的四个原始点均有实质改善：大写 ID 前缀对齐、双向跨模式分页对齐、`n b` 跨值假命中消失、MCP 节点截断不再返回 `resume:null`。

但继续执行完整契约仍发现：

1. LF 拼接把字面换行查询变成跨值/跨别名命中，direct 0、indexed 5；
2. MCP 节点 resume 虽非空，但输入端忽略 skip/limit，重放后重复同一页；
3. 本轮登记宣称完成的 MCP show/brief 预算实际未接入；
4. 标准全量测试两次在 B4 返回 indexed 旧令牌，隔离/另一次全量又通过，新鲜性证明存在间歇失败。

## R11 原始场景复验

| 场景 | R12 结果 |
|---|---|
| 大写 ID 前缀 | direct/indexed 命中同一 ID，已修复 |
| direct→indexed / indexed→direct 两页 | 两端 resume 均为 1，第二页均为另一 ID，已修复 |
| 跨值查询 `n b` | 两模式均 0，已修复 |
| 字面换行 `\n` 查询 | direct 0、indexed 5，仍失败 |
| MCP 深度 frontier | 返回 C 的 frontier，保持通过 |
| MCP 节点截断 | 首次有 skip=1/limit=2；携带它们重放仍重复 B，未闭环 |
| MCP show，brief_bytes=16 | 正文 205 字符完整返回，无截断/resume；CLI 同店仅 16 字符 |
| MCP brief，brief_bytes=16 | 11 个完整 items，无 bytes_budget/bytes_used/skipped/resume |

---

## B7-1. LF 分隔符本身成为假命中源

**位置**：`crates/mem-index/src/sqlite.rs:46-63,247-259,349-363`。

空格拼接已改为 LF 拼接，`n b` 因此不再假命中；但查询可以直接包含换行。对不含换行的任一字符串值执行字面查询 `\n`：

```text
direct:  0 条
indexed: 5 条
```

这些 indexed 命中来自多个值/别名之间的 LF，而不是任何记录值或正文包含换行。R11 已明确指出“采用 LF 连接还需定义含 LF 查询边界”，当前没有处理。

**关闭条件**：不要用拼接文本模拟独立值域；或确保任何查询都不能跨值/别名匹配，包括分隔符自身。新增 `\n`、跨分隔符两端子串及普通跨值查询的 direct/indexed 逐 ID 回归。

## B7-2. MCP 节点 resume 仍不可执行

**位置**：`crates/mem-mcp/src/protocol.rs:164-206`；工具 schema 见 `crates/mem-mcp/src/tools.rs:38-40`。

`trace_nodes=1`、A→B→C 下，第一次响应已有：

```json
{"reason":"节点预算截断","skip":1,"limit":2}
```

但真实第二次 `tools/call mem_trace` 携带同一 id、`skip:1`、`limit:2` 后，仍返回同一个 B 和同一个 resume。原因：

- 工具 schema 只声明 id/dir；
- `protocol.rs:170-172` 始终从配置读取 depth/nodes 并调用 `trace(..., skip=0)`；
- 没有读取 arguments.skip/limit，也没有调用 `trace_from`。

所以“resume 非空”只修了外观，没有修续读。

**关闭条件**：MCP schema 和处理函数接受并执行 skip/limit（或改发可执行 frontier）；真实 MCP 回归必须重放 resume，断言不重复 B 并能到达后续节点。

## B7-3. 本轮宣称的 MCP show/brief 预算未落地

**位置**：`crates/mem-mcp/src/protocol.rs:76-105,153-162`；预算函数检索不到 `effective_brief_bytes` 实现。

生产在 `brief_bytes=16` 下：

- MCP `mem_show` 返回完整 205 字符正文，没有 body_truncated/body_offset/resume；CLI 同记录只返回 16 字符并给 resume=16；
- MCP `mem_brief` 返回 11 个完整 items，没有 bytes_budget、bytes_used、skipped_over_budget 或字节续读；
- 工具 schema 的 `mem_show` 也只有 id，没有 body_offset。

这与修复登记“主动自检优化轮”中“MCP show/brief 预算对齐”“预算校验单源化”的声明直接冲突。R4.2/红线9不因前端不同而豁免。

**关闭条件**：实际接入共享预算校验、show 正文截断/续读、brief items 字节预算和 skipped 清单；补齐工具 schema，并用真实 MCP 请求验证 1/16 预算及续读，而非只登记设计意图。

## B8. B4 新鲜性回归在标准全量测试中间歇返回旧令牌

**位置**：失败断言 `crates/mem/tests/fastpath.rs:135-161`。

本轮按标准命令连续两次执行 `cargo test --workspace`，均在 B4 场景失败：同尺寸修改 title T→U 并还原纳秒 mtime 后，第一次读取仍走 `indexed(RDC)`，返回的 digest 与磁盘现状不同。

```text
read_mode: indexed via_subscription=rdc
返回 token.digest != mem_core::ops::get 的 fresh token.digest
```

另两次结果为：单独复跑该测试通过一次；`cargo test --workspace --no-fail-fast` 完整运行通过一次（30 套件、166/166）。这证明失败具有时序/调度敏感性，不能当作稳定全绿，也不能用后续通过抹除前两次旧值应答。

**已确认**：红线21要求的“第一次读取不得返回旧值”在标准并行测试下两次被破坏。**尚未确认根因**：RDC 通知到达与读路径新鲜查询之间的竞态是待验证假设，本报告不把假设写成事实。

**关闭条件**：查明并修复该时序窗口；保留同尺寸 + 纳秒 mtime 还原 + 第一次读取的对抗测试，在并行负载下重复验证不再出现 indexed 旧令牌。不能通过放宽断言或仅串行运行掩盖。

---

## 验证记录与清理

按实际顺序执行：

1. `cargo check --workspace --all-targets`：通过，零 Rust 警告；
2. `cargo test --workspace`：B4 失败；
3. 单测 `fastpath_end_to_end_via_production_binary`：通过；
4. 再次 `cargo test --workspace`：B4 再次失败；
5. `cargo test --workspace --no-fail-fast`：30 个套件、166/166 通过一次。

生产复验均使用 `target/debug/mem.exe`；快路径为 RDC，MCP 为真实 stdio JSON-RPC。未修改实现代码，临时脚本、正文输入和临时店 `review_r12*` 均已删除，既有历史目录未动。macOS、真实断电和 USN 提权分支仍未验证。

## 自曝三处

1. **换行查询不是默认搜索示例**，但 CLI 接受任意字面子串，且 R11 已把 LF 查询列为关闭条件；当前 direct/indexed 差异是实测，不是语义推定。
2. **MCP show/brief 缺口可能早于本轮存在**。本轮主动优化明确宣称已修，故纳入重审；即使不是新回归，R4.2 仍使其成为发布阻断。
3. **B8 根因尚未定位**。阻断依据是两次标准测试中的旧令牌事实，而非对 RDC 内部机制的未证实推断；隔离测试通过只能证明其间歇性。

## 待批准清单

**本轮请求批准**：B7 继续阻断，并重新打开 B8。LF 命中面、MCP 可执行 resume、MCP show/brief 预算及 B4 新鲜性时序全部关闭后，才可进入发布准备。

**本轮明确不批**：把 resume 非空当作可执行、用隔离测试或 `--no-fail-fast` 的一次通过覆盖标准测试失败、只登记 MCP 预算设计而无实现、设计版本/修订行变更、打包或发布。
