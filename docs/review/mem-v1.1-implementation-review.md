# mem v1.1 实施评审报告（代码评审）

- 评审对象：全工作区实现（crates/ 七个 crate，约 8500 行 Rust；设计文档 v1.1 声称 §8 第 1–8 步全部落地）
- 评审日期：2026-11-05
- 评审环境：Windows 11（win32 10.0.26200），`cargo check --workspace --all-targets` 通过（3 处警告）、`cargo test --workspace` 全绿（约 121 项，含 cli e2e 7 项、acceptance 9 项、backfill 17 项）
- 评审标尺：`agent-memory-requirements.md` v1.2、`agent-memory-design.md` v1.1（D1–D19、红线 1–27、§8 验收清单）、`docs/specs/` 三规范
- 评审方式：全部源文件精读 + 两条阻断项用已构建二进制在临时目录端到端复现（复现命令附各条目内）

## 评审结论（TL;DR）

整体工程质量高于常见水准：锁协议、状态三元组回填、absent 稳定态、并发回填去重、令牌 CAS、字节保持正文、预算标注等核心正确性面实现与设计高度一致，测试真实覆盖了设计验收清单的绝大多数条目。**但存在 3 个阻断项**——两条已端到端复现的数据级缺陷（事件日志毒化、fix 静默丢正文），一条"完成声明与实现不符"（索引快路径整链未接线）：

- **B1**：撕裂尾行的两种处理路径都存在缺陷，规范 §4.2 自身措辞矛盾——任一坏行（崩溃撕裂或手改）经 `reconcile` 追加后，events.jsonl 永久"中部损坏"，**全店写操作与 diff 硬错误，且无任何接口修复通道**（已复现）；
- **B2**：`mem fix` 重建 frontmatter 时正文切分取**最后一个** `---`（规范 D18 是"至下一个 `---`"），正文含水平线的损坏记录会被**静默截断正文**（已复现，直接违反红线27 的字节领土承诺）；
- **B3**：索引/新鲜性快路径**整链未接线**——mem-index 无任何生产引用、`mem serve` 不物化索引（sidecar.rs 注释指向的 Maintainer 回调不存在）、所有读路径 `consult` 的订阅参数恒为 `None` → 永远直读。正确性无损，但设计文档"§8 第 6 步 ✅ 完成"的声明不成立，sidecar 实际是"只写无效检查点的空转进程"。

解决 B1、B2 前不应继续积累数据或发布；B3 需在下一轮规划前裁决（补接线，或如实降级声明）。

---

## 一、阻断项（必须先解决）

### B1. 撕裂尾行处理毒化事件日志：一次坏行 = 全店写路径瘫痪

**位置**：`crates/mem-core/src/event.rs:500-504`（reconcile 路径，补 `\n` 不截断）、`crates/mem-core/src/event.rs:302-310`（`repair_torn_tail`，截断点取文件最后一个换行）；规范源头 `docs/specs/envelope-format.md` §4.2（"先补 `\n` 再写新行"与"视为截断"自相矛盾）

**问题**：设计约定"末行 JSON 不可解析 → 截断，对应记录由回填覆盖"（D12）。实现分裂在两条追加路径上，且各有缺陷：

1. **reconcile 路径（`with_sequence_recheck`）从不截断**——检测到撕裂尾行时仅给新行前置 `\n`。无论坏行原本有没有换行符，追加后坏行都变成"中部行"；下一次 `read_log` 对中部坏行的语义是**响亮错误**（`event.rs:198-201`）。即：**任何**一次撕裂尾行（崩溃半行、或用户手改出坏行）之后跑一次会追加的 reconcile，事件日志即永久损坏。
2. **普通追加路径（`append_raw` → `repair_torn_tail`）的截断点取文件最后一个 `\n`**——坏行自带换行符时（手改坏行、或写 tearing 恰好落满一行），截断保留坏行、追加其后，同样制造中部坏行。

单元测试 `torn_tail_is_truncated_and_reported`（backfill.rs:87）只覆盖"无换行半行 + `append_simple`"这一种组合，掩盖了两处缺陷。

**复现**（Windows，已验证；`$STORE` 为 `mem init` 新店）：

```
printf '{"seq":999,"ts":"20\n' >> $STORE/events.jsonl   # 坏行（带换行）
printf '\nhand-edit\n' >> $STORE/records/principal/main.md  # 制造漂移
mem --store $STORE reconcile    # 成功，backfill_appended: 1 —— 坏行被保留
mem --store $STORE diff --since 0
# => {"error":"记录损坏（events.jsonl）：第 2 行不是合法事件（中部损坏，响亮报告）…"}
mem --store $STORE update …     # => 同上错误；add/delete/reconcile 全部同错误
```

对照组：坏行**不带**换行符，经 `append_simple`（add/update 的事件路径）可正确截断；但经 reconcile 路径同样毒化（已实测）。坏行入体后无接口修复通道——`mem fix` 只针对记录文件，events.jsonl 只能手工编辑。

**为什么是阻断**：(a) 触发条件是设计明文要求可恢复的场景（崩溃撕裂是 R3.1 的既定输入，手改是 R2.4 的合法输入）；(b) 后果是全店写能力与增量读取瘫痪，爆炸半径 = 整店；(c) 规范 §4.2 自身矛盾，实现两路径不一致，说明该语义从未被完整验收；(d) 修复成本低（统一截断到"最后一个完整行界"，同步修规范一句）。

**修复建议**：`read_log` 返回最后完整行界的字节偏移；两条追加路径统一"先截断到该界、再追加"（无论坏行是否带换行）。规范 §4.2 改为"追加例程先把文件截断到最后一个完整行界再写新行"，删去"补 `\n` 保行界"表述。补三组测试：坏行带换行 × 两条追加路径、reconcile-on-torn-tail。

### B2. `mem fix` 正文切分取最后一个 `---`：修复通道静默丢失正文

**位置**：`crates/mem-core/src/ops.rs:1001-1022`（`split_body_bytes`）

**问题**：D18 规定 frontmatter 区域 = "第一行恰为 `---`，至**下一个** `---`"，正文 = 其后全部字节。`split_body_bytes` 在 frontmatter 损坏（无法走正常解析）时，取**全文件最后一个**独立 `---` 行之后的内容当正文。正文自身含水平线（Markdown 极常见）时，水平线之前的正文被静默丢弃——修复操作本身造成数据丢失，且无任何告警。

**复现**（已验证）：损坏文件 `---\n{ broken json\n---\n# 标题段落\n\n---\n\n结尾段落（必须保留）\n`，执行 `mem fix records/memory/broken.md --envelope … --payload '{"title":"broken-rec"}'` 后，文件正文只剩 `结尾段落（必须保留）`——`# 标题段落` 与第一道水平线之前的全部字节丢失。

既有测试 `fix_rebuilds_frontmatter_from_git_keeping_body_bytes`（maintenance.rs:290）的正文不含 `---`，未覆盖此场景。

**为什么是阻断**：fix 是 R6.3 的唯一接口修复通道，红线27 明文"正文是人/Agent 的字节领土"；修复通道自身静默吞字节，恰是本项目五轮评审全力消灭的"静默失败"形态出现在最不该出现的位置。

**修复建议**：`split_body_bytes` 改用与 `split_frontmatter` 相同的边界（首个 `---` 行至下一个 `---` 行），即取**第二个** `---` 之后；与 `split_frontmatter` 共用同一寻行逻辑避免双源漂移。补测试：损坏 frontmatter + 正文含 `---`。

### B3. 索引快路径整链未接线，"§8 第 6 步完成"声明不成立

**位置**：`crates/mem/src/serve.rs`（无 Maintainer 注入）、`crates/mem/src/commands/mod.rs:332` 与 `crates/mem-mcp/src/protocol.rs:78`（`consult(…, None)` 订阅恒为 `None`）、`crates/mem-core/src/sidecar.rs:203-212`（注释声称"索引物化由 mem bin 注入（见 mem::serve 的 Maintainer 回调）"——该回调不存在）

**问题**（三点事实链，均已核实）：

1. **mem-index 无任何生产引用**：全仓 grep，`Index::open/rebuild/query_by_type/neighbors/search`（FTS）仅被 `crates/mem-index/tests/index.rs` 引用。index.sqlite 在生产路径上从不物化、从不查询。
2. **读路径永远直读**：CLI 与 MCP 的 brief 调 `consult(store, checkpoint, None)`——订阅参数恒 `None`，返回恒为 `Direct{reason: "sidecar 不在"}`；list/find/show/trace 压根不经过 consult，直接 `scan_all`。快路径分支只有测试替身在单测里走过（acceptance.rs:280 用假 Subscription）。
3. **sidecar 实际职责只剩**：stat 轮询 → 写检查点 + 维护 USN 订阅（其结果只进握手应答 Hello.backend，无消费者）。USN watcher 在 serve 循环里推进游标并触发"重建"，但重建函数只刷新检查点、不重建索引——检查点成了无人消费的持续写盘产物。

**后果**：正确性无损（直读永远返回新值，红线21 恒满足——freshness 九项验收测试全绿正是因为全直读），但：(a) 设计文档 v1.1 修订行"§8 第 1–8 步全部落地"、状态行"6. ✅ sidecar + 索引 + 订阅（mem-index sqlite/FTS/检查点…）"与实现不符——按 `docs/rules/document-writing.md` §3 的状态标记纪律，`[x]` 要求"产物 + 验证证据齐备"，此处证据不存在；(b) 若按此状态进入打包发布（docs/rules/packaging.md），发布物中的"性能形态 sidecar"名不副实；(c) D5 的"FTS 仅快路径可用"退化为"FTS 永不可用"，而没有任何界面告知这一退化。

**为什么是阻断**：不是代码缺陷而是**状态虚报**——后续所有规划（性能基线、打包、§8-9 生态位）都建立在"第 6 步已完成"之上。与 memstore 评审 B1 同类：声明与实现自相矛盾时，修复成本远低于带着矛盾开工。

**修复建议**（二选一，需用户裁决）：(a) 补接线——serve 循环注入真正的索引物化（sqlite rebuild → 检查点**最后**落盘，顺序保证崩溃安全），CLI/MCP 读路径接 sidecar 握手（Hello 携带 undigested 计数或经 pipe 查询）；(b) 如实降级——v1 承认"索引与快路径为未启用组件"，设计文档状态行改为未完成，sidecar 撤下检查点写盘。无论选哪条，需同步修正 sidecar.rs 的失实注释。

---

## 二、高优先级（修复 B 项后应尽快处理）

### H1. `fix_record` 全程无令牌 CAS，且锁外读、锁内写（红线19 字面违反）

**位置**：`crates/mem-core/src/ops.rs:918-998`

fix 是对整文件的读改写：`bytes` 在取锁**前**读取（:942），锁内**不重读**直接以旧字节派生的 envelope/payload 覆写（:982-984）。并发窗口内的 update 成果会被 fix 静默覆盖（版本回退、载荷丢失），事件链表现为 fix 的 old_v 异常——chain_audit 事后可检出，但内容已丢（R3.2 静默丢失）。红线19"一切读改写必须携带修订令牌并在锁内比较"，fix 无 expect 参数、无比较。建议：frontmatter 可解析时强制 `--expect` CAS；损坏时锁内重读正文再重建（正文是保留部分，必须取最新字节）。

### H2. 三类改名提交不含旧路径，git 历史与真相层长期分叉

**位置**：`crates/mem-core/src/ops.rs:614`（delete 只提交 attic 路径）、`:667`（restore 只提交 records 路径）、`:884-885`（merge 的 touched 不含 dup 的 records 侧路径）

delete/restore/merge 都是 `rename` 语义，但提交批次只含目标侧路径。`git commit -- <pathspec>` 取该路径的工作树状态——旧路径的删除不在 pathspec 内、永不入提交：HEAD 里 records/ 下的已删文件永远存在，worktree 永久脏，D4 的"git 承担历史/审计/撤销"在这三类操作后失真（fix 的 HistoryStore 会从 HEAD 里读到已被删除的旧路径内容——碰巧有用，但语义是错的）。修复：批次含双侧路径（delete → `[旧 rel, attic rel]`，restore 反之，merge 补 dup 的 records 侧）。

### H3. `verify --deep` / `reconcile --deep` 的摘要审计是自比自，检不出任何东西

**位置**：`crates/mem-core/src/agenda.rs:354-362`、`crates/mem/src/commands/mod.rs:492-508`、`commands/mod.rs:911-918`（三处同型）

"全量内容摘要核对"实现为：`scan_all` 时算一次 digest，再读同一文件算一次 digest，两者比较。文件未变则恒等——除非校验过程中文件正被改（竞态误报），**任何保版本手改、任何漂移都检不出**。设计意图（D5：维护审计例程）应对照的是事件日志的最后 new_digest。当前实现给用户"deep 已核对"的假安心。修复：比对 `sha256(当前文件) != 最后事件 new_digest`（漂移检出本已由 reconcile 承担，deep 至少应做"事件后状态 vs 磁盘"的全量复核并在报告中如实区分两者）。

### H4. `agenda::apply` 未持独占门闩执行计划（红线20/D17 字面违反，帮助文本同样失实）

**位置**：`crates/mem-core/src/agenda.rs:268-341`

D17 维护流程 = "独占门闩 → 枚举 → 逐记录 CAS → … → 单提交 → 释放"。实现中 apply 自身不取门闩，每个 step 调用的 ops 各自取放（merge 步独占、update/delete 步共享）——**步与步之间普通写可插入**，多步计划不具维护原子性（executing 标记只兜崩溃，不兜交错）。CLI/MCP 帮助文本"独占门闩内逐记录令牌 CAS"（cli.rs:170）与实际不符。修复：apply 持 `barrier_exclusive`，steps 改为不自行取门闩的内部变体（或如实改文档：计划 = 逐操作执行、非原子会话，需用户裁决）。

### H5. `mem find` 截断无标注、无续读路径（红线9 字面违反）

**位置**：`crates/mem/src/commands/mod.rs:542-568`

命中数达到 limit 即静默 `break`，输出只有 `"limit"` 字段，无 `truncated` 标注、无续读方式。红线9："预算截断必须显式标注并给出续读路径，不得静默丢弃。"修复：输出加 `truncated`/`resume`（find 的扫描序是枚举序，可给"已扫描前缀"提示或按 rel 游标续读）。

### H6. 陈旧锁判定对跨用户/跨主机场景不安全，且判死后忙等自旋

**位置**：`crates/mem-core/src/lock.rs:246-262`（存活判定不看 owner.host）、`platform/windows.rs:161-179`（`process_alive` 对 ACCESS_DENIED 保守判死）、`lock.rs:251-253`（判死后 `continue` 无退避）

owner JSON 携带 `host` 字段但只用于错误信息；`process_alive(pid)` 在**本机**查询——锁由他机进程持有（多机同步是 D4 既定场景）或本机他用户进程持有（OpenProcess 可能 ACCESS_DENIED → 判死）时，实现会覆盖 owner 文件并立即重试 `try_acquire`；真实 OS 锁未释放则 `WouldBlock` → 再读 owner → 再判死 → **紧密自旋**直到超时（默认 30s，全程占满一个核），且 owner 文件被污染（LockTimeout 报告的持有者信息错误）。单机单用户下不触发；按红线22"PID 死亡即抢"的字面实现与"多方共享"目标场景冲突。修复：owner.host ≠ 本机 → 视为存活（只等待）；判死分支加退避 sleep。

---

## 三、中优先级（记录在案，修复时顺手）

| # | 问题 | 位置 | 说明 |
|---|------|------|------|
| M1 | MCP initialize 不建台账，与 D13.1 及 mem-mcp 自身文档矛盾 | protocol.rs:38-48 vs lib.rs:4-5 | 实现选择（身份不可从协议噪声推断）合理，但 D13.1 写明"MCP initialize / mem session start"双触发器；需设计侧裁决并同步两处文档 |
| M2 | `mem init` 不建 .git，git 基座默认缺席 | commands/mod.rs:233-320 | 无 .git 时提交静默跳过（合法降级），但 fix 的历史通道默认不可用、envelope.commit 永远 null；应在 init 时 git init（或明示文档） |
| M3 | add/link 的校验（x-ref 存在性、DAG 预检）在锁外，TOCTOU 为 advisory | ops.rs:202-268、:443-445 | DAG 预检对不相交记录对的并发 link 不充分（注释已承认 verify 兜底）；红线17 的"强制"实为"尽力 + 审计"，应如实写进设计 |
| M4 | MCP `mem_update` 容忍 payload 缺省为 `{}`；body 经 JSON 字符串往返 | protocol.rs:194-196、:182 | lenient 类型下缺省 payload 会静默清空载荷；非 UTF-8 正文经 `from_utf8_lossy` 往返破坏字节（红线27 的 MCP 通道缺口） |
| M5 | 回填/漂移 O(候选×事件)：每候选在序列锁内重读全量 events.jsonl | event.rs:447-449、drift.rs:55 | 万级记录 + 十万级事件时 reconcile 明显塌陷；应记入开放问题29 性能基线 |
| M6 | 读路径落盘三处（红线18 字面违反，方向安全） | store.rs:225-252（.tmp 探针）、session.rs:80-83（brief 建 sessions/ 目录）、platform/windows.rs:190-198（temp 名以 .md 结尾落在 records/ 树内） | 前两处为能力探测/兜底，不碰真相数据；第三处使并发写窗口内 temp 文件被枚举为坏条目（告警噪音 + stat 闪烁，安全方向）——temp 改非 .md 后缀即可 |
| M7 | aliases 非字符串元素静默变 `""` | envelope.rs:93-95 | R6.1 要求可识别；应响亮拒收 |
| M8 | AGENTS.md 严重过时 | AGENTS.md:6、:31 | "脚手架完成，规范起草前"、"acceptance.rs 当前为占位"与实情（v1.1 声称八步全落地、acceptance 392 行 9 项测试）矛盾，误导后续 Agent 导航 |
| M9 | `cargo check` 3 处警告 | mem-core/tests/locks.rs:169,195、mem-mcp/tests/mcp.rs:117 | 违反 AGENTS.md §6"零警告"交付门槛；`cargo fix` 可清 |
| M10 | `scan_current_drift` 截断标注 off-by-one | drift.rs:112-114 | 恰好等于 limit 时误报 truncated（保守方向，红线9 兼容）；diff_since 的同类逻辑是正确的，两处对齐即可 |
| M11 | reconcile 每记录在记录锁内再取序列锁读全量日志——锁序正确但粒度粗 | event.rs:424-435 | 与 M5 同源；对齐为"一次快照 + 锁内复核增量"可消 |

## 四、低优先级（简列）

- **L1**：确定性序列化隐含依赖 serde_json 未启用 `preserve_order`（已验证当前依赖树无人启用）——一旦未来某依赖打开该 feature，frontmatter 键序与全部摘要翻转。建议加守卫测试：断言序列化样例键序为字母序（record.rs:30、envelope.rs:135、event.rs:75 三处注释均依赖此假设）；
- **L2**：`commands/mod.rs:613-647` `write_ctx` 脚手架残渣：每次调用 `Box::leak` 泄漏一个 Mutex、未使用的裸指针与自相矛盾的 SAFETY 注释；功能上被 `LeakSink`/`CURRENT_STORE` 兜住，应重写为直接取 `sink_for_current`；
- **L3**：`refgraph.rs:35` `decl.dag() \|\| true` 恒真，DAG/一般环区分实际失效（agenda 对所有 rel 都报环——行为可接受，代码意图不清）；
- **L4**：`envelope.commit` 字段从未被写入（D18 声明"来源提交 SHA"）；
- **L5**：`fold` 用 `to_lowercase` 近似 casefold 已在 paths.rs:39 如实声明且有原子独占兜底——留档即可；
- **L6**：drift note 分类在"records↔attic 侧变化"时报"未登记变更"，信息性偏差（drift.rs:79-91）。

---

## 五、测试与验收核对（对照设计 §8 验收清单）

| 设计验收条目 | 测试 | 结果 |
|---|---|---|
| freshness 三场景 × 三条件 | acceptance.rs 6 项 + proof 单项 | ✅ 通过——但注意全部由"恒直读"天然满足（见 B3） |
| 回填四注入（v1 崩溃/中间空洞/保版本手改/撕裂尾行） | backfill.rs | 前三类 ✅；**撕裂尾行只测了 append_simple 路径的无换行形态**（B1 缺口） |
| 外部删除/重命名/删除后重建/只读外部删除 | backfill.rs | ✅ 全覆盖且断言到位 |
| absent 稳定态（两次 reconcile + 两次 diff 收敛） | backfill.rs:217 | ✅ |
| 回填并发三态（暂停→update→恢复/双并发/三方交错） | backfill.rs 3 项 | ✅ 真实线程 + 通道暂停点，质量高 |
| 增量两轮（外部 git 提交出现、无 .git 成立） | acceptance.rs:322、backfill.rs:444 | ✅ |
| fix 正文按字节保持 | maintenance.rs:290 | ⚠️ 未覆盖正文含 `---`（B2 缺口） |
| 同令牌并发更新一成一冲突 | ops.rs / cli.rs e2e | ✅（cli.rs:113） |
| brief 永不建台账 | acceptance.rs:380、cli.rs:85 | ✅ 双层覆盖 |
| 索引快路径服务读 | mem-index tests 4 项（孤立） | ❌ 无生产接线验证（B3） |

**结构性缺口**：撕裂尾行 × reconcile 路径、fix × 正文含分隔线、快路径端到端——恰是三个阻断项各自的盲区，说明测试面跟着实现走、未对照规范穷举边界。

## 六、值得保留的亮点

- **并发回填的锁内复核纪律执行到位**：`with_sequence_recheck` 在序列锁内重读最后事件 + 锁内重查当前状态（含"是否已被重建/恢复"），三方交错测试（backfill.rs:360）是真实竞态编排而非形式断言；
- **absent 一等状态与三元组状态机**实现干净，`triple_from_event` 对 external_delete/受管 delete 的"事件后状态"区分与设计逐字对应；
- **锁的进程内注册表 + 平台字节范围锁双层结构**正确处理了 Windows（按句柄）/macOS（按进程）语义差异，锁序注释与实现一致；
- **mem-assets 对红线1 的执行**（只搬运字节、永不 parse）与混合分发裁决（外部目录优先、内嵌兜底）落地如声明；
- **mem-schema 的分档校验**（strict 拒收 / lenient 告警 + 未知字段容忍）与 R8.6 逐条对应，坏声明隔离（TypeSet.broken）不拖垮整体。

## 七、自曝三处（请评审人优先核查本报告最可能错的地方）

1. **B1 的"reconcile 路径连无换行撕裂也毒化"**——我读码得出 `with_sequence_recheck` 只补 `\n` 不截断，并已实测对照组毒化；请核对该路径是否在任何分支调用了 `repair_torn_tail`（我确认没有，但它藏在与 `append_raw` 不同的函数里，是最可能看漏的一处）；
2. **B3 的"整链未接线"**——依据是全仓 grep 无生产引用 + serve.rs 无 Maintainer + consult 恒 None 三点；若存在我未发现的动态装载或计划中的第二入口（如待合并的分支），此条降级为"接线未完成"而非"声明不实"；
3. **H6 的实际后果严重度**——多主机/多用户场景是读码推演（未做双机实验）；存在性与机制我有把握，"自旋 30 秒"的具体表现请以实验复核。

## 八、总结

| 维度 | 评价 |
|---|---|
| 架构方向 | 成立；机制/数据/索引三层与设计一致，无需推翻 |
| 核心正确性（锁/回填/CAS/字节保持） | 高；验收清单大部分真实通过 |
| 阻断项 | **3 项**（B1 事件日志毒化、B2 fix 丢正文、B3 快路径未接线+状态虚报） |
| 高优先级 | 6 项（fix 无 CAS、rename 提交缺旧路径、deep 审计自比自、apply 无独占门闩、find 截断无标注、跨主机锁判定） |
| 测试 | 约 121 项全绿，质量高；但三个阻断项各落在测试盲区 |
| 建议 | B1/B2 修复 + 补测试后方可继续积累数据；B3 需用户裁决（补接线 or 如实降级声明）；H1–H6 建议随 B 项同批处理（均为小改） |

---

### 待批准清单

**本轮批准范围**：上述发现清单及其定级（B1–B3、H1–H6、M1–M11、L1–L6）与测试缺口认定。
**本轮明确不批**：各条目的具体修复方案（正文中的"修复建议"仅为方向，实施需另行提交并按 §8 验收口径补测试）；B3 的二选一路线（补接线 vs 如实降级）需用户裁决后更新设计文档状态行。
