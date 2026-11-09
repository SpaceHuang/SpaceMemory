# Agent 记忆与状态层——设计文档（骨架）

- 版本：v1.3（2026-11-10）
- 修订：v1.3 R10 关键词检索与载荷字段过滤收编（2026-11-10，用户裁决：检索切片记入开放问题29 + 三决策立 D#；实施于 r10-keyword-search 分支，实施评审四轮通过）。**检索成为一等能力**——query 按 Unicode 空白切成词项（含全角空格），多词项按"与"组合命中，`-前缀` 表示排除该词；空查询与纯否定显式拒收（绝不返回空清单或全库清单冒充结果）；正文域要求合法 UTF-8，非合法则该域退出匹配——有损解码的伪命中形态废止（→ **D20**）。**载荷字段可过滤**——类型作者在 payload 属性上写 `x-filter: true` 声明"此字段可作过滤条件"，取值形态由属性本体的 `type` 承载（string/number/boolean/标量数组四类；`integer` 经 2026-11-10 裁决维持排除，需过滤的数值字段请声明 `number`）；过滤值是"字段＋文本"，相等按声明解释：文本折叠等值（大小写与等价字符形式归一）、数值精确实等值（"1"="1.0"，超过 2^53 的整数与浮点不经 f64 折叠互认）、布尔仅认字面、数组任一元素等值即命中；CLI `--filter 字段=值` 与 MCP `filters`（值一律 JSON 字符串）合法条件空间逐字相同；字段未声明、值不可解释、字段重复分别显式报错（4 枚机判 kind，两前端同码）（→ **D21**）。**执行单源 + 证明式索引注入**——解析、过滤校验、精筛、命中序分页、坏记录告警单源在 mem-core，CLI 与 MCP 同源调用（终结"两套匹配实现必然漂移"）；索引仅在 `open_proved` 证明新鲜时服务读（stat 一致 ∧ 只读打开 ∧ schema 门禁含坏记录表 ∧ epoch 同代核对），否则直读并如实标注；检查点 stat 集**恰好等于物化那一次扫描看到的文件集合**（含坏文件），绝不重新枚举磁盘——重枚举会让扫描后新落盘的文件被"提前确认"，消掉直读信号、indexed 漏命中（评审 B2 立法）（→ **D22**）。**坏记录不静默**——find/list 响应透出坏记录告警（位置、原因、修复入口三要素；rel 升序、上限 200、超限标注 + verify 续读），indexed 经 bad_records 表与 direct 同序同子集。**开放问题 29 收窄**：检索切片（mem_find 工具 14→15、文法、词汇、单源）已交付，剩余 = 其余工具映射盘点 + 性能基线。规范载体：type-declaration.md v1.1（x-filter 行；KEYWORD_VOCAB_VERSION 1→2）。评审：`docs/review/keyword-search-implementation-review.md`（四轮；B1 数值精度、B2 检查点重枚举均修复关闭）。
- 修订：v1.2 实施评审修复（响应 `docs/review/mem-v1.1-implementation-review.md`，149 项测试全绿零警告）——**B1** 撕裂尾行：两追加路径统一"截断到最后完整合法行界"（`ReadLog.good_bytes`），坏行带不带换行都被移除；中部坏行保持响亮；规范 §4.2 同步 v1.0.1（"补 
"表述废止）。**B2** fix 正文切分改用首个闭合 `---`（与 D18 同一边界，`record::find_body_after_first_delimiter` 单源）。**B3 裁决＝补接线**（用户评审裁决项，采方案 a）：sidecar 注入 mem-index 物化器（索引先落盘、检查点最后＝崩溃安全序），`Hello.undigested` 携带订阅消化状态，`freshness::consult_with_hello` 以"握手 ∧ stat 检查点"判定快路径，CLI list/brief/find（FTS）/view 类型独占过滤走索引快路径、其余直读并如实标注；e2e 测试（fastpath.rs）从生产二进制验证全链。高优六项：H1 fix 锁内读+可解析路径强制 expect CAS（红线19）；H2 delete/restore/merge 提交双侧路径（HEAD 与真相层不再分叉）；H3 `--deep` 审计改为**磁盘 vs 事件日志**对照（自比自废止）；H4 apply 计划级独占门闩（locked 变体，D17 全字面）；H5 find 截断标注+resume（红线9）；H6 跨主机锁持有者视为存活永不抢+判死退避。中低优：M2 init 建 git 基座、M4 MCP payload 必填+body_b64 字节保真、M6c temp 名不占 .md、M7 aliases 非串响亮、M8 AGENTS.md 刷新、M9 零警告、M10 截断 off-by-one、L1 键序守卫测试、L2/L3 清理、L6 位置变化标注。**实施裁决归档**：M1（MCP initialize 只握手，台账建立必须经 mem_session_start 显式提供 principal——身份不可从协议噪声推断，红线25 优先于 D13.1 的"MCP initialize 触发器"字面）；M3（x-ref/DAG 写路径校验为"锁外尽力+锁内复核+verify 审计兜底"，非全序强制——如实注记）；M5/M11（回填/漂移 O(候选×事件) 记入开放问题29 性能基线）；L4（envelope.commit 未接线 → 开放问题41）。
- 修订：v1.1 实施记录（TDD 全程：先测后码，§8 第 1–8 步全部落地）——**格式规范 v1 三件套定稿**（docs/specs/：envelope-format / type-declaration / conventions-source；收口开放问题 40 配置定名 `init.template_dir`、问题 25 默认预算、问题 37 默认空闲阈值）；**核心库全验收面通过**（freshness 三场景×三条件第一次读取返回新值；回填四注入+外部删除/重命名/删除后重建/只读外部删除；absent 稳定态两次 reconcile+两次 diff 收敛；回填并发三态：比较后暂停→update→恢复不追加过时事件、双并发不重复、三方交错不错序；增量两轮 diff——外部 git 提交出现、无 .git 成立）；**实施中定案三条**：(1) 回填状态比较含**路径**——路径变化=移动，产生 backfill 事件更新 ID↔路径映射（B1 path 字段的语义收口）；(2) link 只在**源记录**加边（目标侧仅令牌 CAS 校验不写——双向落边会凭空成环）；(3) Windows USN 订阅采用 **FRN 快照法**（物化时记录记录文件 File Reference Number 集合，journal 流命中即未消化变更）；实施偏离一处：MCP v1 为手写最小 stdio JSON-RPC（协议面与 CLI 同构），rmcp 装配推迟到工具面定稿（开放问题29）；FSEvents 未接（macOS 永久直读，安全侧，红线21 不破）。v1.0 关闭最后两个格式前置——**D18 信封物理格式**：单文件 Markdown，JSON frontmatter（严格分隔符；`envelope`/`payload` 两块使所有权字面可见；机制确定性再生成 frontmatter、**正文按字节保持不变**，红线27）+ Markdown 正文；**D19 类型声明格式**：JSON Schema 2020-12 + 自定义关键字词汇表（x-grade/x-unique-key/x-ref/x-dag/x-topics——词汇表属基础设施，类型仍是纯数据）+ 键字符集规则（NFC、casefold 路径推导、黑名单、保留名）。开放问题 23/24 关闭，形态定稿，进入实施。v0.9 响应 `docs/review/agent-memory-design-v0.8-review.md`——**B1**：**absent 纳入统一状态机**——记录状态定义为三元组 `(location ∈ {records, attic, absent}, version, digest)`，`external_delete` 的事件后状态即 absent；`last == cur` 覆盖 absent==absent → 同一次删除只回填一次、current_drift 不再持续出现（稳定态）；attic 检查按三元组进行、不绕过 last==cur；**B2**：**缺失记录使用同规范路径记录锁**——锁文件独立于记录文件存在，回填在锁内重查 records/ 与 attic/ 复算当前状态（含复查是否已被重建/恢复），再于序列锁内复核追加——external_delete 与同 ID 重建的竞态由同一把锁排除。v0.8 响应 `docs/review/agent-memory-design-v0.7-review.md`——**B1**：漂移与回填的枚举域从"现存 records/"改为**当前记录集合 ∪ 事件日志历史集合**；事件行增 `path` 字段（ID↔路径映射）；外部删除/重命名产生 `external_delete` 回填事件（`new_digest=null`），与受管 tombstone（attic/ 中、有受管删除事件）明确区分；**B2**：回填纳入统一锁序——记录锁持有期间完成"锁内重读当前状态 → 序列锁内复核最后事件 → 追加"，**禁止锁外旧快照直接追加**，并发回填由记录锁串行化、二次复核去重；漂移项命名 **`current_drift`** 并受独立分页预算。高优先级三项：D3 git 提交路径显式含 `events.jsonl`（commit-pending 重试同）；直读复杂度如实修正为 **O(库规模)**；current_drift 不混入 changes_since_N。v0.7 响应 `docs/review/agent-memory-design-v0.6-review.md`——**B1**：freshness 从"stat 快判 + 盲区补偿"改为**证明式协议**：索引仅在新鲜性可证明（stat 一致 ∧ OS 变更日志订阅覆盖检查点以来无缺口）时服务读请求；否则一律直读真相层（内存、零写盘）——"同尺寸保 mtime"盲区结构性消除，任何形态下第一次读取不可能返回旧值；**B2**：事件行增新旧内容摘要；回填判据从版本号改为**状态覆盖比较**（该记录最后事件状态 ≠ 当前文件状态即回填）——v1 创建、保版本手改、接口崩溃缺口全覆盖；链空洞由 verify 显式报告、不伪造；**B3**：只读增量废弃 worktree 并集，改为**日志过滤 ∪ 漂移扫描**（内存对比当前状态 vs 最后事件状态；与 git 无关，覆盖外部提交与无 .git；未登记项明确标注"backfill 待执行"）；事件日志随写例程同批提交。高优先级四项：令牌摘要升级为**全 SHA-256**；删除 task_ref 跨任务回退；session start 幂等键纳入 task_ref；事件日志 git 策略定稿。
- 修订：v1.0 关闭最后两个格式前置——**D18 信封物理格式**：单文件 Markdown，JSON frontmatter（严格分隔符；`envelope`/`payload` 两块使所有权字面可见；机制确定性再生成 frontmatter、**正文按字节保持不变**，红线27）+ Markdown 正文；**D19 类型声明格式**：JSON Schema 2020-12 + 自定义关键字词汇表（x-grade/x-unique-key/x-ref/x-dag/x-topics——词汇表属基础设施，类型仍是纯数据）+ 键字符集规则（NFC、casefold 路径推导、黑名单、保留名）。开放问题 23/24 关闭，形态定稿，进入实施。v0.9 响应 `docs/review/agent-memory-design-v0.8-review.md`——**B1**：**absent 纳入统一状态机**——记录状态定义为三元组 `(location ∈ {records, attic, absent}, version, digest)`，`external_delete` 的事件后状态即 absent；`last == cur` 覆盖 absent==absent → 同一次删除只回填一次、current_drift 不再持续出现（稳定态）；attic 检查按三元组进行、不绕过 last==cur；**B2**：**缺失记录使用同规范路径记录锁**——锁文件独立于记录文件存在，回填在锁内重查 records/ 与 attic/ 复算当前状态（含复查是否已被重建/恢复），再于序列锁内复核追加——external_delete 与同 ID 重建的竞态由同一把锁排除。v0.8 响应 `docs/review/agent-memory-design-v0.7-review.md`——**B1**：漂移与回填的枚举域从"现存 records/"改为**当前记录集合 ∪ 事件日志历史集合**；事件行增 `path` 字段（ID↔路径映射）；外部删除/重命名产生 `external_delete` 回填事件（`new_digest=null`），与受管 tombstone（attic/ 中、有受管删除事件）明确区分；**B2**：回填纳入统一锁序——记录锁持有期间完成"锁内重读当前状态 → 序列锁内复核最后事件 → 追加"，**禁止锁外旧快照直接追加**，并发回填由记录锁串行化、二次复核去重；漂移项命名 **`current_drift`** 并受独立分页预算。高优先级三项：D3 git 提交路径显式含 `events.jsonl`（commit-pending 重试同）；直读复杂度如实修正为 **O(库规模)**；current_drift 不混入 changes_since_N。v0.7 响应 `docs/review/agent-memory-design-v0.6-review.md`——**B1**：freshness 从"stat 快判 + 盲区补偿"改为**证明式协议**：索引仅在新鲜性可证明（stat 一致 ∧ OS 变更日志订阅覆盖检查点以来无缺口）时服务读请求；否则一律直读真相层（内存、零写盘）——"同尺寸保 mtime"盲区结构性消除，任何形态下第一次读取不可能返回旧值；**B2**：事件行增新旧内容摘要；回填判据从版本号改为**状态覆盖比较**（该记录最后事件状态 ≠ 当前文件状态即回填）——v1 创建、保版本手改、接口崩溃缺口全覆盖；链空洞由 verify 显式报告、不伪造；**B3**：只读增量废弃 worktree 并集，改为**日志过滤 ∪ 漂移扫描**（内存对比当前状态 vs 最后事件状态；与 git 无关，覆盖外部提交与无 .git；未登记项明确标注"backfill 待执行"）；事件日志随写例程同批提交。高优先级四项：令牌摘要升级为**全 SHA-256**；删除 task_ref 跨任务回退；session start 幂等键纳入 task_ref；事件日志 git 策略定稿。
- 起源与评审标尺：`agent-memory-requirements.md` v1.2；评审记录：`docs/review/agent-memory-design-v0.8-review.md`（2026-11-04）及此前四轮；**v0.9 复审通过（2026-11-04，零阻断，第 4–5 步解锁）**
- 状态：**已实施（v1.1，2026-11-05）**——§8 第 1–8 步全部完成（TDD，全工作区测试绿）：
  1. ✅ 信封与目录格式规范 v1（docs/specs/envelope-format.md）
  2. ✅ 类型声明两件套规范 + task 试金石（docs/specs/type-declaration.md + assets/builtin-types/）
  3. ✅ 自举类型 principal/view（经类型声明引入，红线13）
  4. ✅ 核心库（mem-core：门闩/令牌 CAS/事件日志+状态覆盖回填/证明式新鲜性+USN 订阅/漂移扫描/git 同批提交；验收面全过）
  5. ✅ CLI 前端 + brief/台账闭环（mem 二进制，clap 命令面 + e2e）
  6. ✅ sidecar + 索引 + 订阅（mem-index sqlite/FTS/检查点 + mem serve 单实例握手/空闲自退/USN；**v1.2 已接线**：serve 注入索引物化器、读路径经 consult_with_hello 快路径判定，e2e 验证 fastpath.rs）
  7. ✅ 宿主适配（mem mcp stdio 前端 + adapters/zcode + adapters/generic）
  8. ✅ 维护与提炼例程（agenda 提案扫描/apply 计划级独占门闩执行/executing 检出/fix 双通道+expect CAS）

**评审状态（2026-11-05）**：v1.1 实施评审（`docs/review/mem-v1.1-implementation-review.md`）发现 B1–B3 + H1–H6 + M/L 共 20 项；v1.2 修复回应全部阻断与高优项及可顺手的中低优项（细节见修订行），余项归档：M1（MCP initialize 裁决）、M3（校验强度注记）、M5/M11（性能，开放问题29）、L4（envelope.commit，开放问题41）、L5（留档）。
- 说明：术语沿用需求文档。**记号约定**：`task_abc` 等为缩短代号；记录唯一身份 = uuid v7；修订令牌 = `v<版本>@<完整 SHA-256 十六进制>`。

---

## 1. 形态总陈述

> **第二记忆 = 用户级本地数据目录（唯一真相）+ 一个 Rust 多调用二进制 `mem`（机制层：CLI / sidecar / MCP server 三前端合一）+ 随仓库走的类型声明与约定（规格资产）+ 各宿主可选的薄适配。**

身份、主题、项目、视图是**店里的第一类声明式数据**（D9/D10）。**协作状态的真相在会话台账与事件日志，Agent 上下文永远是缓存**（D13/D14）。并发与正确性铁律：**读路径零写盘**（18）、**写有全摘要令牌 CAS**（19）、**屏障必须持有**（20）、**索引只在新鲜性可证明时服务读，否则直读**（21）、**序号由全库串行资源分配**（24）、**接手是显式建立操作**（25）、**增量 = 日志 ∪ 漂移扫描**（26）。

三句排除：

- 不是网络服务、不是常驻守护系统——数据以开放格式文件存续，基础设施无守护义务；
- 不是各工具内置记忆、不是每宿主一套实现——机制层唯一，宿主适配是薄壳；
- 不是"目录 + 提示词纪律"——机器可判定规则（R7-a）全部由 `mem` 机械强制，判断型行为（R7-b）由约定教学交付。

### 1.1 分层

| 层 | 内容 | 归属 |
|---|---|---|
| 数据层（真相） | 记录文件（信封+载荷）、类型声明（含 principal/view 自举类型）、约定文档、会话台账、事件日志、agenda、.git | 用户资产 |
| 机制层 | `mem` 二进制：门闩/锁/令牌 CAS/事件日志/新鲜性证明、校验、信封管理、预算、引用与派生维护、台账、agenda 产出 | 本设计新建 |
| 索引缓存 | index.sqlite（派生、可丢弃；**仅在新鲜性可证明时服务读，否则直读回退；读路径零写盘**） | 附属物 |
| 宿主适配层 | Skill / AGENTS.md 注入 / MCP 配置（含 principal 自我声明）/ hook 注入 / 裸文件访问 | 每宿主一份薄壳 |

### 1.2 架构示意

```
        宿主（codex / zcode / DSH / 任意 Agent 环境）
        hook 注入(机械)  Skill·AGENTS.md(教学)  MCP(协议+principal)  bash(裸访问)
              │                │ 指针/约定           │ stdio            │
              ▼                ▼                    ▼                 ▼
        ┌──────────────────────────────────────────┐
        │        mem（multi-call binary）           │
        │   CLI 子命令  │  serve（sidecar） │  mcp   │
        │        └────────┼─────────┘               │
        │            机制核心库                       │
        │ 门闩·锁·令牌CAS·事件日志·新鲜性证明·台账·agenda│
        └───────────┬───────────────┬──────────────┘
                    │               │
     ┌──────────────▼─────┐   ┌─────▼──────────────┐
     │ 数据目录（唯一真相）    │   │ index.sqlite        │
     │ records/ types/     │   │ 派生缓存：               │
     │ sessions/ events    │   │ 新鲜性可证明→服务读；     │
     │ conventions/ .git   │   │ 否则直读回退（零写盘）    │
     └────────────────────┘   └────────────────────┘
```

### 1.3 被否决的备选形态

| 形态 | 否决原因 |
|---|---|
| 云记忆服务 / 数据库为唯一真相 | R2.2/R2.3；Mem0ᵍ"开源版删光、平台专属"实证托管引力 |
| 常驻守护系统作为起点 | 无存在理由（判断4）；引入"服务挂了"新故障面 |
| 每工具内置记忆 | R1.2/R2.3/R8.11——碎片化记忆等价于没有记忆 |
| 每宿主各自实现机制层 | R7-a 强制语义漂移、R8.11 单一副本破裂 |
| 每项目独立存储 | 跨项目偏好失效、约定单一事实源破裂；隔离靠视图（D10） |
| 路径推断身份 | 文件系统位置不承担本体论职责（D9；carve-out 见红线11） |
| 依赖 Agent 间命名默契 | 身份靠解析不靠默契（D11） |
| SQLite（或任何单文件库）为唯一存储 | R2 硬约束；损坏爆炸半径、git 合并失效（D5） |
| 唯一性/完整性强制点建在索引上 | 索引可丢弃，强制点必须在真相层原子原语（红线8） |
| 读路径隐性写（trail 随读 / 对账随读落盘 / 读推进序号） | 纯读不变量是测试前提（红线18） |
| 台账接续绑定 principal | 工作现场 ≠ 执行体身份（D13.5） |
| watcher 作为新鲜度依据 | watcher 是性能优化，不是证明（红线21） |
| **stat 快判一致即信任索引** | 同尺寸保 mtime 修改返回旧值；新鲜性必须可证明，否则直读（B1，红线21） |
| 维护屏障用"单次探测"实现 | check-then-act 竞态；屏障必须持有（红线20） |
| 用每记录锁分配全库事件序号 | 序号重复/丢失；必须全库串行资源（红线24） |
| **以版本号判据做事件回填** | 漏 v1 创建、保版本手改、中间空洞；判据必须是状态覆盖比较（B2，红线24） |
| **以版本号判据做事件回填** | 漏 v1 创建、保版本手改、中间空洞；判据必须是状态覆盖比较（红线24） |
| **回填在锁外判定后直接追加** | 可与普通写交错追加过时事件；判定必须在记录锁内重读、序列锁内复核（本轮 B2，红线24） |
| **漂移/回填仅枚举现存文件** | 外部删除/重命名同时从日志过滤与漂移结果消失；枚举域必须含日志历史集合（红线24/26） |
| **absent 不作一等状态 / 缺失记录不加锁** | 同一次删除无限重复回填、current_drift 不收敛；external_delete 与同 ID 重建竞态——absent 纳入状态三元组、缺失记录用同规范路径锁（本轮 B1/B2，红线24） |
| 仅用版本号做 CAS 基线 / 截断摘要做绝对承诺 | 保版本手改静默覆盖；碰撞可绕过（红线19） |
| **以 worktree 状态冒充 since 增量** | 无游标语义、漏外部提交与无 git 形态；增量 = 日志 ∪ 漂移扫描（B3，红线26） |
| 后台 LLM 自主整理记忆 | 判断5/R5.5/R9.6（D6） |
| 纯 CLI + 跨进程文件锁（无 sidecar） | Windows 锁语义复杂化；被 D2 双前端取代 |
| 图数据库引擎 / 服务端 LLM 抽图 | 违反 R2 与 D6；图=引用图声明式扩展（D16） |

---

## 2. 决策记录

### D1 形态：本地资产 + 机制层，拒绝服务化

**决策**：存储为用户级目录中的开放格式文件；机制层为无守护义务的本地二进制；宿主薄适配接入。
**约束**：落盘协议崩溃原子（临时文件+原子改名），与前端无关。

### D2 sidecar：懒加载性能形态，两条不变量

**决策**：sidecar 是机制核心库的第二前端。单实例握手懒拉起、空闲自退、版本握手自杀更新。**不变量一**：一个机制核心两个前端，sidecar 不在走嵌入模式，只是降速不是故障。**不变量二**：sidecar 对真相无所有权，任意时刻可 kill，冷启动重扫重建。**sidecar 的独特价值 = 维持 OS 变更日志订阅（新鲜性证明的来源）与索引热维护**（D5）。

### D3 并发协议：门闩（持有）→ 记录锁 → 全摘要令牌 CAS → 原子写 → 事件分配

**决策**：单记录写路径，锁序全库统一（门闩共享 → 记录锁 → 序列锁叶子）：

1. 获取共享门闩贯穿全程（`barrier.lock` 读写门闩；维护持独占；单次探测不构成屏障）；
2. 锁定 `<record>.lock`（**锁文件独立于记录文件存在**——记录不存在（待创建/已删除/待恢复）时同样可锁；create/restore/fix 与缺失记录回填共用同一规范路径锁，删除与同 ID 重建由此互斥）；陈旧锁判定：PID 死亡即抢、存活永不抢、超时只许等待方放弃；
3. **全摘要令牌 CAS**：修订令牌 = `v<版本>@<sha256 全文十六进制（64 字符）>`，机制在读响应附带；锁内比较版本**与**摘要，不一致 → 结构化冲突（含当前令牌），不得落盘。全长度摘要在密码学意义上支撑"必被检出"（高优1）；
4. 类型声明校验载荷（strict 拒收 / lenient 降级告警）；
5. temp + 原子改名落盘，信封版本 = 基线 + 1；
6. 序列锁下向事件日志追加事件（序号唯一分配点），释放序列锁；
7. `git commit -- <该记录路径> events.jsonl`（**记录路径 + 事件日志同批**，高优1；可重试资源；commit-pending 不分配序号，重试时同样含两者）；
8. 释放记录锁；写路径顺带登记台账 trail；
9. 释放共享门闩。

**操作版本语义**：create 无令牌（唯一键路径独占即判定）；update/link 强制令牌（link 两次单记录 CAS + 回滚提示）；merge/apply 独占门闩内逐记录令牌 CAS。
**验收用例**：同令牌并发更新一成一冲突；`show` 后保版本手改 → `update --expect` 冲突且手改保留；维护期间同样检出。

### D4 git 基座：历史 / 同步 / 撤销

git 承担历史、审计、撤销、多机同步；path-scoped 提交；**事件日志随写例程同批提交**（见 D12）。派生关系禁用 git 祖先承载；线性 main；`.git` 附属可去除；git 提交只提供历史，不提供 worktree 跨记录原子可见性。实现路线：CLI 子进程首发 → git2/libgit2。

### D5 存储混合体：文件真相 + SQLite 索引 + **证明式新鲜性协议**

**决策**：记录 = 文件（真相层）；索引 = index.sqlite（派生缓存：过滤/排序/分页/FTS/预算 LIMIT；边表多跳）。索引约束仅作对账诊断，永不拒收。

**证明式新鲜性协议（B1，红线21）**：索引服务读请求，当且仅当其新鲜性**可证明**：

- **证明 = 两个条件同时成立**：
  - (a) **stat 一致**：records/ 目录枚举（路径+mtime+size）与索引检查点一致；
  - (b) **变更覆盖无缺口**：自检查点时刻起，存在覆盖该区间的 **OS 变更日志订阅**（NTFS USN journal / Linux inotify / macOS FSEvents，由 sidecar 常驻建立并维护），且订阅记录中不存在未消化的本店路径修改事件——**内容级修改即使保 mtime、同尺寸也会被变更日志捕获**；
- (a)∧(b) 成立 → 走索引应答（快路径）；
- **任一不成立**（sidecar 不在/嵌入模式、订阅不存在、卷不支持变更日志（exFAT/网络盘）、stat 失配、只读）→ **直读真相层应答**：内存扫描过滤子集构建结果，标注"直读模式"。FTS 仅在快路径可用，直读模式下标注"全文检索需要 sidecar 在场"；
- **读路径零写盘**：判定与直读全在内存；索引持久化只发生在写例程（写路径 / `mem reconcile` / watcher 例程）。

**性质**：不存在"已知可能返回旧值"的路径——要么索引被证明新鲜，要么读的就是文件本身。**直读成本（高优2，如实修正）**：为匹配类型/引用/principal 等过滤条件，直读通常须枚举并读取候选全集，成本为 **O(库规模)**（按 ID/直达路径的查询除外）；输出硬预算（R4.2）不受影响，但影响冷启动成本与大规模直读耗时——记入开放问题29 性能基线；规模增长走判断 3 式升级，协议不变。`verify --deep`（全量内容摘要核对）保留为**维护审计例程**，不再是新鲜性的补偿——盲区已结构性消除。
**验收**（§8 第 4 步）：同尺寸修改、粗时间戳粒度、恢复 mtime 三场景 × sidecar/watcher 停止 × 只读，全部第一次读取返回新值。

### D6 守护进程边界：自主的注意力，被监督的笔

sidecar 自主权 = 机械性动作：变更日志订阅与索引热维护、存疑打标（加性注记）、重复/断引用/未消化证据计数 → **agenda 提案队列**。语义判断由宿主侧维护 Agent 执行；落笔经确认。守护进程永不调用模型。进阶档：版本化预授权策略（留 plan+diff+策略引用，默认关）。

### D7 实现：Rust multi-call binary

`mem` / `mem serve` / `mem mcp` 三形态同一二进制。信封可为 Rust 类型；**内容类型与载荷禁止编译进二进制**。接口稳定，核心重写有界。

### D8 资产是规格，不是代码

护城河 = 三个公开规范：信封格式、类型声明两件套格式、约定文档单一事实源结构。

### D9 身份、主题、项目：第一类建模，拒绝路径推断

三维度统一为"实体记录 + 信封类型化引用"：principal（通道分配、存在性强制、防错报不防伪装）、topic（分类是数据）、project。全走信封引用 + 反向索引（R8.4）。principal 只管归属不管现场相关性（D13.5）。首条 principal 经 `mem init` 自举逃逸。cwd 指针 = 可选缺省视图指针（显式声明）。自举试金石：principal/view 必须经类型声明引入。

### D10 视图：角色的信息可见性是声明式数据

声明式查询模板（类型 × 引用 × principal × 时间），引用一律填 uuid v7。隔离不靠物理分区（R8.11）。未来钩子：`mem export`。

### D11 身份收敛：地址是身份，名字不是

引用按 ID 不按名（存在性强制）；唯一键 → 规范路径原子独占（NFC+大小写折叠+Windows 字符处理），输者返回既有记录及令牌；slug 只是键成分；merge 按 D17 屏障协议；别名只作用于查找解析层；多 Agent 记忆收敛在派生层。R7 归位：(a) 机械强制清单 + 令牌 + 事件序号；(b) conventions 教学。

### D12 事件日志、游标与增量契约

**事件日志 `events.jsonl`**（追加式，一行一事件，**序号 = 行序**）：

```
{"seq":413,"ts":"…","id":"0198…","path":"records/task/proj_x9/login-api.md","op":"update","old_v":5,"new_v":6,
 "old_digest":"9f01…64hex","new_digest":"33ca…64hex","by":"0198…(zcode_main)","kind":"op"}
```

事件携带 `path` 字段（**ID↔路径映射**，支撑已删除/移动记录的定位与警示，B1 前提）、新旧内容摘要。**全库串行分配**：序列锁（叶子锁）下追加；唯一分配点 = 内容写入时刻；commit-pending 不分配。

**状态覆盖回填（统一状态机，替代版本号判据）**：写例程（写路径 / `mem reconcile` / watcher）执行。**枚举域 = 当前记录集合（records/ 枚举）∪ 事件日志历史集合**（events.jsonl 出现过的全部 record_id/path）——仅扫现存文件会漏掉外部删除。

*记录状态三元组*：`state = (location ∈ {records, attic, absent}, version, digest)`：

- records/ 中 → location=records，version/digest 取自信封；
- attic/ 中（受管 tombstone）→ location=attic，version/digest 取自 tombstone 文件；
- 两处皆无 → location=absent，version=null，digest=null。

`external_delete` 事件的"事件后状态"即 absent——**absent 是一等状态，不是"无状态"**；受管删除事件的事件后状态为 attic（两种"消失"是不同的状态值，恢复/续跑不会误判）。

*统一算法（对 known ∪ present 的每条记录）*：

```
cur  = 锁内重查 records/ 与 attic/ 复算的三元组
last = 该记录最后事件的"事件后状态"三元组
· last == cur（含 absent == absent）      → 无动作（稳定态；同一次删除只回填一次）
· last ≠ cur 且 cur.location == absent   → 追加 external_delete 事件（事件后状态 = absent）
· last ≠ cur 且其余                       → 追加 backfill/op 事件（事件后状态 = cur）
  （版本链断裂置 history_incomplete=true）
```

**并发纪律（缺失记录与现存记录同一协议）**：回填逐记录：**获取共享门闩 + 该记录的记录锁**——锁路径 = 记录规范路径 + `.lock`，**锁文件独立于记录文件存在**（文件已删除照样可锁）；create/restore/fix 的写路径本就以同一规范路径锁定 → **缺失记录的回填与同 ID 重建/恢复被同一把锁互斥**。锁内重查 records/ 与 attic/ 复算 cur（**复查是否已被重建/恢复**：重建先落盘者，回填锁内看到 present，按现存规则处理，不追加 external_delete）→ 序列锁内复核最后事件 → `last != cur` 才追加。普通 update 同样无法插入"比较完成、追加未执行"的窗口（同一把记录锁）；并发回填被记录锁串行化，后者锁内重算发现 `last == cur` → 无动作，不产生重复事件。判定状态必须是锁保护下的最新读取，禁止锁外旧快照直接追加。

判据是**状态一致性**（三元组全等，含 absent）；backfill 显式标注 `kind=backfill`，历史不可完整重建时显式报告（history_incomplete）而非伪造中间事件。**链连续性审计**（verify，只报告不补造）：非 backfill 事件的 old→new 版本链断裂 → 报告历史空洞。撕裂尾行：末行 JSON 不可解析 → 截断，对应记录由回填覆盖。
**验收**：v1 创建后崩溃、v2 缺失 v3 存在、保版本手改、撕裂尾行、外部删除、外部重命名、删除后重新创建、只读存储外部删除、回填比较后可控暂停 → 普通 update 完成 → 恢复回填（不得追加过时事件）、双并发回填同一漂移（不得重复）、**同一次外部删除连续两次 reconcile + 两次 diff（第二次不新增事件、不再出现于 current_drift——absent 稳定态）**、**缺失判定暂停 → 同 ID 重建落盘 → 恢复回填不得追加 external_delete**、**两回填 + 一 restore 三方交错不产生错序事件**——回填后 diff 与证据新鲜度检出全部当前变化。

**游标与增量契约（B3 + 本轮 B1，红线26）**：`diff --since N` = 两部分，语义明确、分开呈现：

- **A. `changes_since_N`（日志过滤）**：seq > N 的事件（按记录去重，附变更史）；
- **B. `current_drift`（漂移扫描，内存，只读安全）**：枚举域同回填（当前集合 ∪ 日志历史集合）——**当前三元组 ≠ 最后事件三元组者**（含 location 变化）：records→absent 报告"记录缺失（external_delete 待登记）"，其余报告"未登记变更（backfill 待执行）"；**absent 已登记（最后事件即 absent）→ 不报告**——稳定态与回填收敛一致，同一次删除不会持续出现。每项附当前令牌（absent 项无令牌，附最后已知 path）与文件 mtime（信息性）。current_drift **不受游标过滤**——它们尚无序号；**独立命名、独立分页预算**（高优3），不混入 changes_since_N；写例程回填后获得序号、转入 A，从此不再出现。与 git **完全无关**：外部提交、无 .git、长期未提交手改、外部删除/重命名一律被三元组比较与并集枚举捕获。

`events.jsonl` 的 git 策略：**随写例程同批提交**（与所触及记录路径同一提交）；用户外部 `git add -A` 卷入事件日志无害。
**只读存储的增量契约**：同上结构（A + B），B 项明确标注"只读存储无法登记，将持续出现直至可写回填"——重复是语义真实的（变更确实尚未入日志），不是历史重放噪声。worktree 状态不再参与增量。

### D13 会话台账：协作状态的机制侧原件

1. **创建触发器**（没有一个是读命令）：显式建立（MCP initialize / `mem session start`）；写路径兜底。
2. **台账记录**（per-file 锁）：

   ```
   session_20261104_b7:
     principal: codex_main   view: codex-backend-reviewer   task_ref: 0198…|null
     conventions: v7   watermark: {seq: 412}   trail: [...]
     handoff_from: session_20261102_9f | null
     status: active   last_write: T
   ```

3. **trail 双通道**：写通道（写路径内登记）+ 读通道（客户端累计，`mem session flush` 显式提交）。只读时 flush 失败标注。
4. **git 策略**：不逐次提交，随 flush/对账合并提交（开放问题27）。
5. **接手**（显式、幂等）：`mem session start --handoff auto|<id>` 同一路径完成选择来源 → 创建新台账 → 落盘 handoff_from 与继承水位 → 返回身份块；brief 纯读。**幂等键 = principal + view + task_ref**（高优3：同 view 不同任务的并行会话各得台账，不互串）。选择规则（作用域 = view + task_ref，**删除跨任务回退**，高优2）：

   1. 显式 `--session/--handoff <id>`；
   2. 同 view 且 **task_ref 相等**的最近可接续台账；无则同 view 且 **task_ref 为空**的最近可接续台账；
   3. **task_ref 不同的台账永不接手**（跨任务不误接）；
   4. 无可接续 → handoff_from=null 新建（仅在 start 这一写路径内；brief 无台账时无接续返回）。

   可接续状态：flushed/closed，或 active 且 last_write 超空闲阈值（开放问题37）。接手 = 新台账，来源原样保留；活跃会话不可自动接手（peers 摘要呈现）；活跃性度量 = 写路径触碰。
6. **三条纪律**：台账住店里；R1.1 接手走第 5 条；重投递由台账驱动。
7. **归属与理解同源**。

### D14 义务投递：不指望 Agent 的记性，只控制它的输入

session-start hook = `mem session start --handoff auto` → 注入 brief；end hook = `mem session flush --trail …`（置 flushed 可接续）；工具响应 footer 机械提示。收束：领域记忆真相在记录文件，协作状态真相在台账与事件日志，Agent 上下文永远是缓存。

### D15 role（组合点，暂不实体化）

principal（谁）+ view（看到什么）+ conventions 角色段（怎么行为）会话建立时组合；需要时走类型声明引入。

### D16 图的正名 + 未消化证据查询

节点 = 实体记录 + 内容记录；边 = 信封引用 + 类型化派生关系（图模式由声明定义）。抽取 = 约定 + 调用方执行体；检索 = `mem trace`；永不引入图引擎；有属性的边建模为记录；派生类强制 DAG，一般环 agenda 报告。**未消化证据查询** = 证据引用逐项比较事件序号 vs `evidence_cutoff_seq`（信封字段），非全库 diff。

### D17 多记录事务协议：持有的写屏障 + git 提交语义 + 删除语义

- **写屏障**：barrier.lock 读写门闩——普通写共享锁贯穿全程，维护独占锁；锁序 门闩→记录锁→序列锁；读路径不受门闩影响。
- **维护流程**：独占门闩 → 枚举（真相层直读或证明新鲜的索引）→ 逐记录令牌 CAS → 重定向 → 别名 → tombstone 最后 → 单提交（含 events.jsonl 同批）→ 释放。
- **崩溃恢复**：别名先于删除生效；计划 `status: executing` 供 verify 检出 → 续跑/回滚（显式、经确认）。
- **边界内手改**：令牌 CAS（全摘要）拦截 → 冲突转人工确认队列；边界外手改由漂移扫描/对账暴露。
- **git 提交**：index.lock 可重试；耗尽 → commit-pending（不分配序号）；提交滞后合法。
- **删除语义（定稿）**：派生关系默认阻止（force 时全量警示标注）；一般引用级联 + 断链注记；tombstone 可恢复。

### D18 信封物理格式：单文件 Markdown + JSON frontmatter

**决策**：一条记录 = 一个 `.md` 文件。第一行恰为 `---`，至下一个恰为 `---` 的行为 **frontmatter，必须是合法 JSON**（UTF-8，解析失败是响亮错误，R6 路径处理）；其余为 **Markdown 正文 = 载荷自由文本**。frontmatter 内分两块，**所有权在文件里字面可见**（R2 审计："它到底记了我什么"由 envelope 块机械回答）：

```markdown
---
{
  "envelope": {
    "id": "0198f7a2-b4c1-7cd2-9e3f-1a2b3c4d5e6f",
    "type": "task",
    "created": "2026-11-04T09:12:00Z",
    "modified": "2026-11-04T11:40:00Z",
    "version": 6,
    "principal": "0198f7b2-…",
    "refs": [
      {"rel": "belongs_to", "target": "0198f7a9-…"},
      {"rel": "derived_from", "target": "0198f8d0-…"}
    ],
    "aliases": ["login-api"],
    "evidence_cutoff_seq": null,
    "commit": "e3f1…"
  },
  "payload": {
    "project": "0198f7a9-…",
    "slug": "login-api",
    "status": "in_progress",
    "due": "2026-11-10"
  }
}
---
# 实现登录接口

登录接口的上下文、进展、注意事项……（人/Agent 自由书写）
```

**裁决理由（候选淘汰）**：JSON 侧车双文件——破坏单文件原子性（D3 的 temp+原子改名覆盖一个文件，崩溃窗口留下半更新对），否决；单 JSON（正文做字符串字段）——R2.2 兜底场景最差（转义、无渲染、人工编辑错误温床），否决；YAML frontmatter——**静默类型强转**（`no`→false、`1.0`→float、冒号断行）产生"错误但合法"的数据且恰发生在人手编辑这条无机制保护的路径上，与五轮评审消灭静默失败的主线（R3.2 精神）正面冲突，否决。JSON 解析唯一且确定：失败响亮（strict 拒收 / lenient 降级告警），无静默面。
**机制再生成纪律（红线27）**：机制写记录时只重生成 frontmatter（**确定性序列化：键排序固定**），**正文按字节保持不变**——正文是人/Agent 的字节领土；正文字节变化必然改变修订令牌摘要，由 CAS 收敛。修订令牌（版本+全摘要）照旧覆盖整个文件字节。
**fix 流（R6.3）**：frontmatter 解析失败 → 从 git 历史取上一版 frontmatter ＋ 保留当前正文，走 `mem fix` 交互重建。
**生态取舍（明确接受）**：不兼容 Obsidian 等 YAML-frontmatter 工具——R1–R9 无此要求；若将来需要交换，走 `mem export`。

### D19 类型声明格式与键字符集

**决策**：`types/<t>/schema.json` = **JSON Schema draft 2020-12** ＋ 自定义关键字词汇表（由 `mem` 的校验器机械执行，R8.5）：

| 关键字 | 语义 | (a)/(b) |
|---|---|---|
| `x-grade`: `"strict"` / `"lenient"` | 校验分档（strict 坏字段拒收＋fix 通道；lenient 未知字段容忍＋告警，R8.6） | (a) |
| `x-unique-key`: `[字段名…]` | 自然键（作用域限定的引用字段 + 本记录字段）；规范路径推导依据（D11.2 原子独占） | (a) |
| `x-ref`: `{"target_type": …, "existence": true}` | 引用字段：目标类型与存在性校验（R8.5） | (a) |
| `x-dag`: `true` | 派生类引用强制无环（D16） | (a) |
| `x-topics`: `[主题面…]` | 必挂主题面（校验时机见开放问题33） | (a)/(b) 边界 |

**词汇表属基础设施**（mechanism 认识这五个关键字）；**类型本身仍是纯数据**——新增类型 = 新增 schema.json + conventions.md，零代码（红线13 不破：基础设施新增的是"认识关键字"这一信封级机制，与 R8.2 的"载荷结构零知识"不冲突，载荷字段的含义仍完全由声明提供）。
**键字符集规则（M1 收尾）**：规范化 NFC；路径推导用 **casefold**（Windows 大小写不敏感，两键一径 → EEXIST → 同一记录）；slug 允许 Unicode 字母/数字/连字符，黑名单 = 路径敌对字符（`<>:"/\|?*`）、控制符、结尾点/空格；保留名禁用（`conventions`、`types`、`records`、`sessions`、`agenda`、`index`、`events` 等店级目录名）；长度上限 128 字节（细则随格式规范 v1 微调）。

### D20 查询词项文法：Unicode 空白切分 + `-` 否定标记

**决策**（R10 决策① 收编，2026-11-10 用户裁决提升）：检索 query 是单参数字符串，词项按 **Unicode White_Space**（Rust `char::is_whitespace`，含 U+3000 全角空格——中文输入法友好）切分；每个词项按折叠单源（NFC + Unicode 小写，`paths::fold`）折叠一次后参与匹配。**否定标记 = 半角 `-`（U+002D）前缀**：`-x` 为否定词项（匹配串 = 去前导 `-` 后的原串）；`--x` = 否定词项、匹配串 `-x`；恰为 `-` 的词项 = 字面肯定词项；全角连字符不是标记。**组合谓词**：命中 := ∀正词项命中 ∧ ∀负词项不命中——谓词本体仍是四域单源 `search::record_matches`（id 前缀 ∪ 逐别名 ∪ 逐 payload 值——键名不参与 ∪ 正文）；正文域要求合法 UTF-8，非合法则该域退出匹配（有损解码伪命中形态废止）。**空查询与纯否定查询显式拒收**（`query_invalid`，两前端同码）——绝不表现为空命中清单或全库清单。
**登记边界**：以 `-` 开头的字面肯定词项不可直接表达（`-font` 无法作正词项查询；子串语义下查 `font` 仍可命中含 `-font` 的记录，代价是混入所有含 `font` 的记录）——消除随未来短语精确语法。
**源出与评审**：`keyword-search-design.md` 决策①（六要素与排除方案：NOT 关键字、`!` 前缀）；实施评审四轮见 `docs/review/keyword-search-implementation-review.md`。

### D21 载荷字段过滤：声明词汇 `x-filter` ＋ 规范条件"字段＋标量文本"

**决策**（R10 决策③④ 收编，2026-11-10 用户裁决提升；词汇与等值语义一体，拆开则任一条不自足）：
- **声明词汇**：payload 属性级 `x-filter: true`（与 x-ref 同位）声明该字段可作清单与检索的过滤条件；**取值形态由属性本体的 `type` 关键字承载，不从 enum 推导**。v1 支持形态 = `string` / `number` / `boolean` / 元素为标量的 `array` 四类；`type: "integer"` 显式排除（2026-11-10 用户裁决维持——需过滤的数值字段请声明 `number`）；支持形态之外的声明 → verify 告警 `filter_shape_unsupported`，运行时过滤 → `invalid_filter_value`（声明不失败，前向容忍）。词汇表版本 1→2（机制关键字 6→7）。
- **规范条件 = 字段＋标量文本**：CLI `--filter 字段=值` 值原样为文本；MCP `filters` 值一律 JSON 字符串（非字符串 JSON 标量显式拒收）——两端合法条件空间逐字相同，同一条件必得同一命中集；"只要某一形态"的子集表达两端同由 `--type`/`type` 收窄承担。
- **等值语义**（按记录自身类型的声明解释，精筛裁定）：string → 折叠等值（"Geometry" 与 "geometry" 互可检出）；number → 文本按 JSON 数值文法严格解析后**精确实数等值**（"1" ≍ "1.0"；整数×浮点不经 f64 折叠——实施评审 B1：|v| > 2^53 处相邻整数不误判等值）；boolean → 仅认字面 `true`/`false`；array → 任一元素按其实际 JSON 形态等值即命中。
- **候选类型集与兼容类型集**：候选集 = type 参数/view 限定到的类型集（两者同给取交集；未限定 = 全店已加载）；候选集中无任何类型声明该字段 → `unknown_filter_field`（防手滑静默空结果）；声明了但文本不可解释（兼容集为空）→ `invalid_filter_value`；未声明/不兼容类型的记录静默不命中、不报错；多条件的 indexed 收窄 = 各兼容类型集交集。
- **显式错误 4 枚**（可机判，两前端同码）：`query_invalid` / `unknown_filter_field` / `duplicate_filter_field` / `invalid_filter_value`。
**源出与评审**：`keyword-search-design.md` 决策③④；规范载体 `type-declaration.md` v1.1（x-filter 行 + 配套规则 + 修订表）。

### D22 检索执行器单源下沉 mem-core ＋ 索引"构造即证明"

**决策**（R10 决策② 收编，2026-11-10 用户裁决提升）：检索与过滤清单的执行——词法解析、过滤声明校验、组合谓词、精筛、命中序分页、坏记录告警组装——**单源住在 mem-core（find 模块）**，CLI 与 MCP 两前端同源调用（r7–r11 五轮评审"两套匹配实现必然漂移"教训的制度化收口）。索引注入采用**构造即证明**：`mem-index::open_proved(store, hello)` 是已证明句柄（ProvedIndex）的**唯一构造路径**，门禁 = stat 咨询 ∧ 只读打开（不建库不建表，红线18）∧ 只读 schema 门禁（含 bad_records）∧ epoch 同代核对；`FindPrefilter` trait 只承载已证明句柄的两个查询（candidates 多词项合取超集粗筛 / bad_records 告警清单），**证明逻辑不出现在 trait、不进入执行器、不留在前端**。indexed 执行中任一索引调用失败 → 回退 direct 全量应答（reason 携带真实错误），禁止空命中或空告警的 indexed（红线8）。
**检查点纪律（r5 B3 立法重申并扩展）**：检查点 stat 集**恰好等于物化那一次扫描看到的文件集合**（scan.records ∪ scan.bad，含坏/不可读文件）——绝不重新枚举磁盘（实施评审 B2：重枚举会让扫描后新落盘的文件被"提前确认"，消掉"新文件不在 entries → 实时枚举失配 → 直读"的信号 → indexed 漏命中/漏告警）；护栏以注入式维护器（rebuild_with_entries_with_hook）固化该窗口。
**源出与评审**：`keyword-search-design.md` 决策②；红线 8/9/18/21 守恒对账见该文档 §2.8。

---

## 3. 组件轮廓

### 3.1 数据目录布局

```
~/.agent-memory/
├── .git/                      # 历史基座（附属物；不再参与增量语义）
├── mem.json                   # 库级配置（存疑规则、预算、空闲阈值、订阅能力声明）
├── events.jsonl               # 事件日志（追加式；序号=行序；含新旧摘要；随写例程提交）
├── barrier.lock / events.lock # 运行时锁（门闩、序列锁）
├── .pending-commits           # commit-pending 对账（不分配序号）
├── conventions/CONVENTIONS.md # 约定单一事实源（含角色段）
├── types/<t>/schema.json + conventions.md   # 两件套（task/memory/project/topic/principal/view/edge）
├── records/<type>/<键推导路径>/...
├── sessions/                  # 会话台账（per-file 锁）
├── agenda/                    # 提案队列
└── index/index.sqlite         # 派生索引（证明式新鲜性；读路径零写盘）

（存储之外，可选）D:/proj/A/.agent-memory   # 缺省视图指针（显式声明，可删）
```

### 3.2 信封与记录（物理形态见 D18）

信封字段（frontmatter `envelope` 块，机制所有）：id（uuid v7）、类型标识、created/modified、并发版本、principal（存在性强制）、refs（类型化引用，只填 ID）、aliases、`evidence_cutoff_seq`、来源提交 SHA。frontmatter `payload` 块 = 类型特有字段，仅由类型声明解释（R8.4）。**正文 = 载荷自由文本，机制再生成 frontmatter 时按字节保持（红线27）**。**修订令牌 = 版本 + 完整 SHA-256（64 hex，覆盖全文件字节），读响应附带，不存储**。键字符集规则见 D19。

### 3.3 CLI 命令面（草案）

| 命令 | 作用 | 需求锚点 |
|---|---|---|
| `mem brief [view] [--since <seq>]` | 冷启动工作视图。零写盘；无台账如实报告；索引不可证明 → 直读标注 | R1.3/R3.5 |
| `mem session start [view] [--handoff auto\|<id>]` | 显式建立/接手（幂等键 principal+view+task_ref；同路径落盘 handoff） | R1.1, D13.5 |
| `mem session flush --trail <ids…>` | 提交读留痕，置 flushed | D13/D14 |
| `mem reconcile [--deep]` | 写例程：索引刷新 + 状态覆盖回填（并集枚举域，含 external_delete；锁内判定与追加）+ 漂移物化；--deep 全量摘要审计 | D5/D12 |
| `mem list` / `find` | 过滤定位（LIMIT+续读；证明式新鲜性，否则直读标注；返回完整令牌） | R4, D5 |
| `mem show <id>` / `mem trace <id>` | 钻取 / 多跳遍历（预算+截断标注；返回令牌；零写盘） | R4, D16 |
| `mem add <type>` | 创建（唯一键路径独占；通道盖章；兜底建台账；追加事件） | R3.3 |
| `mem update <id> --expect v6@<64hex>` | 读改写（全摘要令牌 CAS；共享门闩贯穿） | R3.2/R2.4 |
| `mem link <a> <b> --rel <t>` | 建立引用（两次令牌 CAS；派生类 DAG） | R9.1 |
| `mem merge <dup> <canonical>` | 合并（独占门闩内） | R3.4/R5.4 |
| `mem diff [--since <seq>]` | **增量 = changes_since_N（日志过滤）∪ current_drift（独立分页预算；含外部删除/重命名；与 git 无关）** | R4.3, D12 |
| `mem view` / `mem bind` | 视图执行；缺省视图指针 | D10/D9 |
| `mem init` | 建店 + 自举首条 principal | D9 |
| `mem agenda` / `mem apply <plan>` | 提案队列；执行计划（独占门闩内） | R5.5 |
| `mem fix <id>` / `mem verify [--deep]` | 修复；全库校验（坏条目、半成品计划、断链、历史空洞、--deep 摘要） | R6, D17 |
| `mem serve` / `mem mcp` | sidecar / MCP | 渐进增强 |

### 3.4 宿主适配：渐进增强四档与降级路径

| 档 | 能力 | **降级行为** |
|---|---|---|
| L1 原始数据访问（底座） | 裸读目录 | 无台账/身份块/hook；约定直读；接续退化为时间戳+git log；闭环成立（R8.10） |
| L2 约定注入 | 机械注入（start --handoff + brief；end flush）> 行为指针 | 无 hook 降指针；台账退化为写路径兜底 |
| L3 行为教学 | Skill / instructions 包装 | 同 L2 |
| L4 完整工具协议 | CLI + MCP（initialize 建/接手台账；schema 携带 expect 全令牌） | MCP 断连降 CLI；sidecar 不在降嵌入模式（**嵌入 = 直读模式 + 无 FTS**，标注） |
| **（跨档）只读存储** | — | 五读命令全部可用（零写盘）；diff 走"日志 ∪ 漂移"契约，B 项标注只读不可登记；写类操作结构化错误 |

---

## 4. 需求映射

| 需求 | 机制承担 |
|---|---|
| R1.1/1.2 状态跨执行体 | 台账 + `session start --handoff auto`（幂等键含 task_ref；落盘 handoff 与水位）；跨工具验收 = 标准 hook 无传参 |
| R1.3 稳定读取入口 | brief（零写盘、只读可用、无台账如实报告）→ list → show |
| R2.1–2.3 开放格式/人读可改/迁移 | 记录=文本文件+信封规范；.git 附属可去除（增量不再依赖它） |
| R2.4 手改合法 | 手改即时可见（直读模式天然实时 + 漂移扫描检出）；**保版本手改被全摘要令牌检出**；补登在写例程 |
| R3.1 写原子性 | temp+原子改名 |
| R3.2 无静默丢失 | 共享门闩（物理）+ 全摘要令牌 CAS（逻辑）+ 冲突一等结果 |
| R3.3 创建唯一性 | 唯一键→规范路径原子独占 |
| R3.4 引用完整性 | 删除语义定稿；merge 在持有式独占门闩内完成 |
| R3.5 外部修改收敛 | **证明式新鲜性**：索引仅在可证明时服务读，否则直读——任何形态第一次读取不返回旧值；同尺寸/保 mtime/粗粒度场景全覆盖（D5） |
| R4.1 分层访问 | brief→list→show→trace；视图入口 |
| R4.2 硬预算 | LIMIT+标注+续读；遍历预算 |
| R4.3 增量刷新 | **changes_since_N（日志过滤）∪ current_drift（并集枚举 + 状态比较）**：已登记变更按游标重放；未登记变更与**外部删除/重命名**（日志历史集合捕获）由漂移检出并标注，回填后转正——不漏报、不冒充 since 语义（D12） |
| R4.4 机制强制 | 门闩/令牌/序号/证明式判定全在 mem 内 |
| R5.1 新鲜度可见 | 信封时间戳 |
| R5.2 存疑策略 | 用户声明规则 → 机械打标 |
| R5.3 依赖前验证 | 约定教学；peers 证词姿态 |
| R5.4 矛盾收敛 | 令牌 CAS 更新 + 唯一键 + merge/别名 + 约定防增殖 |
| R5.5 维护例程 | agenda → 确认 → 独占门闩内单提交；半成品可检出 |
| R6.1 可识别 | 校验定位条目与字段；verify 报告坏条目/半成品/断链/历史空洞 |
| R6.2 可隔离 | 读跳坏条目+告警；只读全可用；索引不可证明直读回退不崩溃 |
| R6.3 可修复 | `mem fix` + 手改双通道 |
| R7.1–7.3 强制/教学分离 | (a) 全在 mem；(b) conventions/ |
| R7.4 单一事实源多级下发 | conventions + hook / 指针 |
| R8.1/8.2 声明式/零知识 | types/ 运行时加载；载荷禁编译；自举自检 |
| R8.3/8.4 信封/载荷 | 信封 Rust 类型；跨类型操作只碰信封；有属性边建模为记录 |
| R8.5/8.6 校验强制与分档 | jsonschema + 分档/唯一键/DAG/存在性 |
| R8.7 演化容忍 | 版本降级标注；迁移=显式例程 |
| R8.8 两件套 | schema.json + conventions.md |
| R8.9–8.11 渐进增强/单一副本 | 四档降级表；单目录单副本；隔离靠视图 |
| R9.1 派生可表达 | 类型化引用（图模式由声明定义） |
| R9.2 证据可追溯 | 反向索引；派生 force 警示；断链注记；principal 溯源 |
| R9.3 收敛约束 | 提炼走令牌 CAS 更新；agenda 确认 |
| R9.4 增量再提炼 | 证据引用逐项 vs `evidence_cutoff_seq`（事件序号，依赖 B2 全库串行与回填完整性） |
| R9.5 层间冲突可见 | 派生链可见（trace）；优先级归类型约定 |
| R9.6 原件不可侵 | 提炼加性；接手建新台账 |
| R9.7 产物新鲜度 | 证据截止时间 + `evidence_cutoff_seq` |

---

## 5. 设计红线

1. 内容类型与载荷结构禁止编译进二进制；信封除外（R8.2）。
2. 派生关系用类型化应用引用承载，禁止用 git 祖先/提交拓扑。
3. 守护进程与 sidecar 永不调用模型；改写记录内容必须有授权（逐次确认或版本化预授权，默认关）。机械注记除外。
4. 约定注入用 hook 机械注入或指针，不用全文拷贝。
5. sidecar 对真相无所有权；机制协作状态的原件在存储（台账、事件日志）。
6. 落盘协议崩溃原子，与前端无关。
7. 记忆库不用分支模型；线性 main。
8. 索引永不 gate 正确性；索引约束永不作为拒收依据。
9. 预算截断必须显式标注并给出续读路径。
10. `.git` 是附属物：去掉后存储仍完整合法（**增量契约不依赖它**）。
11. 身份、主题、项目是声明式数据，禁止环境推断；cwd 指针仅选择已声明视图。
12. principal 强制存在性校验；自报身份不被采信；防错报、不防伪装。
13. R8 自举检验：principal 与 view 必须经类型声明引入。
14. Agent 上下文中的协作状态视为缓存；协作状态必须有机制侧原件。
15. 身份的传达与归属同源同通道。
16. 永不引入图数据库引擎。
17. 派生类引用强制 DAG；一般引用环由 agenda 报告。
18. **读取路径零写盘**——含索引缓存与事件日志；判定与直读全在内存；对账持久化只发生在写例程。
19. **一切读改写必须携带修订令牌（版本 + 完整 SHA-256 摘要）并在锁内比较**；保留版本号的人手修改必被检出（全长度摘要支撑密码学意义上的"必"）。
20. **维护屏障必须持有而非探测**：普通写共享门闩贯穿全程，维护独占门闩；锁序全库统一（门闩→记录锁→序列锁）。
21. **索引仅在新鲜性可证明时服务读请求**（stat 一致 ∧ OS 变更日志订阅覆盖检查点以来无缺口）；证明不存在（sidecar 不在、卷不支持、stat 失配、只读）→ 直读真相层应答并标注；**不承认任何"已知可能返回旧值"的快路径**。
22. **陈旧锁判定**：PID 死亡即抢、存活永不抢；超时只允许等待方放弃。
23. principal 只负责归属；接续作用域是 view + task_ref。
24. **全库事件序号由序列锁（全库串行、叶子锁）分配**；事件携带新旧内容摘要与记录路径（ID↔路径映射）；**记录状态为三元组 (location ∈ {records, attic, absent}, version, digest)，absent 是一等状态**——最后事件已为 absent 时 `last == cur` 稳定态成立，不得重复回填；**回填判据 = 最后事件三元组 ≠ 当前三元组，枚举域 = 当前记录集合 ∪ 事件日志历史集合**（覆盖创建、保版本手改、崩溃缺口、外部删除/重命名）；**回填的比较与追加必须在记录锁 + 序列锁保护下完成，缺失记录使用同规范路径锁（锁文件独立于记录存在）并在锁内重查 records/ 与 attic/ 复算当前状态**，禁止锁外旧快照直接追加；链空洞由 verify 显式报告、不伪造；每记录锁不得分配序号。
25. **接手是显式建立操作**（`session start --handoff`，幂等键含 task_ref）；纯读 brief 永不创建或修改台账。
26. **增量契约 = changes_since_N（日志过滤）∪ current_drift（状态比较 + 并集枚举域）**；current_drift 独立命名、独立分页预算，不混入游标结果；禁止以无游标语义的 worktree/旁证状态冒充 since 结果；未登记变更与外部缺失必须显式标注"backfill 待执行"，回填后转正，不得静默漏报。
27. **机制再生成记录文件时，Markdown 正文按字节保持不变**——机制只拥有 frontmatter（确定性序列化：键排序固定），正文是人/Agent 的字节领土（R9.6 的字节级表达）；正文字节变化必然改变修订令牌摘要，由 CAS 收敛（D18）。

---

## 6. 依赖与许可（全链 MIT/Apache 目标，落地时逐个核实）

| 组件 | 选型 | 用途/备注 |
|---|---|---|
| 异步运行时 / named pipe | tokio | sidecar、单实例握手 |
| 字节 range 读写锁 | LockFileEx / fcntl F_SETLK | 门闩（共享/独占）、序列锁；回退全库写互斥 |
| **OS 变更日志** | NTFS USN journal / inotify / FSEvents | **新鲜性证明的覆盖来源**（D5；能力矩阵见开放问题39） |
| 文件 watcher（notify） | notify | 订阅之上的便利层；非新鲜度依据 |
| SQLite | rusqlite + bundled | 派生索引 + 边表；约束仅对账诊断 |
| Schema 校验 | jsonschema crate | draft 2020-12 + 分档/唯一键/DAG 关键字 |
| CLI | clap | 命令面 |
| 序列化 / 摘要 | serde, serde_json / sha2 | 信封、事件行；全摘要令牌 |
| ID | uuid v7 | 记录唯一身份 |
| MCP | rmcp（3.0 stable，MIT） | schema 携带 expect 全令牌 |
| git | CLI 子进程 → `Git` trait → git2/libgit2 | 历史基座；事件日志同批提交 |
| 目录扫描 | ignore crate | stat 枚举、直读回退 |
| 检索升级坡道 | sqlite-vec / tantivy | 暂不引入 |

---

## 7. 开放问题

**已关闭**（1–22，本轮 +7）：

1–15. （仓库布局、读写性质、唯一键、令牌 CAS、写屏障、接续解耦、外部修改可见、别名作用层、首条 principal、删除语义、游标事件化、陈旧锁、handoff 建立顺序、task_ref 选择序、未消化证据查询——见 v0.6 及前文各决策）

16. **freshness 盲区** → 结构性消除：证明式协议（stat ∧ 变更日志覆盖），否则直读（D5，红线21）。
17. **事件回填完整性** → 状态覆盖判据（最后事件状态 ≠ 当前状态即回填），事件携带新旧摘要；链空洞 verify 显式报告（D12，红线24）。
18. **只读增量** → 日志过滤 ∪ 漂移扫描；worktree 退出增量语义；外部提交与无 git 由状态比较覆盖（D12，红线26）。
19. **令牌摘要长度** → 全 SHA-256（D3，红线19）。
20. **task_ref 回退** → 删除跨任务回退：相等 > 为空 > 永不接（D13.5）。
21. **session start 幂等键** → principal + view + task_ref（D13.1）。
22. **事件日志 git 策略** → 随写例程同批提交；外部卷入无害；增量不依赖 worktree（D12）。
23. **信封物理格式** → 单文件 Markdown + JSON frontmatter（envelope/payload 所有权分块；机制确定性再生成、正文按字节保持）；YAML 静默强转、侧车双文件原子性、单 JSON 兜底体验三项淘汰（D18，红线27）。
24. **类型声明格式** → JSON Schema 2020-12 + 自定义关键字词汇表（x-grade/x-unique-key/x-ref/x-dag/x-topics）+ 键字符集规则（NFC/casefold/黑名单/保留名/长度上限）（D19）。

**未决**（25 起，编号保持稳定；23/24 已关闭见上）：

25. 预算单位与默认值；遍历深度/广度上限；**漂移扫描与 current_drift 的成本/分页预算**（diff 全量状态对比的上限、current_drift 独立分页）。
26. 会话级状态的形状：台账是否受管类型。
27. sessions/ 的 git 策略收尾——与问题28耦合。
28. 多机同步约定：版本/令牌跨机、aliases 跨机、**事件日志跨机归并**（序号分叉）、sessions/ 同步。
29. CLI 命名与 MCP 工具面定稿；性能基线（枚举、直读）；**回填/漂移 O(候选×事件) 复杂度**（v1.2 评审 M5/M11：每候选在序列锁内重读全量 events.jsonl——万级记录 + 十万级事件时 reconcile 塌陷；对齐为"一次快照 + 锁内复核增量"可消）。**检索切片已交付并收编（2026-11-10，实施评审四轮通过）**：MCP 工具面 mem_find 定稿（14→15；query 必填且须含肯定词项，纯过滤由 mem_list 承载）、mem_list 增 filters、查询词项文法（D20）、载荷字段过滤声明词汇与等值语义（D21）、执行器单源下沉与索引"构造即证明"（D22）、坏记录告警经 find/list 透出——记录：`keyword-search-design.md` v0.3；实施评审：`docs/review/keyword-search-implementation-review.md`。**剩余**：merge/bind/agenda/apply/fix/init 的工具映射或豁免盘点；性能基线（回填/漂移复杂度、大库检索成本实测）；FTS 引擎升级（tantivy/sqlite-vec）维持暂不引入，由本问题性能基线触发（R10 粗筛 = 折叠列 LIKE 合取，升级坡道不变）。
30. principal 注册与托管流程。
31. view 类型表达力边界。
32. merge 细节：别名唯一性、tombstone 形态、证据链重挂。
33. 主题分类面声明。
34. role 是否实体化（暂不）。
35. 检索侧时间感知（暂缓）。
36. 多 principal 安装的选择机制。
37. 台账活跃性参数（空闲阈值默认值、保留期归档）。
38. 事件日志轮转与压缩（衔接29）。
39. **变更日志订阅能力矩阵**：USN/inotify/FSEvents 各平台与卷类型（exFAT/网络盘/容器挂载）的检测方式、降级声明与配置（衔接 D5——不支持订阅的卷永久直读）。**v1.2 复审（r2）增补**：非提权 Windows 进程无法打开卷句柄（USN 不可用——**最常见形态**，此前矩阵缺失）；**实施侧裁决（待设计收口）**：新增 ReadDirectoryChangesW 订阅源（`backend: "rdc"`，无需提权、内核过滤驱动产生事件——写操作本身触发通知，同尺寸保 mtime 写入同样被捕获；缓冲溢出 ERROR_NOTIFY_ENUM_DIR 视为覆盖缺口 → 永久直读）。**r4 增补（B6）**：USN **保守停用**——FRN 集合只在 attach 时快照且无调用方更新（新记录/原子替换后的记录失去覆盖），修复 FRN 维护并补提权验收后方可恢复；当前后端 = RDC（非提权可用，端到端已验证）或 none（永久直读）。**r4 B3 同批**：检查点发布收敛纪律（维护后新鲜查询为 0 才发布）与 index_epoch 代际核对（检查点与索引同代）已入实现与规范 §4.2/§5。订阅计数把 `.lock`/`.tmp-` 机制副产物排除在变更之外。
40. **播种资产分发形态**：方向已裁决（2026-11-05，脚手架阶段）——**混合模式：外部目录优先（exe 旁 / 模板覆盖配置指定），字节级嵌入兜底**（git templates 先例；裁决记录见仓库 `assets/builtin-types/README.md`，携带点 `crates/mem-assets`，红线1 解释为禁"认识"、不禁字节级携带）。未收口随格式规范 v1：正式分发渠道承诺、模板覆盖配置定名（已定 `init.template_dir`）、携带边界的规范条文。
41. **envelope.commit 字段接线**（2026-11-05，v1.2 评审 L4 归档）：字段在信封规范中声明（"来源提交 SHA"），机制从未写入——写入时机需要裁决（提交后回写 = 二次写；或读时附带）。与开放问题29 性能基线同批处理。

---

## 8. 下一步（建议顺序）

按复审口径：**第 4–5 步待本版（B1–B3）复审通过**；第 1–3 步可继续：

1. 信封与目录格式规范（v1 定稿）——前置已解决（D18/D19），可直接起草；
2. 类型声明两件套格式规范 + task 试金石；
3. 自举类型：principal 与 view；
4. 核心库（门闩/令牌/事件日志+状态覆盖回填（并集枚举域+锁内判定）/证明式新鲜性+订阅/漂移扫描/git 提交含事件日志）——验收：三场景×三条件 freshness 断言第一次读取不返回旧值；四类注入（v1 崩溃、中间空洞、保版本手改、撕裂尾行）+ **外部删除、外部重命名、删除后重建、只读外部删除**回填后 diff 与证据新鲜度全检出；两轮 diff --since（current_drift 不重复入 A、外部提交出现、无 .git 成立）；**回填并发用例：比较后可控暂停 → update 完成 → 恢复回填不得追加过时事件；双并发回填不产生重复**；
5. CLI 前端 + brief/台账闭环；
6. sidecar + 索引 + 订阅；
7. 宿主适配（MCP → hook → Skill → 指针）；
8. 维护与提炼例程。

---

## 9. 生态盘点与组件选型依据（2026-11-03 扫描）

| 产品 | 许可 | 形态 | 与本设计的关系 |
|---|---|---|---|
| Basic Memory | **AGPL-3.0** | Python MCP，4.4k★ | 架构最接近；无并发控制/git 基座/强制校验/派生链。只参考设计 |
| Engram | MIT | Rust 单二进制 MCP | 可借：别名归一、矛盾检测、显著性衰减 |
| 官方 MCP memory server | MIT | TS，图存单 JSON | 动词面是事实标准 |
| Mem0 | Apache-2.0 | 服务端+向量库 | 图记忆平台专属（托管引力实证）；ADD-only 与收敛路线相反 |
| Letta / Zep(Graphiti) | Apache-2.0 | 自编辑 / 时序知识图 | bi-temporal 是证据截止机制的工业化先例 |
| Cline Memory Bank | 模式 | markdown+指令循环 | 纯约定形态；文件分类可作 conventions 起点 |
| nb | AGPL-3.0 | Bash，git-backed | 同构参考，代码不可用 |

**组件结论**：rmcp 3.0 吃掉 MCP 前端；gix 写路径未就绪；自研仍是三个规格（信封、类型两件套、台账/事件/派生）；心智借签：别名归一、bi-temporal、时间感知检索、官方动词面、Cline 分类、平台引力教训。
