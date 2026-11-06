# MemStore 设计评审报告

- 评审对象:`memstore-design.md`(草案 v0.1,2026-10-30)
- 评审日期:2026-10-30
- 参考源核对:已对照 `D:\Develop\claude-code-main`(`src/utils/tasks.ts`、`src/memdir/`)实际源码

## 评审结论(TL;DR)

设计整体成熟度高:机制/约定分离(P1)、降级表、非目标边界、server 强制的上下文卫生预算、可独立交付的里程碑,这些骨架决策都成立,参考源事实引用全部核实无误。**但存在 2 个阻断项**——都是"规格空洞或自相矛盾"而非架构错误,改动成本低,解决前不应开工 M1:

- **B1**:memory scope `lock: none` 与 `uniqueName: true` 在多进程目标场景下自相矛盾,唯一性约束会被竞态击穿;
- **B2**:semantic-filename 的 slug 规格不完整(非 ASCII 归一化、id 缺省行为未定义),中文内容第一次写入即踩坑。

---

## 一、阻断项(必须先解决)

### B1. memory `lock: none` 与 `uniqueName: true` 在多进程下自相矛盾

**位置**:§4.6 内置 scope 表(memory 行)、§8 各 scope 启用建议("memory — 无锁")

**问题**:目标 §1.2 明确承诺"支持多进程、多 Agent 共享同一份数据(文件锁协调)"。uniqueName 的执行方式只能是"扫描全库 name → 检查 → 写入"的 check-then-act,且检查跨越多个文件——这不是单文件原子操作。在 `lock: none` 下,两个进程并发 create 同名记录会双双通过检查,唯一性被破坏;且事后没有任何对账/修复机制(两个文件都合法存在,`record_list` 会如实展示两条同名记录)。

这直接击穿设计的第一卖点:§1.1 结论 4"机械约束比提示词纪律可靠"与 §6.2"查重是机制不是提示词"。机制层的硬保证恰恰在文档自己承诺的目标场景(多 Agent 并发)下不成立。

同一缺口还波及:

1. `record_create` 的 ID 存在性检查(§4.3)——若实现为普通"检查文件存在→写入",同样存在 TOCTOU;规格未要求使用原子独占创建(`O_EXCL`/`wx`);
2. **MEMORY.md 索引是全系统争用最高的单文件**(每次记忆写入都可能更新它),它走 `record_update`,而 memory 无锁、CAS 仅 optional——索引丢失更新是整个系统里概率最高的并发事故。

**为什么是阻断**:违反文档自身的核心承诺与目标场景;一旦唯一性被破坏无修复路径;修复成本极低(改一行配置或一段规格),没有理由带着矛盾开工。

**修复建议**(推荐 a + c 组合):

- a) memory 的 create 路径(create 的唯一性检查 + 写入)纳入 scope 级文件锁,与 todo 同款;锁窗口毫秒级,成本可忽略;
- b) 若坚持无锁:把 uniqueName 从"硬不变量"降级为"尽力而为 + 启动/定期对账检测重复并 nudge 修复",并在文档中如实降级该承诺;
- c) semantic-filename 的 ID 分配在规格中强制原子独占创建,把"ID 不撞"的原子性从锁中剥离(单文件原子,无需锁)。

### B2. slug 规格不完整:非 ASCII 归一化与 id 缺省行为未定义

**位置**:§4.3 ID 策略、§6.2 `record_create(scope, fields, body, { id?: string })`

**问题 1——中文名坍缩**。§4.3 规定"模型提供 slug 作为 id(server 做 `[a-z0-9-]` 归一化)"。本文档全文中文、示例 description 均为中文,目标用户显然包含中文场景。模型给出中文 slug(或按习惯从中文 name/title 派生)时,`[a-z0-9-]` 归一化会剥离全部 CJK 字符,得到空串或无意义 id;不同中文 slug 归一化后坍缩为同一 id。§1.2"数据是磁盘上人类可读可手编的文件"、§4.3"文件名即 id"对中文内容失效。

**问题 2——id 缺省行为未定义**。`record_create` 签名中 `id` 为可选参数,而 §4.3 说 semantic-filename"模型提供 slug"。两处矛盾:semantic-filename scope 下 `id` 到底必填还是可选?缺省时 server 从 name 派生(派生规则未定义)、拒绝、还是兜底生成?

**为什么是阻断**:memory 是 v1 默认 scope,semantic-filename 是它的 id 策略;这是实现第一天、中文用户第一次写入就会撞上的规格空洞,不先定义必然返工。

**修复建议**(三选一,写入规格):

- a) slug 允许 Unicode:仅排除路径危险字符与保留名,做 NFC 归一化(推荐——与"人类可读可手编"目标一致);
- b) 保持 `[a-z0-9-]`,但规定 semantic-filename 下 `id` 必填,Skill 明确教学"slug 用 ASCII 英文";
- c) `id` 缺省时 server 兜底生成(如短 hash),并在 nudge 中提示改名。

同时明确 `id` 必填性,以及保留 id(MEMORY.md)与 slug 的碰撞归一化规则(见 M6)。

---

## 二、高优先级(实现前必须决策)

### H1. CAS 以 mtime 为版本凭据不够稳

**位置**:§8 层 3、§4.2 `cas` 策略项

同一文件在同一文件系统时钟 tick 内两次写入,mtime 不变 → 持有旧 mtime 的客户端通过 CAS 误放行,丢失更新不可检测。§8 用"NTFS/ext4 精度足够"带过,但连续快速 `record_update` 是任务系统的常态调用模式,同 tick 连写并非纯理论;附录 A 还明确支持跨机共享("另一台机器上的同事 Agent"),时钟漂移进一步放大。wiki 的 `cas: required` 是强承诺,凭据本身应更稳。

**建议**:frontmatter 增加 server 维护的 `version` 整数(每次写入 +1;server 无状态、每次直读磁盘,与 P2/P6 相容),CAS 凭据改用 `ifMatchVersion`(或保留 mtime 为别名);至少在规格中写明同 tick 风险的量化接受理由。

### H2. MEMORY.md(model-maintained 索引)的维护协议缺失,索引必然漂移

**位置**:§10.2 memory Skill 写入协议、§6.3 nudge

Skill 写入协议 5 步中没有任何"create/update/delete 后同步更新 MEMORY.md"的步骤;nudge 也不覆盖索引提醒。索引只在模型"恰好想起"时更新。而 §10.2 的降级路径(库 >100 条时"先看 MEMORY.md 索引")恰恰依赖索引新鲜度——机制层(`index: model-maintained`)与约定层(Skill)之间出现职责真空,违反 P1 的分工完备性。

**建议**:create/delete/update 命中 memory scope 时,nudge 追加"MEMORY.md 需同步(本条已/未被索引引用)"——server 现算即可判断,纯机械;或 Skill 写入协议补第 6 步。

### H3. `data:` 与 `root:` 配置键二义

**位置**:§5 磁盘布局注释与 config.yaml 示例

布局注释说"scope 可用 `root:` 覆盖到任意路径",而示例中 todo/memory 用 `data: ./data/todo`,wiki 用 `root: ~/wikis/main`(且无 `data`)。同一概念两个键,语义未定义(`root` 是 `data` 别名?还是 scope 包根?相对路径相对于 MEMSTORE_HOME 还是 config 文件?)。

**建议**:统一为一个键(建议 `data:`),`~/` 展开与相对路径基准写进规格。

### H4. derived 索引(wiki INDEX.md)重建时机未定义

**位置**:§9.1"派生重建:derived 索引每次写入后现算"、§4.3 保留 id

"写入后现算"只覆盖 server 写入。wiki 的 root 指向外部目录(`~/wikis/main`),其核心使用方式恰是人手/外部工具编辑(P2)。人手编辑后 INDEX.md 保持陈旧,直到下一次 server 写入;而 INDEX.md 是保留记录、`record_get` 直读磁盘 → 模型读到陈旧索引,与"磁盘是唯一真相"相悖。

**建议**:`record_get` 命中 derived 索引时现算比对(mtime 扫描成本在 caps 内),不一致则重建后返回;或在规格中明确"derived 索引仅保证 server 写入后新鲜,人手编辑后陈旧属预期行为"。

---

## 三、中优先级(规格应补充)

| # | 问题 | 位置 | 建议 |
|---|------|------|------|
| M1 | strict 坏记录"拒读并警告"后无修复路径:`record_update` 需先读旧内容做浅合并,大概率一并拒绝,坏记录只能人手修文件——与 P2"人手编文件天然生效"的优雅故事衔接不上 | §4.2 | `record_update` 对"当前内容校验失败"的记录提供覆盖式修复路径(如全量替换 body+fields 的 force 模式) |
| M2 | todo 默认全局数据根:两个项目两个宿主实例共用同一 todo scope,任务互相污染;且 §7 中 todo 无 maxFiles 上限,而 §6.3 nudge 每次更新后全量扫描(O(N) 无上界),与上下文卫生哲学不一致 | §5、§6.3、§7 | todo 设宽松但存在的 maxFiles;安装器/README 明确 per-project data root 推荐配置 |
| M3 | Skill 引用了不存在的 filter 语义:§10.2 写"用 `filter={name-substring}` 查相似记录",但 §6.1 的 filter 是字段**精确匹配** | §10.2 vs §6.1 | 改为 `record_search`(子串扫描已覆盖 name);保持 filter 极简 |
| M4 | 409 NAME_CONFLICT 触发语义含糊:是精确同名还是相近?附录 A 示例中 409 返回的 "testing-notes" 与新建名并不精确同名;"最接近列表"算法未定义 | §6.2、附录 A | 精确同名才 409;近似建议归 nudge(§6.3 已有),两者分开;"最接近"算法(编辑距离/子串)写入规格 |
| M5 | sinceMtime 时间源未定义:附录 A 用"会话开始时间戳"(客户端时钟),mtime 是文件系统时钟,跨机场景时钟漂移会漏增量 | §6.1、附录 A | 增量游标取上次 `record_list` 返回中的最大 mtimeMs(server 时钟),写入规格 |
| M6 | 保留 id 在大小写不敏感文件系统上的碰撞:Windows 上 slug "memory" 归一化小写 → `memory.md` 与保留记录 `MEMORY.md` 是同一文件;目前只能靠 fs 存在性检查"碰巧"挡住 | §4.3 | 保留 id 的占用检查必须 casefold;禁用保留名及其大小写变体 |

## 四、低优先级(简列)

- **L1** header-only 扫描(前 30 行)未定义 name/description 落在窗口外(手编长 frontmatter)时 header 的降级显示(建议:回退显示文件名并标注);
- **L2** model-maintained 索引**写入侧**超限(maxIndexLines/maxIndexBytes)时 `record_update` 是 413 还是放行+警告未定义(§7 只定义了读侧);
- **L3** 409/413 应明确承载于结构化 tool result 而非 MCP error——冲突恢复需要 current/相近列表等结构化数据,多数客户端对 isError 的展示不利;
- **L4** `record_tree`"仅当 scope 声明 tree 时暴露"与 MCP 静态工具列表的关系(调用时报错 vs `tools/list_changed`)未说明;
- **L5** `.highwatermark` 写入与删除文件的崩溃顺序应写入 M1 验收(先写 watermark 再删文件,消除 ID 复用窗口);
- **L6** 索引保留记录的 id 形态不一致(示例混用 `MEMORY.md` 带扩展名与 `testing-notes` 不带扩展名),统一规则需写明。

---

## 五、参考源事实核对(全部一致 ✓)

| 文档声明 | 核对结果 |
|---|---|
| 文件锁退避参数 retries=30 / min=5ms / max=100ms | ✓ `src/utils/tasks.ts:102` LOCK_OPTIONS(retries:30, minTimeout:5, maxTimeout:100) |
| `.highwatermark` 防复用 | ✓ `src/utils/tasks.ts:92` |
| header-only 扫描、200 行索引上限 | ✓ `src/memdir/memoryScan.ts` scanMemoryFiles;`memdir.ts:35` MAX_ENTRYPOINT_LINES = 200 |
| truncateEntrypointContent 截断警告 | ✓ `src/memdir/memdir.ts:57` |
| MEMORY_DRIFT_CAVEAT / TRUSTING_RECALL_SECTION | ✓ `src/memdir/memoryTypes.ts:201,240` |
| verificationNudge | ✓ `src/tools/TodoWriteTool/TodoWriteTool.ts` |
| findRelevantMemories | ✓ `src/memdir/findRelevantMemories.ts` |

唯一轻微偏差:文档写 `utils/tasks.ts`,实际路径为 `src/utils/tasks.ts`,不影响结论。

## 六、值得保留的亮点

- **机制/约定分离(P1)+ 降级表(§3)**:"任何宿主可用"是真实承诺而非口号,MCP 为地板向上增强的兼容策略清晰;
- **非目标(§1.3)边界干净**:拒绝 embedding、raw 写、运行时 scope_register,每个拒绝都给了理由;
- **上下文卫生全部 server 强制(§7)且预算量化**,与参考源实测数据一致(200 行/25KB 来自真实 p97/p100 观察);
- **§8 第 4 层诚实承认语义冲突只能缓解**,不假装机械消除;
- 里程碑各自独立可交付,M1 验收即含并发锁测试与级联测试。

## 七、总结

| 维度 | 评价 |
|---|---|
| 架构方向 | 成立,无需推翻 |
| 阻断项 | **2 项(B1 并发矛盾、B2 slug 规格空洞)** |
| 高优先级 | 4 项(H1 CAS 凭据、H2 索引维护协议、H3 配置键二义、H4 derived 索引时机) |
| 事实准确性 | 参考源引用全部核实一致 |
| 建议 | 解决 B1、B2 后即可进入 M1;H1-H4 建议在 M1 开工前一并定稿(均属规格层,不增加架构复杂度) |
