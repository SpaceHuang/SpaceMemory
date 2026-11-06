# mem v1.1 实施评审——复审报告（R2，修复验证轮）

- 评审对象：首轮报告（`mem-v1.1-implementation-review.md`）B1–B3 / H1–H6 / M / L 全部发现的修复回应；修复声明见设计文档 v1.2 修订行
- 评审日期：2026-11-05
- 评审环境：同首轮（Windows 11，非提权用户进程）；`cargo check --workspace --all-targets` **零警告**、`cargo test --workspace` **137 项全绿**（首轮 121 → +16）
- 评审方式：逐项读码核对 + 新增测试核对 + 原复现场景端到端复验 + 快路径新增两组实验（B3 相关）

## 复审结论（TL;DR）

首轮 B1、B2 **修复验证通过**（代码、针对性新测试、原复现场景复验三者齐全）；H1–H6 全部落实；文档与状态声明同步到位，修复纪律好。**但 B3 的修复在验证过程中暴露出 1 个新的阻断项与 1 个高优先级问题**：

- **B4（新阻断）**：快路径的新鲜性"证明"用的是 sidecar **轮询缓存的未消化计数**（≤400ms 旧），不是咨询时刻的新鲜值——同尺寸保 mtime 修改落在轮询间隙内时，快路径会用旧索引应答（旧令牌/旧 FTS 结果）。这恰是红线21 立法要消灭的场景（"不承认任何'已知可能返回旧值'的快路径"），违反 §8-4 验收断言"第一次读取返回新值"的字面；
- **H7（新高优）**：Windows 非管理员进程无法打开卷设备句柄（`\\.\C:` GENERIC_READ 需提权）→ `UsnWatcher::attach` 失败 → **本机实测：sidecar 在场、检查点已写，read_mode 仍为 direct（reason"卷不支持变更日志订阅"）**——快路径与 FTS 在标准非提权 Windows 上永不生效。降级行为本身安全合法（D5），但能力矩阵（开放问题39）缺"非管理员"一行，sidecar/索引的实际收益面与声明不符。

注：因 H7，indexed 分支在**本机无法端到端触发**——B4 的结论基于代码路径推证（证据链见正文），fastpath e2e 测试在本机走的是 none 分支。这是复审的如实边界。

---

## 一、阻断项验证

### B1 撕裂尾行毒化 → ✅ 修复通过

**核对**：(a) `ReadLog` 新增 `good_bytes`（最后完整合法行界偏移，event.rs:158-160）；(b) `repair_torn_tail` 截断到 `good_bytes`（event.rs:324-335）——坏行无论是否带换行都被整体移除；(c) **两条追加路径统一**：`append_raw`（:305-308）与 `with_sequence_recheck`（:527-531）都调用同一修复；(d) 规范 envelope-format.md §4.2 同步修订 v1.0.1（"补 `\n`"表述废止，明确"截断到最后完整合法行界"）。
**新测试**：`torn_tail_with_newline_repaired_on_append_path`、`torn_tail_repaired_on_reconcile_path`、`mid_file_bad_line_stays_loud_not_silently_repaired`（backfill.rs:417/443/470）——正是首轮点名的三个盲区组合。
**端到端复验**（本机，新二进制）：坏行带换行 + reconcile → 追加成功、diff 正常、update 成功（v2 令牌）、events.jsonl 终态 3 行且坏行已移除；无换行对照同样正常。**首轮的"全店写路径瘫痪"场景不复现。**

### B2 fix 静默丢正文 → ✅ 修复通过

**核对**：`split_body_bytes` 改调 `record::find_body_after_first_delimiter`（ops.rs:1062-1064）——与 `split_frontmatter` 共用同一边界定义（首个 `---` 至下一个 `---`），单源无漂移。
**新测试**：`fix_keeps_body_bytes_when_body_contains_horizontal_rules`（maintenance.rs:331）——用首轮的原始复现文件断言正文字节全保。
**端到端复验**：同首轮场景，fix 后正文完整保留 `# 标题段落 … 结尾段落（必须保留）`。**通过。**

### B3 快路径整链未接线 → ⚠️ 结构性修复完成，但暴露 B4/H7（见下）

**已核实**：(a) `mem serve` 注入真实物化器（serve.rs：`Index::rebuild` + `ensure_fts` + `sync_fts`）；(b) 物化在**前**、检查点在**后**（sidecar.rs:221-233，崩溃安全顺序正确）；(c) CLI list/find/view 走 `fastpath()` → `consult_with_hello`，证明成立时查索引并输出 `read_mode: indexed`；(d) 索引行携带真实全摘要（sqlite.rs:105，digest 来自扫描令牌），快路径响应含完整令牌；(e) 直读/快路径排序一致（modified DESC, id DESC 两处相同）；(f) fastpath.rs 从生产二进制 e2e 验证物化与 read_mode 标注、空闲自退回落 direct。设计文档 v1.2 已裁决"方案 a 补接线"并归档。**接线与声明问题就此关闭。**

---

## 二、新发现（阻断 1 项 + 高优 1 项）

### B4（新）. 快路径证明使用轮询缓存的 undigested 值——红线21 咨询时刻语义未成立

**位置**：`crates/mem-core/src/sidecar.rs:192`（握手应答携带循环缓存的 `undigested_now`）、`sidecar.rs:265-274`（`try_accept_and_reply` 不做新鲜查询）、`crates/mem-core/src/freshness.rs:147-174`（`consult_with_hello` 把 Hello 值当作证明）

**问题**：serve 循环每 400ms 轮询一次 USN 并缓存 `undigested_now`；CLI 咨询时经管道握手拿到的这个值**至多 400ms 旧**。`consult_with_hello` 以"Hello.undigested == 0 ∧ stat 一致"判快路径——两个条件都可能以咨询时刻之前的状态通过：

1. 修改发生在上一轮轮询之后（同尺寸 + 恢复 mtime → stat 不变，USN 已记录但未轮询到）；
2. 此时 CLI 咨询：Hello.undigested = 0（缓存）、stat 匹配检查点 → **Indexed → 以旧索引应答**（旧 digest 令牌、旧 FTS 命中/漏报）——直到下一轮轮询触发维护为止。

这正是红线21 明文禁止的"已知可能返回旧值"的快路径，也违反 §8-4 验收断言（"三场景×三条件第一次读取返回新值"——同尺寸保 mtime 是三场景之一，此处恰在订阅在场的条件下失败）。现有 fastpath e2e 与 freshness 验收测试都测不到它：前者只测稳态命中，后者的"订阅在场"用例用测试替身（acceptance.rs:280 的 OkSub 恒 Some(0)），都不覆盖"修改落在轮询间隙"。

**为什么定阻断**：红线21 是本设计五轮评审的核心成果（v0.7 B1 的直接产物）；修复后的代码在**启用快路径的条件下**重新引入了它立法消灭的缺陷类别。修复本身是小改：serve 循环在应答握手前先做一次新鲜 `undigested_changes()`（查询与应答在同一循环体内，watcher 就在手上），若 >0 先跑维护再应答。

**如实标注**：受 H7 影响，本机（非提权）快路径不启用，B4 当前无从实际触发——定级基于启用条件下的代码语义，不基于本机复现。

### H7（新）. 非管理员 Windows 上 USN 挂接失败，快路径与 FTS 永不生效——能力矩阵缺口

**证据**（本机实测）：sidecar 在场、index/checkpoint.json 与 index.sqlite 均已产出，`mem list` 输出 `"read_mode": {"direct": {"reason": "卷不支持变更日志订阅（永久直读形态）"}}`；当前进程非提权（`net session` 拒绝）。机理：`UsnWatcher::attach` 经 `CreateFileW(GENERIC_READ)` 打开 `\\.\C:` 卷设备——Windows 对非管理员进程返回 ACCESS_DENIED（读 USN journal 需管理员权限），attach 返回 None → backend=none。

**定性**：降级行为本身安全（直读 + 如实标注，正确性无损，D5 明文合法）。问题在于：(a) 开放问题39 的能力矩阵未记"Windows 非管理员 → USN 不可订阅 → 永久直读"这一**最常见形态**——矩阵目前的排除项只写了 exFAT/网络盘/容器挂载；(b) 实际后果是 sidecar + 索引 + FTS 的全部性能收益在标准用户环境为零，用户无从知晓；(c) fastpath e2e 测试按 backend 分支断言，在本机永远走 none 分支——快路径分支的回归保护在 CI 上是空的。

**建议**（需用户裁决）：方案一，接受现状并把"非管理员 = 永久直读"写入开放问题39 矩阵与 README（声明义务）；方案二，为非管理员形态增加 `ReadDirectoryChangesW` 订阅源（设计需修订"notify 非新鲜度依据"的既有裁决——ReadDirectoryChangesW 的丢失语义需先论证能否作为证明来源）；方案三，文档化"sidecar 以管理员/服务身份运行"的部署要求。

---

## 三、高优先级验证（H1–H6 全部通过）

| # | 验证结果 |
|---|---|
| H1 fix CAS | ✅ `fix_record` 增 `expect` 参数；锁内读（ops.rs:990-993）；可解析记录**强制** expect（:998-1004，缺失即拒），损坏记录豁免且有理由注明；CLI `--expect` 旗标接线（cli.rs:182）；新测试 `fix_on_parseable_record_requires_expect_token`（maintenance.rs:439） |
| H2 rename 双侧提交 | ✅ delete `[旧rel, attic]`（ops.rs:636-637）、restore `[attic旧, records]`（:699-700）、merge touched 含 dup records 侧（补齐）；注释标注 H2 |
| H3 deep 审计 | ✅ `deep_digest_audit` 改为**磁盘摘要 vs 事件日志最后 new_digest**（ops.rs:1078-），三处调用点（reconcile/verify/agenda）同源；无事件者如实报"创建事件缺失" |
| H4 apply 独占门闩 | ✅ apply 持 `barrier_exclusive` 贯穿全部步骤（agenda.rs:286）；step 调 `merge_locked/delete_locked/update_locked`——已核对这些变体**不取门闩**（公共包装器取后委托），记录锁/序列锁照取，无同线程 RwLock 重入死锁 |
| H5 find 截断标注 | ✅ 直读路径 `truncated` + resume（remaining_after_stop 精确计数）；快路径 `truncated: rows.len() == limit`（恰好等于 limit 时保守多报，红线9 兼容方向） |
| H6 跨主机锁 | ✅ owner.host ≠ 本机 → 视为存活永不抢（lock.rs:250-263）；判死后 25ms 退避不再紧密自旋；新测试 `foreign_host_lock_is_never_stolen`（locks.rs:206） |

## 四、中低优先级验证

| # | 验证结果 |
|---|---|
| M1 initialize 不建台账 | ✅ 按用户裁决归档进设计文档 v1.2（红线25 优先于 D13.1 字面），mem-mcp 文档注释同步 |
| M2 init 建 git | ✅ `git_initialized` 字段入 init 输出 |
| M3 锁外校验 | ✅ 裁决归档（"锁外尽力+锁内复核+verify 兜底"如实注记） |
| M4 MCP payload/body | ⚠️ **部分修复**：`mem_add` payload 已必填、`body_b64` 字节保真通道已加（protocol.rs:312-317，含手写 base64 解码）；但 **`mem_update` 的 payload 仍是 `unwrap_or_else(\|\| json!({}))` 容忍缺省**（protocol.rs:196）——lenient 类型下省略 payload 仍会静默清空载荷。残留小缺陷 |
| M5/M11 O(C×E) | ✅ 归档开放问题29（方向裁决） |
| M6 读路径落盘 | ⚠️ **M6c 已修**（temp 名改为 `<name>.tmp-<pid>-<ns>`，不再以 .md 结尾落 records/ 树内）；**M6a（.tmp 探针）/M6b（brief 建 sessions/ 目录）未改且未见裁决记录**——建议补归档（方向安全，但按决策落档纪律应有明文） |
| M7 aliases | ✅ 非字符串元素响亮拒收（envelope.rs，R6.1 注记） |
| M8 AGENTS.md | ✅ 已刷新（"v1.1 已实施＋首轮实施评审修复完成"）。小瑕疵：仍指向"docs/review/mem-v1.1-implementation-review.md 的修复回应"——修复回应实际在**设计文档 v1.2 修订行**，指针应改指设计文档 |
| M9 零警告 | ✅ cargo check 零警告（本机验证） |
| M10 截断 off-by-one | ✅ drift 改"全量收集后截断"（drift.rs:130-133），与 diff_since 对齐 |
| L1 键序守卫 | ✅ foundation.rs:244 起新增守卫测试（preserve_order 启用即红） |
| L2/L3/L6 | ✅ write_ctx 残渣清理；`dag() \|\| true` 死代码已清；漂移标注已改 |

**新记录的低优**：L7——sidecar.rs:185 的 `undigested_now == Some(1_u64 << 62)` 是死条件（右侧 `n > 0` 已覆盖），疑似重构残渣，建议删除；L8——tools.rs 的 inputSchema 未宣传 `body_b64` 参数（协议接受但客户端不可见）。

## 五、测试与验收核对

- 137 项全绿（+16）：B1 三盲区、B2 正文含分隔线、H1 fix CAS、H6 跨主机锁、L1 键序守卫、fastpath e2e 均有针对性新测试；
- **fastpath e2e 的分支性**：按 backend 断言（usn→indexed / 其他→direct）——本机因 H7 走 none 分支，**indexed 分支未在本机执行过**；其行为仅有代码阅读与单测（fake Subscription）背书。建议 CI 明确记录跑在哪类环境，或补一条提权环境的显式外部验收项（docs/rules/document-writing.md §3 门禁二分：本机执行不了的门禁是外部验收项）；
- B4 的"轮询间隙修改"场景无测试覆盖（修复时应随 B4 一并补：sidecar 在场 + 同 stat 修改 + 立即读 → 必须返回新值）。

## 六、自曝三处（请评审人优先核查本报告最可能错的地方）

1. **B4 未在本机实际复现**（受 H7 遮蔽）——结论依赖"Hello 携带缓存值 + consult 信任之"的读码链（sidecar.rs:192/265、freshness.rs:164-168）。若存在我未发现的机制（如管道请求触发新鲜查询），此条撤销；请以提权环境的一次实验复核；
2. **H7 的根因归因**——"非管理员 → 卷句柄 ACCESS_DENIED → attach None"是 Windows 文档行为 + 本机现象（reason 字段佐证）的合成推断，未做提权对照实验；若 attach 失败另有原因（如路径形态），结论方向不变但机理描述需修正；
3. **B3 各分项的"通过"判定**——索引物化顺序、digest 来源、排序一致性均为读码核对 + 本机部分验证（index.sqlite 产出、direct 标注）；indexed 分支的查询正确性（分页等价性等）本机未验证。

## 七、总结

| 维度 | 评价 |
|---|---|
| 首轮阻断 B1/B2 | **修复验证通过**（代码 + 测试 + e2e 复验） |
| 首轮阻断 B3 | 结构性修复完成并归档裁决；衍生 B4（证明用缓存值）/H7（非管理员失效） |
| 首轮 H1–H6 | 全部通过 |
| 首轮 M/L | 大部分通过；M4 残留半项（update payload），M6a/b 未处置未归档，指针小误 |
| 新阻断 | **1 项（B4）**——启用快路径条件下的红线21 违反；因 H7 在标准环境暂无从触发，可与 H7 合并修复后再启用快路径 |
| 建议 | B4 + H7 同批处理（serve 应答前新鲜查询 + 能力矩阵补行或部署要求文档化）后再考虑发布；M4 残留与 L7/L8 顺手清 |

---

### 待批准清单

**本轮批准范围**：首轮 B1/B2/H1–H6 修复的验收通过；B3 结构性修复的验收通过与 B4/H7 的发现及定级；M/L 各项的处置核对结论。
**本轮明确不批**：B4 的修复方案与 H7 的路线选择（声明义务 / ReadDirectoryChangesW / 提权部署）——需用户裁决；快路径在 B4 修复前的启用状态（当前因 H7 事实未启用，属安全侧）。
