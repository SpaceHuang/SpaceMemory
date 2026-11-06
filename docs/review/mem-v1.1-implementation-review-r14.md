# mem v1.1 实施评审——第十四轮报告（R14，R13 四项阻断修复重审）

- 评审对象：R13 处置后的工作区（B9-1/2/3/4 修复）
- 评审日期：2026-11-06
- 评审环境：Windows 11，非提权用户进程；生产快路径后端为 RDC
- 评审方式：修复读码、真实 CLI + stdio MCP 原始场景复跑、RDC 哨兵对抗实测（含普通文件名对照组）
- 验证概况：`cargo check --workspace --all-targets` 零警告；`cargo test --workspace --no-fail-fast` 连续两遍 175/175 全绿

## 评审结论（TL;DR）

**R13 的四项发布阻断（B9-1/2/3/4）全部关闭，本轮未发现新阻断。批准进入发布准备。**

每项均按 R13 的原始复现场景在生产二进制上重跑通过，不是仅凭登记或测试计数采信；修复引入的行为变化（ignore 过滤收紧、tmp 事件计入）经检查均落在安全侧。未实测边界（macOS 真机、USN 提权分支、真实断电）继承既往轮次，属外部验收项，不随本轮结论消除。

## 逐项复验（生产二进制，真实 CLI/MCP）

| 项 | R14 结果 | 证据 |
|---|---|---|
| B9-1 MCP trace 深度截断 resume:null | **关闭** | 默认预算 trace_depth=4、A→B→C：`mem_trace {id:A,depth:1}` → [B] + truncated + `resume{reason:"深度预算截断", depth:2, next}`；按 resume 重放 depth=2 → [B,C]、truncated=false、resume=null。`protocol.rs:220-254` 与 CLI 三分支完全同构 |
| B9-2 MCP diff 截断无续读 | **关闭** | diff_since_limit=1、4 组变更：逐页携带 resume.offset 续读，**4 页、4 组全部可达、逐 ID 无重复**，total_groups=4；schema 已补 offset；`protocol.rs:318-333` 与 CLI 同语义（全量取组再分页） |
| B9-3 MCP verify 误报全绿 | **关闭** | 同一分裂店（payload.topic 保留、envelope.refs 外部删除）：MCP `mem_verify` → **ok:false** + payload_mirror_inconsistencies/plans 均含"镜像缺失"，与 CLI 逐条同报；`deep:true` 真正被读取，应答携带 digest_mismatches。r5 登记行已在 fixes 档案中更正 |
| B9-4 RDC 哨兵名吞记录变更 | **关闭** | 外部重命名记录为 `records/topic/x.rdc-sentinel.md` + 同尺寸 title T→U + mtime 纳秒还原：第一次 `find U` → **direct（订阅存在未消化变更）且命中新内容**——事件不再被吞，转直读安全侧；普通文件名对照组同为 direct + 命中。`windows.rs:782` 哨兵改精确匹配（`eq_ignore_ascii_case`） |

R12 修复面抽验同步通过：trace 节点 resume 重放第二页到达 C 不重复 B；MCP show 205→16 字节 + body_offset 续读；brief 16 字节预算 items 0 + skipped 4 + resume.offset=4。

## 修复引入边界的检查

- **ignore 过滤收紧**（windows.rs:789-790）：改为末段组件后缀匹配，sidecar 订阅参数同步为 `[".lock"]`——机制锁文件仍忽略；`.tmp-` 中缀不再过滤，原子写 temp 事件计入未消化变更。方向是安全侧（多计数只多触发维护/直读），不会吞记录事件；性能影响属既有直读降级成本，非正确性问题。
- **mem_trace 分支顺序**：深度/节点截断的判定顺序与 CLI 逐行同构，未引入"双截断时原因错配"的新分叉。
- **mem_verify 审计组**：与 CLI 共用 `mirror_consistency_audit` / `agenda::verify_report` / `deep_digest_audit` 同一实现，ok 判定纳入全部审计——不是 MCP 侧另写一份的漂移面。
- **新增回归**：`crates/mem/tests/r13_regressions.rs` 4 则 + mem-core RDC 单测，覆盖上述全部场景；本轮生产复验与其结论一致。

## 验证记录与清理

1. `cargo check --workspace --all-targets`：通过，零警告；
2. `cargo test --workspace --no-fail-fast`：连续两遍 175/175（170 基线 + r13 回归 4 则 + RDC 单测 1 则）；
3. 生产复验使用 `target/debug/mem.exe`，MCP 为真实 stdio JSON-RPC；对抗写入为同尺寸 + mtime 纳秒还原；
4. 临时店与复验脚本已全部删除；本轮未修改任何实现代码；macOS、USN 提权分支（停用中）、真实断电仍未实测（继承）。

## 自曝三处

1. **B9-4 修复后的观测是"转直读"而非"indexed 下仍正确"**：事件计入后该记录走直读/再维护，关闭条件（不吞事件、第一次读取正确）已满足；但"哨兵子串名记录在 indexed 模式下长期正确"未单独观测，依赖普通路径的既有新鲜性协议。
2. **间歇性缺陷不能由两次通过证明根除**：R12 的 B8 在本轮两遍全量中未复现，与 R12 处置后的基线一致，但并行时序类问题理论上仍需更长时间窗观察——本轮不把它重新打开，也不宣称统计学证明。
3. **本轮是定向重审而非全量重评**：覆盖 R13 四场景 + R12 抽验 + 修复边界读码；其余历史面由 175 项自动化测试承担，未逐条重做既往所有生产复现。

## 待批准清单

**本轮请求批准**：B9-1/2/3/4 四项阻断关闭；项目进入发布准备（打包发布仍需按 `docs/rules/packaging.md` §2 落地清单逐项执行）。

**本轮明确不批**：把 macOS/USN/真实断电等外部验收项视为已验证；R13 登记的四条低优注记的产品语义变更（trace 空页收尾、body_slice lossy、空查询语义留待裁决；哨兵写盘与红线18 的字面关系属设计侧收口）；设计文档版本/修订行变更。
