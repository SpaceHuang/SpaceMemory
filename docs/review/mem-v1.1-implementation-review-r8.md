# mem v1.1 实施评审——第八轮报告（R8，R7 B7 修复重审）

- 评审对象：R7 唯一残留 B7 的本轮修复
- 评审日期：2026-11-05
- 评审环境：Windows 11，非提权用户进程；生产快路径后端为 RDC
- 评审方式：修复读码、生产 CLI 直读/快路径对照、跨模式续读、按返回建议实际重跑、完整构建测试
- 基线结果：`cargo check --workspace --all-targets` 零 Rust 警告；`cargo test --workspace` 的 **29 个测试套件、163 项测试全部通过**

## 评审结论（TL;DR）

**B7 仍未关闭，保留 1 项发布阻断。本轮不批准进入发布准备。**

字面 `%/_`、非 ASCII 大小写和同模式两页 ID 顺序已改善；64 字节 brief 也不再返回 173 字节完整首项。但生产复验仍发现：

1. find 的 NFC、ID、payload 键名命中面不等价，跨模式 resume 会漏掉剩余命中；
2. trace 深度建议仍被硬上限钳回，实际重跑仍只返回 B；
3. brief 把小于 64 的配置硬预算静默提高到 64，且 `bytes_used` 仍比实际 items 序列化大小多 1。

因此修复登记中“R7 三边界全部闭环”不成立。新增测试全绿，但没有覆盖这些失败条件。

## R7 场景复验

| 场景 | R8 结果 |
|---|---|
| 字面 `%` / `_`，direct/indexed | 两模式 ID 集一致，通过 |
| `Ärger` 以 `ärger` 查询 | 两模式各命中同一 ID，通过 |
| 组合字符 `Café` 以分解形式 `Cafe\u0301` 查询 | direct 1 条，indexed 0 条，失败 |
| 两条 `shared`，limit=1 各自续读两页 | 两模式页内 ID 顺序一致，通过；但 resume offset 分别为 4/1 |
| direct 第一页 resume 交给 indexed | 第二页为空，遗漏另一条命中，失败 |
| trace 硬上限 1，执行返回的 `--depth 2` 建议 | 仍只返回 B，并重复相同建议，失败 |
| brief 预算 64 | 首项实际 47 字节，未超预算且 offset 推进；`bytes_used=48`，记账不等 |
| brief 配置预算 16 | 输出擅自改报 `bytes_budget=64`，返回 47 字节首项，失败 |

---

## B7-1. find 仍有四种 direct/indexed 语义分歧

**位置**：`crates/mem-index/src/fts.rs:87-144`；折叠物化见 `crates/mem-index/src/sqlite.rs:40-42,299-316`；直读见 `crates/mem/src/commands/mod.rs:805-857`。

### 1. NFC 只在索引内容侧执行，查询侧与直读正文没有同一折叠

记录 payload 含组合形式 `Café`，查询分解形式 `Cafe\u0301`：

```text
direct:  1 条
indexed: 0 条
```

索引列使用 `paths::fold`（NFC + 小写），但 `crate_fold` 在 `fts.rs:142-144` 只有 `to_lowercase`。同时直读正文在 `commands/mod.rs:831-832` 也只转小写；因此不能只修查询函数，正文存储/查询的组合与分解形式矩阵也需对齐。

### 2. ID 中段在 indexed 命中、direct 不命中

直读在 `commands/mod.rs:829` 使用 `env.id.starts_with(query)`；索引在 `fts.rs:110` 使用前后都带 `%` 的 LIKE，实际是 ID 任意位置子串。生产查询某 ID 的中段 `1da3-`：

```text
direct:  0 条
indexed: 1 条
```

### 3. payload 键名进入索引，直读只查字符串值

`sqlite.rs:40-42,305-307` 对整个序列化 JSON 原文折叠，包含键名与结构字符；直读 `payload_contains` 只递归字符串值。查询公共键名 `title`：

```text
direct:  0 条
indexed: 10 条（本例全部含 title 键的记录）
```

这快路径返回了直读查询范围外记录，也与 `sqlite.rs:45-47`“键名不进索引”的既有注释冲突。

### 4. 同一分页的 resume offset 定义不同，跨模式续读漏命中

两条 `shared` 命中、中间夹有其他记录时：

```text
direct 第一页 resume:  {"offset":4}
indexed 第一页 resume: {"offset":1}
```

直读 offset 是全记录下标（`commands/mod.rs:827-855`），索引 offset 是命中行下标（SQL OFFSET）。虽然各自续读时两页 ID 相同，但将 direct 第一页的 `offset=4` 交给 indexed 后，生产返回 **0 条**，第二条 `shared` 被跳过。sidecar 在两次读取间启动/退出是正常状态变化，resume 不能因此失效。

**关闭条件**：查询、索引文本和直读正文使用完全相同的 NFC + 小写折叠；ID 统一为现有直读的前缀语义或同步修改公开契约；payload 只索引字符串值；offset 统一为命中序号或统一的稳定游标。新增组合/分解 Unicode 双向矩阵、ID 中段、payload 键名、两页逐 ID 与 direct→indexed / indexed→direct 交叉续读测试。

## B7-2. trace 深度 resume 仍不可执行

**位置**：`crates/mem/src/commands/mod.rs:895-922`，钳制点仍为 `:896`。

生产店配置 `trace_depth=1`，A→B→C、节点预算充足：

1. 第一次返回 B，`truncated:true`，建议 `--depth 2`；
2. 实际执行 `--depth 2`，仍被 `.min(max_depth)` 钳回 1；
3. 第二次仍只返回 B，并再次建议同一个 `--depth 2`，C 永远不可达。

新增 e2e 在 `crates/mem/tests/fastpath.rs:419-428` 使用默认硬上限 4，所以 depth 1→2 能通过；它没有设置评审复现的硬上限 1，注释“不得被钳回 1”与实际布置不符。

**关闭条件**：深度 resume 使用不会被同一预算钳制的 frontier/游标等真实续读机制。测试必须把 `trace_depth` 设为 1，实际执行返回的 resume，并断言读到 C。

## B7-3. brief 仍会突破配置硬预算，字节记账仍不准确

**位置**：`crates/mem/src/commands/mod.rs:498-555`。

`byte_budget = configured.max(64)` 不是配置校验，而是静默提高调用方预算。生产将合法解析的 `brief_bytes` 设为 16 后：

```text
配置预算：16
输出 bytes_budget：64
首项 items 实际序列化：47 字节
```

即仍返回超过配置硬预算的首项。若实现选择“最小可续读条目至少 64 字节”，应在配置入口明确拒收更小值，而不是改写上限后继续输出。

在预算 64 下，首项已降级为仅 ID，actual=47、offset 从 1 推进到 2，方向正确；但输出 `bytes_used=48`。原因是 `commands/mod.rs:503` 给首项也固定加 1 个逗号字节，fallback 在 `:525` 同样如此。测试在 `fastpath.rs:439-443` 使用 `used <= 64+32` 容差，既不检查实际 items 字节，也不要求记账相等，无法捕获该问题。

**关闭条件**：不得上调已接受的硬预算；无法容纳最小续读条目时明确拒绝配置或采用不突破预算的可续读表示。`bytes_used` 必须等于被记账的 items 数组实际序列化字节数。新增预算 16/64 两组测试，断言实际字节不超限、记账精确相等、游标持续推进。

---

## 验证覆盖与清理

- 已执行：`cargo check --workspace --all-targets && cargo test --workspace`，29 个套件、163/163 通过，零 Rust 警告。
- 所有失败均来自生产 `target/debug/mem.exe`；快路径实测 `via_subscription=rdc`，没有把 direct 自比当作 indexed 证据。
- 本轮未修改实现代码，未重开 B1/B3/B4 等已通过项；完整工作区回归保持通过。
- 临时 Python 脚本与临时店 `review_r8*` 均已删除；`target/tmp` 下既有历史目录未动。
- macOS 真机、真实断电、USN 提权分支仍未验证；USN 当前保持停用。

## 自曝三处

1. **ID 中段与 payload 键名是本轮扩大复验出的命中面分歧**，不是 R7 原始三个示例；但它们位于同一 `search` 实现，且修复登记宣称与直读完全等价，因此归入 B7，而非另立泛化需求。
2. **跨模式 resume 不是同模式两页测试的必检项**，但 sidecar 可正常启停，且 R7 关闭条件明确要求同一 offset 定义；当前游标跨模式漏数据，不能视为仅展示差异。
3. **brief 的 16 字节预算小于任何正常 UUID 条目**。评审不要求在 16 字节内魔法般塞入完整 ID；明确要求的是拒收不可满足的配置，或提供合法续读表示，而不是静默把硬上限提高到 64。

## 待批准清单

**本轮请求批准**：B7 继续作为唯一发布阻断；上述 find、trace、brief 边界全部固化回归并生产复验通过后，才可关闭 B7。

**本轮明确不批**：以默认硬上限代替 trace=1 复现、以容差代替字节准确断言、把 payload 键名搜索设为未声明的新语义、放宽或上调硬预算、设计版本/修订行变更、打包或发布。
