# 关键词检索与载荷字段过滤（R10）实施评审报告

- 评审对象：`r10-keyword-search` 分支相对 `main` 的实施改动（`bbf6bad..adf5173`）
- 评审日期：2026-11-10
- 评审标尺：`docs/requirement/agent-memory-requirements.md` v1.4 的 R10.3、R10.5、R10.10 及验收 8；`docs/specs/type-declaration.md` v1.1 的数值等值约定
- 评审方式：改动读码、生产 CLI 反例复现、`cargo check --workspace`、`cargo test --workspace`

## 评审结论（TL;DR）

**不予通过：1 项阻断。** 数值过滤在整数与浮点表示混合比较时转为 `f64`，会把超过 `2^53` 的不同 JSON 数值判为相等，返回实际不满足过滤条件的记录。该逻辑由 direct/indexed、CLI/MCP 共用，因此四个入口均受影响。

其余已审的查询文法、否定词项、索引超集粗筛、坏记录告警及新鲜性回退未发现新的阻断；但现有 210 项测试全部通过并不能覆盖本报告的大数反例。

## 阻断项

### B1 数值过滤经 `f64` 比较，不同的大数被误判为等值

**代码位置**：`crates/mem-core/src/find.rs:127-137`；标量字段调用点 `:160-163`，数组元素调用点 `:144-146`。

**约定**：过滤采用 JSON 数值等值，`"1"` 与 `"1.0"` 同值；清单与检索只返回匹配记录。该约定要求消除文本表示差异，不允许用会丢失精度的近似比较扩大命中集。

**问题**：`number_eq` 只在双方都为 `i64` 或双方都为 `u64` 时执行整数比较；一方是整数、另一方是浮点表示时进入 `as_f64()` 分支。`f64` 在超过 `2^53` 后不能表示每个整数，因此不同的 JSON 数值可能得到相同的 `f64`，产生误命中。数组中的数值元素复用同一函数，也有相同问题。

**生产复现（direct 路径）**：声明 `num.score` 为 `{"type":"number","x-filter":true}`，分别写入两条记录：

```json
{"score":9007199254740993}
{"score":9007199254740992.0}
```

随后执行：

```text
mem list --type num --filter score=9007199254740992.0
```

实际返回 **2 条**记录；预期只返回 `score = 9007199254740992.0` 的 **1 条**。第一条真实值是 `2^53 + 1`，过滤值是 `2^53`，两者并不相等，但当前实现均转换为 `9007199254740992.0` 后判等。

**影响**：这是错误的正常命中，不是告警或性能降级。它违反 R10.10 及验收 8 的“仅返回匹配记录”，并使过滤结果依赖数值的 JSON 表示形式；由于精筛共用该函数，indexed 路径和 MCP 前端不能规避。

**必须修复**：

1. 数值等值不得经过有损的 `f64` 转换；在保留 `1` 与 `1.0` 同值的同时，精确保留并比较 JSON 数值的数学值。
2. 先补反例回归，再修实现：至少覆盖 `9007199254740993` 与 `9007199254740992.0` 的正反方向，并覆盖标量及数组元素。
3. 保持 direct/indexed 命中一致，并对 CLI/MCP 至少各做一次生产路径验证。

## 验证记录

- `cargo check --workspace`：通过，零警告。
- `cargo test --workspace`：通过，210 项测试全部成功；现有数值用例只覆盖 `1`、`1.0`、`1.5` 等可精确表示的小数，未覆盖 `2^53` 以上的整数/浮点混合比较。
- `git diff --check main...HEAD`：通过。
- 生产反例已在无 sidecar 的 direct 路径复现；临时存储目录已删除，本轮未修改实现代码。

## 待批准清单

**本轮请求批准**：B1 修复并完成上述回归前，不批准 R10 实施通过。

**本轮明确不批**：以现有全量测试全绿替代大数精度验证；继续用 `f64` 等值作为 JSON 数值过滤的最终裁定。

---

# 第二轮重审（2026-11-10）——B1 修复复验

- 评审对象：`adf5173..3d9638c`（R10-B1 修复）
- 评审方式：修复读码、原反例的 direct/indexed 生产 CLI 复跑、真实 stdio MCP 回归、全工作区检查与测试

## 重审结论

**B1 关闭，未发现新阻断；R10 实施通过本轮评审。**

`number_eq` 现在分别处理整数×整数、整数×浮点和浮点×浮点。原反例所属的整数×浮点路径不再把整数转换为 `f64`，而是要求浮点侧为有限整值，再以 `i128` 精确展开后比较；`i64`/`u64` 整数域均可由 `i128` 无损承载。同一实数的整数/浮点表示仍互认，既有 `1` 与 `1.0` 行为保持。

## 关闭证据

1. **内核回归**：`filter_number_precision_beyond_f64_safe_range` 覆盖原反例正反方向、标量与数组元素，以及 `2^53` 的整数/浮点互认和安全区间内的 `1`/`1.0` 行为。
2. **CLI 生产复跑**：同一店中保存 `9007199254740993` 与 `9007199254740992.0` 两条记录；过滤 `9007199254740992.0` 和 `9007199254740993` 均只返回各自记录。direct 与实际 `read_mode=indexed` 两种路径的命中 ID 分别一致。
3. **MCP 生产回归**：真实 stdio `mem_list` 两个方向均各命中 1 条且 ID 不同；`mem_find` 组合过滤同样只命中 1 条。
4. **全工作区验证**：`cargo check --workspace --all-targets` 通过、零 Rust 警告；`cargo test --workspace --no-fail-fast` 的 **213 项测试全部通过**；`git diff --check main...HEAD` 通过。

## 非阻断测试改进

`cli_number_filter_exact_beyond_f64_safe_range` 的 indexed 段目前只等待索引文件出现，没有断言响应确为 `read_mode=indexed`，理论上可能在 direct 下空转。本轮已通过生产响应显式确认实际进入 indexed，因此不影响 B1 关闭；建议后续在该回归中补充模式断言，防止未来等待逻辑退化后失去覆盖。

临时复验存储已删除，本轮未修改实现代码。

---

# 第三轮评审（2026-11-10）——HEAD 全量复审

- 评审对象：`r10-keyword-search` 分支相对 `main` 的全部实施改动（`bbf6bad..a8bc5be`，HEAD 全量——含第二轮之后的 `a8bc5be` 回归补强）
- 评审方式：全量读码（`find.rs` / `proved.rs` / `sqlite.rs` / `protocol.rs` / `commands/mod.rs` / `cli.rs` / `decl.rs` / `keywords.rs` / `ops.rs` / `search.rs` / R10 测试五件 + 既有回归迁移）＋对照 `keyword-search-design.md` v0.3 七决策与红线 8/9/18/21 ＋ `cargo check --workspace --all-targets` ＋ `cargo test --workspace`
- 评审日期：2026-11-10

## 评审结论（TL;DR）

**不予通过：1 项阻断（B2）。** 索引物化的检查点 stat 补采把集合范围从"该次扫描看到的文件"扩大为"磁盘当前文件"——扫描之后新落盘的文件可被检查点确认，而索引中没有它的内容，读路径将错误地判定 Indexed 并**漏掉该记录**。这是 r5 B3 立法明文禁止的"检查点提前确认"窗口（红线21）在 R10 实施中的回归。

其余已审面（查询文法、组合谓词、正文 UTF-8 收口、过滤执行器与兼容类型集、open_proved 全门禁、坏记录告警双路径同序同子集、两前端同码同形、数值精确保持）未发现新的阻断；6 项非阻断观察见后。213 项测试全部通过不能覆盖 B2——该窗口需要"扫描后、补枚举前注入新文件"的时序用例，现有测试无一落在该窗口内。

## 阻断项

### B2 检查点 stat 补采重新枚举磁盘，扫描后新增文件被"提前确认"——indexed 可漏召回

**代码位置**：`crates/mem-index/src/sqlite.rs`，`rebuild_with_entries` 内 COMMIT 之后的补采循环（当前 `315–332` 行：`for rel in mem_core::event::enumerate_md(store, "records")` 一段）。

**约定背景（就地解释）**：检查点是快路径的"新鲜性证明"——它记录一组文件 stat（路径 → mtime/长度），读路径拿磁盘现状与它逐项比对，完全一致才走索引（红线21：索引仅新鲜性可证明时服务读）。为保证"检查点声称一致 = 索引内容完整"，检查点的 stat 集必须**恰好等于索引物化那一次扫描所看到的文件集合**。这是 r5 B3 的立法结论，`freshness.rs` 的 `publish_converged_checkpoint` 处有明文注释："检查点 entries 采自**与索引同一次扫描**的文件集——**绝不在此刻重新枚举磁盘**；'最后 fresh 查询之后'的写入：新文件不在 entries（实时枚举失配 → 直读）"。设计文档 §2.5 也据此定稿："复用**该次扫描**的 stat 集（覆盖**该次扫描看到的**全部 records/ 文件，含不可解析/不可读文件）"。

**问题**：R10 为承接坏记录告警（决策⑦/B1）把 stat 集扩到坏文件——动机正确，但实现是在事务 COMMIT 后**重新枚举磁盘**（`enumerate_md`）来补采，而不是取同一次扫描已产出的坏文件清单（`scan.bad`）。补采集合因此 = "磁盘当前文件"，包含扫描开始之后才落盘的文件。

**触发时序（全部为生产路径允许的交错，无需异常条件）**：

1. 物化例程 `scan_all`（T0）——此时新文件 X 尚不存在，索引 `records`/`bad_records` 两表都不会有 X 的行；
2. X 在 T0 之后、补枚举（T2）之前落盘；
3. 补枚举（T2）枚举到 X → X 连同其 stat 进入 entries；
4. 收敛原语做新鲜订阅查询（T3）：sidecar 的消化循环独立于物化例程——若它已消化 X 的变更日志事件（T0.5 落盘、T0.6 消化是现实时序），查询结果为 0 → 检查点被发布：**entries 含 X、索引无 X**；
5. 读路径：实时枚举含 X、stat 与 entries 一致、epoch 同代 → 判定 Indexed → **命中清单漏 X**；若 X 是坏文件，`bad_records` 无其行，**告警同样漏 X**（决策⑦的"告警新鲜"在新增方向失效）。

**为什么既有防护不兜底**：r5 B3 防护"扫描后新文件"的机制恰恰是"新文件不在 entries → 实时枚举失配 → 直读"——补枚举把失配信号本身消掉了；`undigested > 0` 的兜底只在 sidecar 尚未消化时有效，消化完成后即失效。设计 §2.1 的铁律是"不存在'已知可能返回旧值'的路径"——坏检查点在 sidecar 下一轮维护覆盖它之前的存活期内，每次读都在漏，窗口存在即违反，与概率高低无关（红线是硬约束）。

**既有测试为何没有抓住**：`maintenance_race.rs`（r4 B3）注入的是**修改既有文件**，且注入点在 `rebuild_with_entries` 返回**之后**（补枚举之后），靠订阅事件兜底——与本题窗口正交；`bad_records_same_tx_and_stat_covers_all_files` 的坏文件**先于物化**写入，只验证了 entries 覆盖坏文件的正路，没有护栏方向的断言（entries **不得**含扫描后新增文件）。两轮实施评审的 213 项用例均不落在 T0–T2 窗口内。

**影响面**：两前端共用同一执行器与同一检查点判定，CLI 与 MCP 的 indexed 路径（find / 纯清单 list）均受影响；违反 R10.3 完备召回（漏命中）与红线21（检查点签发了它无证据的证明）。direct 路径不受影响。

**必须修复**：

1. 补采 stat 的 rel 集合改用 `scan.bad`（`ops::scan_all` 同一次扫描已产出的坏文件清单，含不可读与不可解析两类）——`scan.records ∪ scan.bad` 恰等于扫描时刻的枚举全集，不引入任何扫描之后的磁盘状态；某坏文件的 metadata 失败（如已被删除）→ 缺席，与既有 `stat_snapshot` 语义一致。
2. 先补护栏回归再修实现：注入式维护器（模式同 `maintenance_race.rs`）在 `rebuild_with_entries` 之后、检查点发布之前**新增**一个文件，断言发布的检查点 entries 不含该文件（或读路径因枚举失配走 direct）；正路测试 `bad_records_same_tx_and_stat_covers_all_files` 保持全绿。
3. 复验时对 CLI 与 MCP 各做一次生产路径确认（新文件落盘后 indexed 应答不得漏）。

## 非阻断观察

| # | 级别 | 位置 | 内容 |
|---|---|---|---|
| L1 | 低 | `crates/mem-mcp/src/protocol.rs:535` | `) -> WriteContext<'a> {    let principal_label = …`——函数体首语句与 `{` 挤在同一行，rustfmt 必改形态（本机工具链未装 `cargo-fmt`，未能机器复跑，人工核验确定）。建议随 B2 修复同批 `cargo fmt` 全仓过一遍 |
| L2 | 低 | `crates/mem-index/src/fts.rs` | `Index::search`（单串 LIKE）设计明言"被 candidates 取代"，现无任何非测试调用者，未删除——留作死代码会误导后来者以为存在第二条检索路径 |
| L3 | 低 | `crates/mem-core/src/find.rs`（`candidate_types`） | `--type` 与 `--view` 同给时候选类型集取 view 独占、type_narrow 被忽略，而精筛是 view ∧ type 交集——若过滤字段只被 `--type` 指定的类型声明，会误报 `unknown_filter_field`。另注意这是相对 v1.1 的行为变化（旧版 view 在场完全忽略 type），变化方向合理（交集更保守）但未显式登记。建议：同给时候选集取交集，或显式拒绝该组合 |
| L4 | 观察 | `docs/specs/type-declaration.md` v1.1 | 规范显式把 `type: "integer"` 排除在 x-filter 支持形态外——integer 数值字段过滤将得到 `filter_shape_unsupported` 告警 + 运行时 `invalid_filter_value`。实施与规范一致、非缺陷；但 JSON Schema 用 integer 记数值很常见，提请用户确认是否扩展支持集（或在 conventions 层提示类型作者改用 `number`） |
| L5 | 观察 | `AGENTS.md` §2 | 登记行仍写 keyword-search-design"实施待启动"，与分支现状不符；主设计文档衔接（设计 §9 第 2 条：开放问题29 收口、查询文法/x-filter/单源是否立新 D#）仍挂账——随发布准备收口即可 |
| L6 | 观察 | 分支内容 | 本分支一并提交了 R11 提案与评审文档 6 份（`binary-content-*`、`agent-memory-requirements-review` 等，主仓库原为未跟踪文件）——与 R10 实施无关，合并回 main 时注意确认是否有意同批 |

## 验证记录

- `cargo test --workspace`：**213 项全部通过，0 失败**（含 `a8bc5be` 补强的 `read_mode=indexed` 显式断言——第二轮非阻断建议已落实）。
- `cargo check --workspace --all-targets`：通过，**零警告**。
- `git diff --check main...HEAD`：通过。
- rustfmt：本机工具链未安装 `cargo-fmt` 组件，未能机器核验（L1 以人工核验代替）。
- 首轮 B1（数值过滤精度）修复保持完好：`number_eq` 三分支精确实数比较 + `filter_number_precision_beyond_f64_safe_range` 回归在位。

## 待批准清单

**本轮请求批准**：B2 修复完成并通过护栏回归（新增文件不进检查点 entries）之前，不批准 R10 实施合入。

**本轮明确不批**：以 213 项测试全绿替代时序窗口验证；以"窗口概率低/自愈快"为由接受存在坏检查点路径；`enumerate_md` 重枚举作为 stat 补采来源的任何变体。
