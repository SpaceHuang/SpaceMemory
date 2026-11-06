# mem v1.1 实施评审——第九轮报告（R9，R8 B7 修复重审）

- 评审对象：R8 唯一残留 B7 的本轮修复
- 评审日期：2026-11-06
- 评审环境：Windows 11，非提权用户进程；生产快路径后端为 RDC
- 评审方式：修复读码、生产 CLI 直读/快路径对照、双向 Unicode 与跨模式续读、真实 trace 重跑、完整构建测试
- 基线结果：`cargo check --workspace --all-targets` 零 Rust 警告；`cargo test --workspace` 的 **29 个测试套件、165 项测试全部通过**

## 评审结论（TL;DR）

**B7 仍未关闭，保留 1 项发布阻断。本轮不批准进入发布准备。**

本轮有效修复包括：payload 键名不再进入快路径命中面、ID 统一为前缀语义、`%/_` 与非 ASCII 大小写保持通过、brief 在 64 字节下的 `bytes_used` 已精确等于 47。trace 也不再返回必然被钳回的假建议。

但 R8 的完整失败契约尚未闭环：

1. find 的 NFC 查询与直读正文折叠仍不一致，双向跨模式 resume 分别漏命中和重复命中；
2. trace 只把假建议改成“调大全局预算后重跑”，仍没有红线9要求的续读路径；
3. brief 虽如实报告 16 字节预算，却仍返回 47 字节首项，硬预算依然被突破。

因此修复登记中“R8 全部闭环”不成立。

## R8 场景复验

| 场景 | R9 结果 |
|---|---|
| 字面 `%` / `_` | direct/indexed ID 集一致，保持通过 |
| `Ärger` 以 `ärger` 查询 | 两模式命中同一 ID，保持通过 |
| payload 键名 `title` | 两模式均 0 条，已修复 |
| ID 中段 / ID 前缀 | 中段均 0；前缀均命中同一 ID，已修复 |
| NFC `Café` 查询组合/分解 payload | 两模式集合一致，通过 |
| 分解查询 `Cafe\u0301` 查 NFC payload | direct 2 条，indexed 0 条，失败 |
| NFC 查询含 NFD 正文的记录 | direct 漏该正文记录，indexed 多 1 条，失败 |
| 两条 `shared` 各自同模式续读 | 两页 ID 顺序一致，通过 |
| direct→indexed 续读 | direct resume=5；indexed 第二页 0 条，漏命中 |
| indexed→direct 续读 | indexed resume=1；direct 重复返回第一页 ID |
| trace 硬上限 1 | 假 suggest 已移除；重跑仍只见 B，resume 无游标 |
| brief 预算 64 | actual=47、bytes_used=47、offset 推进，通过 |
| brief 预算 16 | bytes_budget=16、actual=47、bytes_used=47，突破硬预算 |

---

## B7-1. find 的 Unicode 折叠和跨模式游标仍未对齐

**位置**：`crates/mem-index/src/fts.rs:92-149`；直读见 `crates/mem/src/commands/mod.rs:806-858`。

### 1. 查询侧仍未做 NFC，直读正文也仍只转小写

`fts.rs:147-149` 的 `crate_fold` 仍是：

```rust
fn crate_fold(s: &str) -> String {
    s.to_lowercase()
}
```

索引内容使用 `paths::fold`（NFC + 小写），分解查询因而无法匹配已经 NFC 的索引列。生产对两条等价 payload（NFC/NFD 各一条）执行分解查询 `Cafe\u0301`：

```text
direct:  2 条
indexed: 0 条
```

反向执行 NFC 查询 `Café` 时，indexed 还命中一条 NFD 正文记录，而 direct 不命中：

```text
direct:  2 条（仅两条 payload）
indexed: 3 条（两条 payload + NFD 正文）
```

原因是 `commands/mod.rs:832-833` 的直读正文仍使用 `to_lowercase`，没有 `paths::fold`。需要同时修查询与直读正文，不能只改其中一侧。

新增测试没有覆盖这两个输入：`crates/mem-index/tests/index.rs:331-335` 只查询预组合的 `Ärger/ärger`；`crates/mem/tests/fastpath.rs:521-534` 同样只查询 `Ärger`，没有分解形式查询。

### 2. offset 仍是两个不同坐标系，双向跨模式续读均失败

索引 SQL OFFSET 是命中行序号；直读 `commands/mod.rs:828-856` 的 offset 是全记录下标。本轮生产第一页：

```text
direct resume:  {"offset":5}
indexed resume: {"offset":1}
```

两条 `shared` 各自同模式续读能得到相同第二页，但交叉执行时：

- direct 第一页 → indexed 使用 offset=5：返回 **0 条**，遗漏第二条；
- indexed 第一页 → direct 使用 offset=1：重复返回第一页 ID。

这说明仅比较两模式各自翻页不足以验证 resume；sidecar 启停导致模式切换时，游标必须保持同一语义。本轮没有修改该坐标差异，也没有新增交叉续读测试。

**关闭条件**：查询和直读正文都使用与索引内容相同的 NFC + 小写折叠；offset 统一为命中序号或稳定游标。回归至少覆盖 NFC/NFD 内容与查询的双向矩阵（payload 和正文）、direct→indexed 与 indexed→direct 两页逐 ID 对照，并断言无重复、无遗漏。

## B7-2. trace 假建议已移除，但仍无实际续读路径

**位置**：`crates/mem/src/commands/mod.rs:896-943`。

在 `trace_depth=1`、A→B→C 下，第一次和显式 `--depth 2` 重跑都只返回 B。现在 resume 如实给出：

```text
reason: 深度预算已用尽
note: 深度预算 trace_depth=1 已到底——续读需调大 mem.json budgets.trace_depth 后重跑
```

这修复了“建议一条必然无效的 CLI 命令”，值得确认；但 resume 没有 frontier、skip 或其他可执行游标，调用方必须修改全局硬预算并从头重跑才能读 C。红线9要求“预算截断必须显式标注并给出续读路径”，R4.2 验收要求“所有截断均有标注与续读路径”；修改预算提示不能代替剩余遍历的续读机制。

新增测试 `crates/mem/tests/fastpath.rs:593-607` 只断言没有 suggest、note 含“预算”，没有执行续读并断言读到 C，因此它固化的是提示文案，而非续读能力。

**关闭条件**：提供不修改全局预算即可执行的剩余遍历游标/路径，并实际执行读到 C。若项目要把“修改全局预算后重跑”重新定义为可接受续读，应另走设计/需求评审裁决，不能仅以实现登记替代。

## B7-3. brief 记账已准确，但 16 字节硬预算仍被突破

**位置**：`crates/mem/src/commands/mod.rs:499-535`。

预算 64 下，本轮 direct/indexed 均为：

```text
items 实际序列化：47 字节
bytes_used：47
bytes_budget：64
resume.offset：1
```

该项已修复。但预算 16 下，两种模式仍均为：

```text
items 实际序列化：47 字节
bytes_used：47
bytes_budget：16
```

`:510-525` 在完整项和精简项都放不下时，仍无条件保留只含 ID 的首项；没有任何 `used <= byte_budget` 检查。`.max(64)` 改成 `.max(1)` 只消除了配置值被谎报的问题，没有满足 R4.2 的硬预算。

新增测试 `fastpath.rs:536-555` 只断言 `bytes_budget=16`、`bytes_used` 等于实际长度和 offset 推进，恰恰缺少 `actual <= bytes_budget`，所以 47>16 仍全绿。

**关闭条件**：对无法容纳最小可续读项的预算在配置入口明确拒收，或采用不突破预算的可续读表示；不得返回超预算首项。16/64 两组回归都必须同时断言实际序列化字节不超限、`bytes_used` 精确相等、续读可推进。

---

## 验证覆盖与清理

- 已执行：`cargo check --workspace --all-targets && cargo test --workspace`，29 个套件、165/165 通过，零 Rust 警告。
- 所有失败均来自生产 `target/debug/mem.exe`；快路径实测 `via_subscription=rdc`，不是 direct 自比。
- 本轮未修改实现代码，未重开 B1/B3/B4 等已通过项；完整工作区回归保持通过。
- 临时脚本、正文输入和临时店 `review_r9*` 均已删除；`target/tmp` 下既有历史目录未动。
- macOS 真机、真实断电、USN 提权分支仍未验证；USN 当前保持停用。

## 自曝三处

1. **trace 的提示已比 R8 更诚实**。本轮阻断不再是指责假 suggest，而是指出“修改全局预算后重跑”与既有红线9续读路径不等价；若需求方明确接受这种降级，应由其裁决变更验收口径。
2. **16 字节无法容纳正常 UUID 首项**。关闭条件不是要求超预算输出或破坏 ID，而是明确拒收不可满足的配置，或提供其他合法续读表示。
3. **跨模式游标测试属于本轮新增对抗覆盖**。但 sidecar 启停是设计的正常路径，且 R8 已要求同一 offset 定义；本轮代码未改此处，因此该失败不是新增需求。

## 待批准清单

**本轮请求批准**：B7 继续作为唯一发布阻断；find 的 Unicode/双向游标、trace 的真实续读、brief 的实际字节上限全部有回归并通过后，才可关闭 B7。

**本轮明确不批**：把“调大全局预算”默认为 trace 续读、只验证预算数字不验证实际字节、只测预组合 Unicode 查询、只测同模式分页、设计版本/修订行变更、打包或发布。
