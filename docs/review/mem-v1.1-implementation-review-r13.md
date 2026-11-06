# mem v1.1 实施评审——第十三轮报告（R13，R12 修复复验 + MCP 前端与 RDC 哨兵主动审查）

- 评审对象：R12 处置后的工作区（B7 三子项 + B8 哨兵排空修复）
- 评审日期：2026-11-06
- 评审环境：Windows 11，非提权用户进程；生产快路径后端为 RDC
- 评审方式：修复面生产复验（真实 CLI + stdio MCP）、CLI/MCP 全工具面对照读码、RDC 哨兵对抗实测
- 验证概况：`cargo check --workspace --all-targets` 零警告；`cargo test --workspace --no-fail-fast` 两遍无失败（第二遍逐套件统计 31 套件 170/170）

## 评审结论（TL;DR）

**新发现四项发布阻断：B9-1 / B9-2 / B9-3（MCP 前端续读与校验缺口）、B9-4（RDC 哨兵名子串过滤吞掉记录变更，红线21 新鲜性证明失效）。本轮不批准进入发布准备。**

R12 修复面本身复验全部通过：LF 族查询两执行器逐 ID 相等且符合内容期望、MCP 节点 resume 重放不重复、MCP show/brief 字节预算与续读生效、brief_bytes=1 双前端响亮拒收。B8 的哨兵排空修复在普通文件名记录上有效（全量测试含 maintenance_race 对抗回归，两遍通过）。

但 B9-4 表明哨兵机制引入了新的事件过滤漏洞：文件名含 `.rdc-sentinel` 子串的**记录文件**，其变更事件被当作哨兵信号而不计入未消化变更——新鲜性证明对该记录永久失效仍走 indexed。

## R12 修复面复验（生产二进制，真实 CLI/MCP）

| 场景 | R13 结果 |
|---|---|
| 字面 LF / 含 LF 多字符 / 跨分隔符子串 / 字面反斜杠 / 常规查询（5 组） | direct 与 indexed 逐 ID 相等，且与内容事实一致（LF 族均 0 命中，常规查询命中预期记录） |
| MCP trace 节点截断 resume 重放（trace_nodes=1，A→B→C） | 首页 [B] + resume{skip:1,limit:2}；原样重放返回 [C]、不含 B；第三页空页终止 |
| MCP show brief_bytes=16 | 205 字节正文截为 16 + resume.body_offset=16，续读页 16→32 可推进 |
| MCP brief brief_bytes=16 | items 0 + skipped_over_budget 4 条全列 + resume.offset=4 可执行 |
| brief_bytes=1 | CLI 非零退出；MCP 返回 JSON-RPC error（含"最小为 2"） |
| 标准全量测试 | 两遍无失败（170/170），R12 的间歇旧令牌未复现 |

---

## B9-1. MCP mem_trace：depth 低于全局预算的深度截断返回 resume:null

**位置**：`crates/mem-mcp/src/protocol.rs:218-243`；对照 CLI `crates/mem/src/commands/mod.rs:905-933`。

复现（默认预算 trace_depth=4，A→B→C 链）：

```text
MCP tools/call mem_trace {id:A, dir:up, depth:1}
  → nodes=[B], truncated=true, resume=null
CLI mem trace A --dir up --depth 1（同店同参数）
  → truncated=true, resume={reason:"深度预算截断", suggest:"mem trace A --dir up --depth 2（提高深度重跑）"}
```

原因：MCP 的 resume 构造只有两分支——`depth_limited && depth >= max_depth` 给 frontier、`truncated_by_nodes` 给 skip/limit；CLI 的第三分支（深度截断且 depth 未达预算上限 → 给 `--depth+1` 可执行建议）在 MCP 缺失，落入末尾 `Value::Null`。截断发生而续读路径为空，违反红线9；与 R12 B7-2 同族（"resume 非空"尚且不够，此处直接为 null）。

**关闭条件**：MCP 补齐该分支（与 CLI 同一判定顺序），真实 MCP 回归覆盖 depth<max_depth 的深度截断，断言 resume 携带可执行续读。

## B9-2. MCP mem_diff：changes_since 截断无续读路径

**位置**：`crates/mem-mcp/src/protocol.rs:301-311`；工具 schema `crates/mem-mcp/src/tools.rs:69-70`；对照 CLI `crates/mem/src/commands/mod.rs:1094-1132`。

复现（diff_since_limit=1，店内 4 组变更）：

```text
MCP mem_diff {since:0} → changes_since 1 组, changes_truncated=true,
                         应答键仅 {changes_since, changes_truncated, current_drift, drift_truncated}——无 resume
CLI mem diff --since 0 → changes_truncated=true, total_groups=4, resume={offset:1}
```

MCP 输入端也只接受 since（schema 无 offset），被截断的 changes_since 组经 MCP 永远无法续读。CLI 在 r4 B7 已落实组级 offset 续读，MCP 未同步。红线9 不因前端豁免（R12 B7-3 同标准）。

**关闭条件**：mem_diff 接受 offset、截断时输出 resume.offset（与 CLI 同语义）；真实 MCP 回归两页不重复不遗漏。

## B9-3. MCP mem_verify 缺镜像/计划/摘要审计——分裂场景误报 ok:true；r5 修复登记不实

**位置**：`crates/mem-mcp/src/protocol.rs:330-345`；对照 CLI `crates/mem/src/commands/mod.rs:1222-1265`。

代码对照：MCP mem_verify 只做 scan.warnings + chain_audit + dangling_refs；CLI 另有 `mirror_consistency_audit`（payload x-ref vs envelope.refs 双向 + payload 悬空）、`agenda::verify_report`（半成品计划）、`--deep` 摘要对照。MCP schema 声明 `deep` 参数（tools.rs:73-74），但实现不读取它——传 `deep:true` 后应答键不变、无 digest_mismatches。

生产复现（memory M 的 payload.topic=T 保留、外部手改删除 envelope.refs 镜像边）：

```text
CLI mem verify  → ok:false，payload_mirror_inconsistencies=["镜像缺失：… payload.topic=T 无 envelope 镜像"]
MCP mem_verify  → ok:true（同一店同一时刻；scanned=6, warnings=[], history_holes=[], dangling_refs=[]）
```

verify 是验收与对账工具，MCP 前端把已知分裂报成全绿，正是 r5-B4 的立法场景。另需如实指出：**r5 修复登记（mem-v1.1-fixes.md 第 80 行）写"镜像一致性审计接入 CLI/MCP verify"，与实现不符**——MCP 侧从未接入，与 R12 B7-3 的"登记宣称完成实际未落地"同型。

**关闭条件**：MCP mem_verify 接入同一组审计（镜像 + 计划 + deep 摘要），输出 payload_mirror_inconsistencies/plans/digest_mismatches 字段；用上述分裂场景做真实 MCP 回归（ok:false）；更正 r5 登记行。

## B9-4. RDC 哨兵名子串过滤吞掉记录变更——indexed 模式新内容漏命中（红线21）

**位置**：`crates/mem-core/src/platform/windows.rs:776-784`（`name.contains(".rdc-sentinel")`）；哨兵排空机制见同文件 655-676。

R12 的哨兵排空用"文件名含 `.rdc-sentinel` 子串"识别哨兵事件。RDC 事件的 name 是相对监视根的路径（含子目录），于是**任何文件名含该子串的记录文件**，其变更事件都被当成哨兵：sentinel_seen 递增、undigested 不递增。正常写路径无法生成这种文件名（唯一键成分校验拒绝 `.`），但**外部重命名/放置记录文件是设计明确承认必须覆盖的现实**（红线24/26 的枚举域与 drift 语义）。

对抗实测（临时店，topic 记录被外部重命名为 `records/topic/x.rdc-sentinel.md`，sidecar 收敛后）：

1. 同尺寸修改 title `T`→`U` 并按纳秒还原 mtime（刻意绕过 stat 条件，与 B4/B8 同手法）；
2. 第一次读取：`find U` → **indexed(rdc)，0 命中该记录**——磁盘真相已含 U（逐字节确认）；
3. `find T` → indexed，该记录不命中（r12 精筛按磁盘裁定，旧值被纠正——所以本缺陷的表现是**新内容漏命中**而非旧值返回）。

因果链闭合：事件被哨兵名吞掉 → fresh 查询 Some(0) → 判定 indexed → 粗筛用旧索引列，不含新内容 U → 漏命中。若事件未被吞，pending>0 会转直读而命中。是否随后续 tick 自愈未验证；不影响阻断判定——红线21 要求第一次读取就正确，且"不承认任何已知可能返回旧值/旧视图的快路径"。

**关闭条件**：哨兵事件判定改精确匹配（监视根下的 `.rdc-sentinel` 本身，而非任意路径的子串；注意 Windows 大小写）；`ignore` 的 contains 过滤（同函数 780 行，`.lock`/`.tmp-`）一并评审是否需收紧为前缀/精确规则。新增对抗回归：含哨兵子串文件名的记录 + 同尺寸 + mtime 纳秒还原 + 第一次读取，断言不吞事件（转直读或命中新内容）。

---

## 低优与待收口注记（不阻断，登记备查）

- **trace 节点续读需空页收尾**：trace_nodes=1 时第二页（limit=2 被钳回 1）返回最后节点仍 truncated=true，第三页空页才 truncated=false。续读可终止、不重复，红线9 满足；建议 truncated 按"是否还有未访问节点"判定以消除误报页。
- **body_slice 的 UTF-8 lossy 截断**（budget.rs:57-64）：切点落在多字节序列中时产生 U+FFFD，本页显示文本可能略超预算字节；续读偏移按原字节推进、可还原全文，既有 CLI 行为，不阻断。
- **哨兵写盘与红线18 的字面关系**：fresh_undigested 的哨兵追加发生在 sidecar 进程（维护/握手判定侧），CLI 读进程零写盘不变；但红线18 写"判定与直读全在内存"，哨兵写盘是判定机制的一部分，建议在设计侧补一句收口（参照 M6a/b 运行时区写的归档先例）。
- **空查询命中全部记录**（find ""）：两执行器一致（starts_with/contains 空串恒真），非等价性缺陷；是否应响亮拒收属产品语义，留开放问题，不阻断。

## 验证记录与清理

按实际顺序执行：

1. `cargo check --workspace --all-targets`：通过，零警告；
2. `cargo test --workspace --no-fail-fast` 两遍：均无失败（第二遍逐套件统计 31 套件、170/170；含 r12_regressions 与 maintenance_race 对抗回归）；
3. 生产复验均使用 `target/debug/mem.exe`；MCP 为真实 stdio JSON-RPC 会话；快路径为 RDC；对抗写入均为同尺寸 + mtime 纳秒还原；
4. 临时店与复验脚本（系统临时目录 r13store/r13sent 等）已全部删除；未修改任何实现代码与既有评审/登记文件；macOS、USN 提权分支（停用中）、真实断电仍未验证（继承既往轮次）。

## 自曝三处

1. **B9-1/B9-2 的"阻断"定级沿用"红线9 不因前端豁免"的历史标准**（R12 B7-2/B7-3 同型）。若裁决 MCP 允许更弱的续读表达（如仅标注 truncated、由用户自行提高预算），则需先改红线解释，再降级这两项。
2. **B9-3 的分裂用外部手改构造**，不是 merge 路径复现；但镜像审计正是为"payload 与信封分裂"这一现实设立，MCP 缺审计由代码对照与应答键集合双重证实，不依赖构造路径的代表性。
3. **B9-4 的进入条件依赖外部重命名为含哨兵子串的文件名**，正常 CLI/MCP 写路径不可达（slug 校验拒绝 `.`）。定阻断的依据是：外部重命名属设计承认的覆盖域，且进入该状态后该记录的每次内容变更都不可见——不是一次性竞态。若评审认为应降级，降级理由需回答"为什么该文件名的记录不配新鲜性保证"。

## 待批准清单

**本轮请求批准**：B9-1 / B9-2 / B9-3 / B9-4 四项发布阻断成立；全部关闭（含各自的真实 MCP / 对抗回归）后，才可进入发布准备。r5 登记行（MCP verify）需同步更正为实际状态。

**本轮明确不批**：把"两执行器结果一致"当作"命中正确"（B9-4 的 direct 对照才是真相）；以 CLI verify 的 ok:false 覆盖 MCP verify 的误报；把哨兵名 contains 过滤视为"只影响内部文件"；设计文档版本/修订行变更；打包或发布。
