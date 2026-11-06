# mem v1.1 实施评审——第四轮报告（R4，全量复核）

- 评审对象：R3 及其后修复完成时的当前工作区代码；重点覆盖索引快路径、视图、写原子性、merge、预算与续读
- 评审日期：2026-11-05
- 评审环境：Windows 11，非提权用户进程，实际快路径后端为 RDC（ReadDirectoryChangesW）
- 评审方式：需求/设计/公开规范对照读码、`cargo check --workspace --all-targets`、`cargo test --workspace`、生产 CLI 对抗实测、临时 Rust 竞态复现测试
- 基线结果：编译检查零 Rust 警告；**28 个测试套件、151 项测试全部通过**。下列问题均未被现有测试面覆盖

## 评审结论（TL;DR）

**发现 7 项发布阻断（B1–B7），当前不应进入发布准备。**

其中 5 项已通过生产 CLI 或真实 `mem-index` 对抗复现：视图漏过滤并 panic、删除索引后返回空结果、维护竞态证明旧索引新鲜、merge 后引用与事件链同时损坏、FTS 分页重复第一页。另 2 项由确定代码路径确认：create 直接写正式文件、USN 订阅不更新文件身份；它们分别破坏崩溃原子性和 USN 分支的新鲜性证明。

“151 项全绿”不构成发布证据：现有 fastpath e2e 只覆盖正常物化与一次保 mtime 编辑，没有覆盖缓存被丢弃、复合视图、维护中二次写入、USN 文件替换及分页；merge 测试没有让 canonical/dup 自身参与引用重挂。

---

## B1. `mem view` 快路径绕过视图过滤，空类型视图直接 panic

**位置**：`crates/mem/src/commands/mod.rs:128-150`， panic 点为 `:139`。

**原因**：`type_only` 的布尔条件写反。当前逻辑在“存在 refs/principals/since，或类型数不等于 1”时进入索引分支；进入后只执行 `query_by_type`，完全丢弃其他过滤条件。`types=[]` 时又直接访问 `filter.types[0]`。

**生产复现（RDC 快路径）**：

1. 建 project P1/P2、task T1/T2，T1 属于 P1、T2 属于 P2；
2. 视图声明 `types=["task"]` 且 `refs=[project=P1]`；
3. `mem list --view <id>` 直读结果只有 T1，正确；`mem view <id>` 却返回 **T1+T2**，且标注 `read_mode=indexed`；
4. 对 `types=[]` 的合法视图执行 `mem view`，进程退出码 **101**，报错 `index out of bounds: the len is 0 but the index is 0`。

**影响**：视图不再表达其声明的信息范围，返回视图外记录；合法输入还能使读命令崩溃。违反 D10、R4.1、R6.2，且结果带有错误的“新鲜已证明”标注。

**修复验收**：只有“恰好一个类型且没有其他过滤条件”才允许走 `query_by_type`；其余视图直读。fastpath e2e 必须覆盖 refs/principal/since、多类型、空类型，并逐 ID 对比直读与快路径结果；任何合法视图不得 panic。

## B2. 丢弃 `index.sqlite` 后，读路径自建空库并把空结果当作真相返回

**位置**：`crates/mem/src/commands/mod.rs:227-246`；`crates/mem-index/src/sqlite.rs:64-103`。

**原因**：新鲜性判定只检查 records 检查点与 sidecar 握手，不检查索引是否存在及其物化代际。随后 `Index::open_store` 使用会创建文件的 `Connection::open`，并执行 `CREATE TABLE IF NOT EXISTS`。索引文件缺失时，这一步得到的是一个合法空库，查询成功，因此不会进入直读兜底。

**生产复现（RDC 快路径）**：

1. `mem list --type project` 返回 1 条记录，模式 `indexed`；
2. sidecar 保持运行，删除派生物 `index/index.sqlite`；
3. 立即再读，返回 `items: []`、`truncated: false`，仍为 `indexed`；
4. 被删除的 `index.sqlite` 已由这次**读命令**重新创建。

**影响**：用户按设计合法丢弃缓存后会持续看到空清单，而不是直读真相；同时读路径实际创建/修改 SQLite，违反红线 8、18 和 R4.1。

**修复验收**：读路径只以只读方式打开已存在索引，不得建库/建表/切换 journal mode；索引缺失、schema 不符或物化代际不匹配一律直读。保留 sidecar 运行并删除索引的 e2e，第一次读取必须返回完整真相且索引文件不得由读命令重建。

## B3. 维护完成后未重查订阅，维护期间二次写入可被检查点“提前确认”

**位置**：`crates/mem-core/src/sidecar.rs:288-299`、`:335-358`。

**原因**：握手先以 `fresh_undigested()` 消化信号，再执行 `run_maintenance`。维护器先重建索引，检查点随后重新枚举磁盘 stat；只要外部写入发生在“索引扫描之后、检查点快照之前”，检查点就会包含新文件状态，而索引仍是旧内容。代码在维护成功后直接返回 `Some(0)`，没有再次查询维护期间新到的订阅事件。

**确定性对抗复现**：临时 Rust 测试使用与生产 `mem/src/serve.rs` 相同的真实 `Index::rebuild` 物化器，在每次重建后执行同尺寸写入并恢复 mtime。结果：

```text
mode=Indexed { via_subscription: "rdc" }
indexed_digest=8ff94fb102197543ed2ae08fcd5d3f44b450d71461a674b9d16a2a2d2b143c82
fresh_digest=41d6e93bbaedc97b7289a7eec5120bc93012561e9a1e2465fb2a019eebb72a63
```

即 Hello 宣称无未消化变更、stat 检查点也一致，但索引摘要与磁盘当前摘要不同。

**影响**：红线 21 的“第一次读取不返回旧值”在真实并发窗口内失效；这是错误数据应答，不只是性能或标注问题。

**修复验收**：维护后、应答前必须重新查询订阅；出现新事件时继续维护至收敛，或有界尝试后如实返回未消化并转直读。检查点只能证明与索引同一代的扫描结果。回归测试需在“索引重建后、检查点写入前”注入保 stat 修改，断言第一次 CLI 读取即返回新摘要。

## B4. canonical 自身引用 dup 时，`merge` 成功后立即留下断链和事件链空洞

**位置**：`crates/mem-core/src/ops.rs:817-825`、`:830-879`、`:882-906`。

**原因**：`fresh_can` 在引用重挂循环之前读取。循环会把 canonical 自身也当作普通依赖者写入一次；随后别名移交仍使用循环前的 `fresh_can`，以相同的新版本再次覆盖 canonical，把刚完成的引用重挂丢掉，并追加重复 `old_v` 的事件。dup 自身参与重挂时也有同类陈旧快照问题。

**生产复现**：

1. canonical C 经 `mem link C D --rel derived_from` 引用 dup D；
2. 携带当前双令牌执行 `mem merge D C`，命令成功；
3. C 的 refs 仍指向已进入 attic 的 D；
4. 立即 `mem verify` 得 `ok:false`：一条 dangling ref，以及“期望 old_v=3、实际 old_v=Some(2)”的 history hole。

**影响**：一次成功的维护操作同时破坏 R3.4 引用完整性和事件历史可审计性；调用方只有主动再跑 verify 才能发现。

**修复验收**：canonical/dup 不得再走普通依赖者的陈旧快照路径；每步基于最新锁内状态 CAS，事件版本链连续。新增 canonical→dup、dup→canonical、第三方→dup 三类回归，merge 返回后 verify 必须立即全绿。连带修正 `:856-863`：payload 只能重挂声明为 x-ref 的字段，不能替换所有恰好等于 dup ID 的普通字符串。

## B5. create 直接写正式记录文件，不具备崩溃原子性

**位置**：`crates/mem-core/src/ops.rs:301-342`；`crates/mem-core/src/platform/windows.rs:227-233`；`crates/mem-core/src/platform/macos.rs:166-172`。

**原因**：add 的正式发布调用 `create_new(target, bytes)`；两个平台实现都直接对最终 `target` 执行 `create_new + write_all + sync_all`，没有经过临时文件和原子发布。

**影响**：写入进行中，其他读者可以枚举到不完整的正式 `.md`；进程崩溃或短写会在规范路径留下半条记录。后续 add 看到路径已占用，再调用 `get` 解析坏文件并失败，连既有记录输者协议也无法执行。这直接违反 R3.1、红线 6，以及规范 §5.3 的 temp+原子改名写路径。

**依据边界**：代码路径确定，未做真实断电/崩溃注入；但“直接写正式路径”与规定的原子发布机制相反，不依赖概率性复现即可判定。

**修复验收**：完整字节先写同目录临时文件并同步，再以“不覆盖既有文件”的原子发布语义进入规范路径，保留 create 唯一性。注入写入中断和并发扫描，任何时刻正式路径只能是完整旧记录或完整新记录；崩溃残留不得阻塞后续 create/fix。

## B6. USN 的 FRN 集合只在启动时快照，新记录和原子替换后的记录失去订阅覆盖

**位置**：`crates/mem-core/src/sidecar.rs:258-267`；`crates/mem-core/src/platform/windows.rs:377-443`；原子替换见 `:185-224`。

**原因**：USN watcher 只把 attach 时存在的记录 FRN 放进 `frn_set`。虽然提供了 `track()`，全工作区没有任何调用。新创建记录不会进入集合；`MoveFileExW(REPLACE_EXISTING)` 以临时文件替换正式文件后，当前文件身份也不再等于启动时的 FRN。维护循环没有重建或增量更新该集合。

**失效场景**：在提权 USN 分支中，新建一条记录或经机制原子替换一条记录，等待索引与检查点推进；随后对它做同尺寸、保 mtime 外部修改。stat 一致，USN 流中的当前 FRN 又不在旧集合内，握手可得 0，旧索引因此被判定为新鲜。

**影响**：USN 是公开声明的两种有效订阅后端之一，该分支不满足红线 21。RDC 实测通过不能覆盖 USN 的独立正确性。

**依据边界**：未在提权环境运行，FRN 更新缺失由调用关系与 Windows 文件替换语义确认；发布前必须补提权验收，不能以本机 RDC 结果替代。

**修复验收**：每次物化在检查点发布前重新建立与当前记录集合一致的 FRN 集合并处理 USN 游标竞态；在修复前可保守禁用 USN、转 RDC/直读。提权 e2e 覆盖“新建后保 stat 修改”和“原子替换后保 stat 修改”，第一次读取均须返回新值。

## B7. 硬预算与续读契约未闭环；FTS 的 offset 被忽略且没有 resume

**位置**：`crates/mem/src/commands/mod.rs:679-730`；`crates/mem-index/src/fts.rs:60-87`。同类缺口见 `cmd_diff`（`commands/mod.rs:931-954`）、`cmd_trace`（`:748-772`）、`cmd_show`（`:734-745`）；`brief_bytes` 仅有定义和未使用的 getter（`crates/mem-core/src/budget.rs:28-31`）。

**生产复现**：sidecar 快路径下，两条 memory 都匹配 `shared`：

- `mem find shared --limit 1 --offset 0` 返回第一条，`truncated:true`，无 `resume`；
- `mem find shared --limit 1 --offset 1` 仍返回**同一条**。

CLI 接收了 offset，但 FTS 分支没有传递；FTS SQL 也只有 LIMIT、没有 OFFSET。直读分支的 H5 修复没有覆盖快路径。

**同项缺口**：diff 和 trace 可报告截断却不给续读参数；show 可返回任意大小正文；`brief_bytes` 没有参与任何输出控制。这些路径不能满足 R4.2“每次访问有确定上限，截断附续读方式”和红线 9。

**修复验收**：FTS 支持稳定排序与 offset/游标，输出 resume，两个模式逐条对齐；diff/trace 给出可执行的续读方式；show/brief 落实字节预算及溢出标注。新增“匹配数大于 limit”的快路径 e2e，验证第二页不重复、不遗漏。

---

## 高优遗留（不另计入 7 项阻断）

### H1. 按路径 `get` 的记录与令牌来自两次磁盘读取

`crates/mem-core/src/ops.rs:83-95` 第一次读取并解析 Record，第二次重新读取计算 Token。外部写入落在两次读取之间时，返回值可能由“版本 A 的 payload/body + 版本 B 的令牌”组成。调用方若按 A 构造更新却携带 B 的令牌，锁内 CAS 会对 B 成功，从而覆盖 B。应改为一次读取同时生成 Record 与 Token，并增加路径寻址的并发回归。

### H2. CLI/MCP `reconcile` 只推进检查点，没有执行其宣称的索引刷新

CLI `crates/mem/src/commands/mod.rs:554-577` 与 MCP `crates/mem-mcp/src/protocol.rs:239-245` 都只执行 backfill 和 checkpoint 保存，没有 `Index::rebuild`/FTS 同步。通常 sidecar 后续维护会补偿，因此单列为高优；但在订阅信号缺失或与 B2/B3 叠加时，检查点推进会扩大旧索引被误信的窗口。修复时应统一一个“物化索引→确认无新变更→发布检查点”的维护原语，CLI、MCP、sidecar 不得各自实现不同顺序。

---

## 验证覆盖与边界

- 已执行：`cargo check --workspace --all-targets`；`cargo test --workspace`，151/151 通过。
- 已实测：RDC 快路径、视图过滤与空类型 panic、索引删除、FTS 分页、merge 后 verify、真实 mem-index 维护竞态。
- 未实测：提权 USN 分支、macOS、真实断电/崩溃。对应结论已按代码证据与运行证据分开标注。
- 临时复现脚本与测试仅用于本轮评审，不作为项目测试面交付；正式修复必须先把上述场景固化为回归测试。

## 自曝三处

1. **B5 定级依赖规范硬约束而非崩溃实测**。若项目明确允许 create 暴露半文件，则它可降级；但当前 R3.1/红线 6 与规范均作相反承诺，因此本轮按阻断处理。
2. **B6 尚缺提权运行证据**。若 Windows 的目标卷实现被证明在原子替换后仍保留同一 FRN，则“替换后失覆盖”一半需重估；“新记录从未 track”仍独立成立。
3. **B3 的写入由测试维护回调注入**。它不是自然时序压测，但注入点正是生产维护器重建索引之后、核心写检查点之前，且使用真实 RDC、真实 SQLite 和真实握手，因此证明的是生产顺序允许的竞态，不是 mock 查询结果。

## 待批准清单

**本轮请求批准**：B1–B7 的发现与发布阻断定级；H1/H2 的遗留定级；在全部阻断修复并补上对应回归前，不进入发布准备。

**本轮明确不批**：具体修复方案选型、设计文档版本/修订行变更、USN 是否永久撤下、打包或发布。修复实现需另行评审；尤其 B3/B6 需要重新审查“索引物化、订阅游标、检查点发布”三者的顺序证明。
