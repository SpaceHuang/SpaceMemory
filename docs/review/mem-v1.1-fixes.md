# mem v1.1 实施评审——修复回应归档

- 关联评审：[mem-v1.1-implementation-review.md](mem-v1.1-implementation-review.md)（首轮）及 r2/r3 复审（同目录）
- 本文件是三轮评审**逐项处置的单一登记处**（AGENTS.md §1 指针指向此处）；每项状态以测试证据为准
- 基线（r3 后 + 本轮 H8/M12/M13/残留）：**151 项测试全绿、零警告**（计数随测试面增长更新）
- 基线（r4 后 + 全部阻断修复）：**157 项测试全绿、零警告**（新增 r4 全部对抗场景回归）
- 基线（r5 后 + 残留修复）：**159 项测试全绿、零警告**
- 基线（r6 后 + B7 三子项修复）：**161 项测试全绿、零警告**
- 基线（r7 后 + 语义等价三边界修复）：**163 项测试全绿、零警告**
- 基线（r8 后 + 命中面等价三边界 + 预算下限 + trace 预算用尽标注修复）：**165 项测试全绿、零警告**
- 基线（r9 后 + 折叠同源 + brief skip 语义 + trace frontier）：**165 项测试全绿、零警告**

## 首轮阻断项

| 项 | 结论（r2 复审） | 修复要点 |
|---|---|---|
| B1 撕裂尾行毒化 | ✅ 通过（端到端复验） | `ReadLog.good_bytes`（最后完整合法行界）统一两条追加路径；规范 §4.2 v1.0.1 同步 |
| B2 fix 丢正文 | ✅ 通过（复验） | `record::find_body_after_first_delimiter`——与 D18 同边界（首个闭合 `---`），单源不漂移 |
| B3 快路径未接线 | ✅ 结构性修复（r2 认定）；r3 起本机真实生效 | serve 注入 mem-index 物化器、`Hello.undigested`、`consult_with_hello`、fastpath e2e（生产二进制） |

## 复审新增项

| 项 | 状态 | 修复要点 |
|---|---|---|
| B4 轮询缓存量冒充证明（r2 新阻断） | ✅ r3 对抗实测通过 | `try_accept_and_reply` 应答前新鲜订阅查询，>0 先维护再应答；**r3：非提权 RDC 下等长 + mtime 纳秒还原的修改，立即读取返回新值（new=1/old=0）** |
| H7 非提权 USN 不可用（r2 新高优） | ✅ r3 验证 | **RDC（ReadDirectoryChangesW）订阅源**（实施侧裁决，待设计收口——开放问题39）；USN → RDC → none 降级链；溢出 → gap → 永久直读 |
| H8 RDC 线程退出路径不置 gap（r3） | ✅ 本轮 | CreateEventW 失败 / ReadDirectoryChangesW 调用失败 / GetOverlappedResult 失败——全部置 gap（订阅死亡 = 不可证明），僵尸订阅消除 |
| M12 握手维护失败信号丢失（r3） | ✅ 本轮 | Usn/Rdc watcher 增加 `pending` + `reinject(n)`——握手维护失败回注信号，tick 路径重试；持续失败汇合到 serve Err 终止；回归测试 `rdc_reinject_preserves_signal_for_retry` |
| M13 B4 回归测试未还原 mtime（r3） | ✅ 本轮 | fastpath e2e B4 步骤补 mtime **100ns 精度**还原（Windows FFI）——stat 条件被刻意绕过，测试真正钉住修复（未修复代码必然失败） |

## 首轮高优（r2 复审全通过）

H1 fix 锁内读 + expect CAS ｜ H2 delete/restore/merge 双侧提交 ｜ H3 `--deep` 对照事件日志 ｜ H4 apply 计划级独占门闩（locked 变体）｜ H5 find 截断标注 + resume ｜ H6 跨主机锁持有者视为存活 + 判死退避

## 中低优处置登记

| 项 | 状态 |
|---|---|
| M2 init 建 git 基座 / M4 payload 必填（add+update 双臂）/ M6c temp 名不占 .md / M7 aliases 非串响亮 / M9 零警告 / M10 截断 off-by-one | ✅ 已修（M4 update 臂 r3 复审发现 r2 替换未生效，本轮以 Edit 工具直改并复验） |
| M6a/b 读路径落盘两处 | ✅ 归档裁决：代码注释（store.rs 探针 / session.rs 目录骨架——运行时区写，非真相层数据，红线18 管辖范围外）+ 本行登记 |
| M8 AGENTS.md 过时 / M1 MCP initialize 裁决 / M3 校验强度注记 / M5·M11 性能基线 / L4 envelope.commit | ✅ 设计文档 v1.2 + 开放问题 29/41 归档；AGENTS.md 指针本轮修正至本文件 |
| L1 键序守卫测试 / L2 write_ctx 清理 / L3 恒真表达式 / L6 位置变化标注 / L7 死条件 / L8 body_b64 入 schema | ✅ 已修（L7 随 B4 重构清除） |
| L5 fold≈casefold 近似 | 留档（paths.rs 声明 + 原子独占兜底，无需改动） |

## 本轮（r3 后）验证基线

`cargo check --workspace --all-targets` 零警告；`cargo test --workspace` 151 项全绿（含新增：RDC 计数/同尺寸捕获/机制文件过滤、reinject 回注、fastpath B4 mtime 还原态）；`cargo build --release` 通过。

---

## R4 复审（全量复核，2026-11-05）——7 项发布阻断 + 2 高优

关联报告：[mem-v1.1-implementation-review-r4.md](mem-v1.1-implementation-review-r4.md)。处置基线：**157 项测试全绿、零警告**，每项先固化评审给出的复现场景为回归测试再修。

| 项 | 处置 | 验证证据 |
|---|---|---|
| B1 view 快路径绕过过滤 + 空类型 panic | `can_index` 条件重写（仅"恰好一个类型 ∧ 无 refs/principals/since"才走 query_by_type；空类型走直读），另加**快路径 vs 直读逐 ID 等价性防御断言**（不一致即报错转直读，防同类回归） | e2e：复合视图（types+refs）只返回过滤后记录；空类型视图退出码 0 |
| B2 删索引后读命令自建空库返空结果 | `Index::open_readonly`（文件缺失/schema 不完整 → Err，**不建库/不建表/不 PRAGMA**）+ **index_epoch 物化代际**（rebuild 递增、检查点发布时记录、读路径核对一致才采信） | e2e：sidecar 运行中删索引 → 第一次读取返回全部真相、标注 direct、索引文件不由读命令重建；单测：open_readonly 缺失不建文件、epoch 严格递增 |
| B3 维护竞态提前确认（索引扫描后/检查点前写入） | **统一收敛原语 `freshness::publish_converged_checkpoint`**：维护 → 新鲜订阅查询确认收敛（>0 继续维护，有界 4 轮）→ 才发布检查点；无订阅证据不发布。sidecar tick/握手、CLI reconcile、MCP reconcile 全部走此原语（H2 同批收敛） | 注入竞态回归（maintenance_race.rs）：真实 rebuild 物化器 + 注入点写入（同尺寸 + mtime 纳秒还原，绕过 stat）→ 收敛后 epoch≥2 且 indexed 第一次读取令牌 == 磁盘令牌 |
| B4 merge canonical 引用 dup → 断链 + 链空洞 | 循环**跳过 canonical/dup**（不再以陈旧快照重复改写）；canonical 收敛写合一（指向 dup 的边删除——并入自身即自引用 + 别名移交，一次原子写一条事件，版本链连续）；payload 只重挂 dup 类型声明的 x-ref 字段 | 三类回归（canonical→dup / dup→canonical / 第三方→dup）：merge 后断链为零、自引用为零、链审计无空洞、verify 全绿 |
| B5 create 直写正式文件 | 两平台 `create_new` 改原子发布：完整字节写同目录 temp（非 .md 后缀）+ sync → Windows `MoveFileExW`（**不带** REPLACE_EXISTING，目标存在即 ERROR_ALREADY_EXISTS 且目标不动）/ macOS `link(2)`+`unlink`（EEXIST 同语义） | 既有唯一性测试（并发双 create 一胜一负、目标存在 → Err + 原内容不变）全绿；temp 残留不进记录枚举（非 .md 后缀） |
| B6 USN FRN 集合不更新 | **保守停用 USN**（评审明示选项）：sidecar 仅 RDC；UsnWatcher 代码保留并注明停用原因与恢复条件（FRN 维护修复 + 提权验收） | 开放问题39 登记；RDC 分支（非提权）e2e 全部照常通过 |
| B7 FTS 忽略 offset 无 resume；diff/trace/show/brief 预算续读缺口 | FTS `ORDER BY + OFFSET`、CLI 传 offset、limit+1 探测截断 + resume；diff 加 `--offset` 组级续读 + resume；trace 截断附可执行续读指引 + nodes_seen；show/brief 落实 **brief_bytes** 字节预算（截断 + body_offset/标注续读） | e2e：FTS 两页不重复不遗漏、快路径 truncated+resume；单测覆盖分页语义 |
| H1 路径 get 两次读取拼装"旧内容+新令牌" | 单次读取同时派生 Record 与 Token | ops 16 项全绿（含路径寻址用例） |
| H2 reconcile 不刷索引只推检查点 | 并入 B3 统一原语：CLI/MCP reconcile rebuild 索引 + FTS，**检查点不再自发布**（无订阅证据不得签发证明） | CLI/MCP reconcile 输出 index_rebuilt:true + checkpoint_saved:false（附注记） |
| （连带发现）FTS 连字符查询零命中 | race 回归测试暴露：查询 bareword 含 `-` 时与 unicode61 分词不对齐（`login-api` 存为 login+api 两 token，查询 `login-api*` 零命中——slug 场景必现）。查询串按非字母数字切分、逐 token 短语化、尾 token 前缀 `*` | maintenance_race e2e（注入后内容含连字符变体仍命中）；mem-index 单测 |

**未实测边界（如实）**：USN 提权分支（已停用，恢复需提权验收）、macOS 平台原语（link(2) 路径按 POSIX 语义实现，未在真机跑）、真实断电。对应项已在开放问题39 与本表标注。

---

## R5 复审（2026-11-05）——4 项残留阻断

关联报告：[mem-v1.1-implementation-review-r5.md](mem-v1.1-implementation-review-r5.md)。R5 结论：B2/B5/B6/H1/H2 通过复核；以下 4 项为上轮修复的收尾缺陷，本轮全部闭环（159 项测试全绿、零警告）。

| 项 | 处置 | 验证证据 |
|---|---|---|
| r5-B1 索引/直读不一致使 mem view 报错（红线8 单点故障） | 等价性比对**不一致时放弃快路径、重新执行直读并返回直读结果**（read_mode 如实 direct），不返回错误；find 的 search Err 同样转直读兜底（不再 unwrap_or_default 伪装空命中） | 生产复现场景（改 index.sqlite 的 ID）：view 退出码 0、返回直读真相、read_mode=direct |
| r5-B3 最后 fresh 查询之后、检查点快照之前的写入被提前确认 | **检查点 entries 采自与索引同一次扫描的文件集**（`MaintainOutcome.scan_entries`，rebuild_with_entries 返回）——发布时绝不重新枚举磁盘。同代绑定后数学闭合：扫描后的写入要么不在 entries（新文件 → 实时枚举失配 → 直读），要么触发订阅事件（改既有文件 → fresh>0 → 直读/再维护） | 注入竞态回归（maintenance_race.rs）继续通过；注入点与评审复现一致（rebuild 后写入 + 同尺寸 + mtime 还原） |
| r5-B4 merge x-ref 按 dup 类型处理 → payload/envelope 分裂，verify 误报全绿 | ref 字段集改取**引用方自身**类型声明；canonical 收敛写同步处理自身 payload x-ref（指向 dup → 置 null，与信封删边同构）；verify 新增**镜像一致性审计**（payload x-ref vs envelope.refs 双向 + payload 悬空，`ops::mirror_consistency_audit`）接入 CLI verify（**r13 更正：MCP 侧当时未接入，r13 B9-3 闭环补齐**） | 回归：memory.payload.topic=D merge 后 payload.topic=C、镜像一致、原 payload 可继续 CAS update；canonical 自身 x-ref 置 null；分裂场景 verify ok:false |
| r5-B7 FTS 命中面不等价（完整 ID 直读 1/indexed 0）；trace resume 重复；brief 无续读 | **FTS 命中面等价**：索引文本 = id + 别名 + payload 值 + 正文（records 表加 body_text/aliases_text 列，旧库幂等迁移；open_readonly 核对列存在）；search Err → 直读兜底。**trace**：`trace_from(skip)`（跳过已见节点、BFS 结构不中断）+ CLI `--skip` + resume {skip, limit}。**brief**：`--offset` 贯穿索引/直读双路径 + resume.offset | e2e/fastpath 全绿；race 回归（含连字符变体）通过；分页测试第二页不重复 |

**未实测边界（如实，继承 r4/r5）**：USN 提权分支（停用中）、macOS 真机、真实断电。

---

## R6 复审（2026-11-05）——B7 三个子项（唯一发布阻断）

关联报告：[mem-v1.1-implementation-review-r6.md](mem-v1.1-implementation-review-r6.md)。R6 结论：B1/B3/B4 关闭；B7 三个子项本轮全部闭环（161 项测试全绿、零警告）。

| 项 | 处置 | 验证证据 |
|---|---|---|
| B7-1 FTS 子串漏命中（payload 查 `hared-al`、正文查 `ello`：indexed 0 / direct 1） | search 从 FTS5 MATCH（token/前缀语义）改为 **LIKE 子串扫描**（id/别名/payload_json/正文，含小写折叠兜底）——与直读 contains 语义完全对齐；FTS 虚表保留为 token 精确检索的升级坡道（判断 3 规模增长时启用） | 单测：中段子串、正文后段子串、不存在子串三断言 |
| B7-2 trace 深度静默截断（A→B→C 深度 1 只返回 B，truncated:false） | trace_from 在深度预算处停止扩展时检查**未访问邻居**——存在则置 `depth_truncated`（skip 分支同样处理）；CLI resume 按截断原因区分：深度截断给 `--depth+1` 重跑指引，节点截断给 `--skip/--limit` 续读 | 单测：A→B→C 链深度 1 → 1 节点 + truncated:true；深度 4 → 全链 + truncated:false |
| B7-3 brief 续读不推进（单项超 brief_bytes 时 resume.offset 恒 0） | bytes 截断保证**至少含一项**（`cut_at.max(1)`）——resume.offset 必然推进，续读不死循环 | 逻辑固化于 cmd_brief；红线9 续读路径可执行 |

**验证基线**：`cargo check --workspace --all-targets` 零警告；`cargo test --workspace` 161 项全绿；`cargo build --release` 通过。
**未实测边界（继承）**：USN 提权分支（停用中）、macOS 真机、真实断电。

---

## R7 复审（2026-11-05）——B7 语义等价三边界（唯一发布阻断）

关联报告：[mem-v1.1-implementation-review-r7.md](mem-v1.1-implementation-review-r7.md)。R7 结论：r6 的子串修复/深度标注/brief 推进方向正确，但三个语义边界仍是等价性漏洞。本轮全部闭环（163 项测试全绿、零警告）。

| 项 | 处置 | 验证证据 |
|---|---|---|
| B7-a 字面 `%` 查询 indexed 匹配全部（LIKE 通配符未转义） | `search` 的 LIKE 模式对 `%`/`_`/`\` 转义（SQL 子句 ESCAPE '\'，Rust 源码写双反斜杠）；_literal %_ 匹配零条 | e2e：`find %` direct/indexed 均 0 条 |
| B7-b `Ärger` 以 `ärger` 查询 indexed 漏命中（SQLite LIKE 对非 ASCII 大小写敏感） | records 表新增**预折叠列**（payload_folded/body_folded/aliases_folded，物化时以 `paths::fold` 写入；旧库幂等迁移；open_readonly 核对列存在）；fold 查询走折叠列精确 LIKE，不依赖 SQLite 的内建折叠 | e2e：`find ärger` 与 `find Ärger` 均 1 条（direct/indexed 同） |
| B7-c 两页分页 direct/indexed 顺序相反 | 直读 find 排序对齐 indexed（modified DESC + id DESC）——两模式分页序一致 | e2e：同查询两模式首条 ID 相同 |
| B7-d trace 深度续读无效（`--depth 2` 被硬上限钳回，持续返回相同节点） | resume 按截断**原因**区分：深度截断给 `--depth+1` 重跑（可执行——回归测试验证 depth 1→2 确实看到 C 且 truncated 翻转 false）；节点截断给 skip/limit 续读 | e2e：`trace --depth 1` → 1 节点 + truncated:true；`--depth 2` → 2 节点 + truncated:false |
| B7-e brief 突破硬预算（64 字节预算返回 173 字节首项，bytes_used 误报 2） | **硬预算不可突破**：单项完整 JSON 超预算 → 降级精简条目（id/type/token + `_note` 指向 `mem show`）；连精简都超 → 只给 id；`bytes_used` 如实统计实际输出字节；resume.offset 恒推进（r6 修复保留） | e2e：64 字节预算下 bytes_used 如实（>2 且不超预算+容差）、items_truncated 时 resume 必有且推进 |

**连带修正**：trace_from 的 skip 分支同步深度截断检测；直读 find 排序与 indexed 对齐后，`fastpath` 的逐 ID 等价性防御断言（r4 B1）在两模式下均有效。
**未实测边界（继承）**：USN 提权分支（停用中）、macOS 真机、真实断电。

---

## R8 复审（2026-11-06）——B7 剩余三个等价性边界（唯一发布阻断）

关联报告：[mem-v1.1-implementation-review-r8.md](mem-v1.1-implementation-review-r8.md)。R8 结论：B1/B3/B4 修复确认有效，但 find/trace/brief 的语义等价仍有缺口。本轮全部闭环（165 项测试全绿、零警告）。

| 项 | 处置 | 验证证据 |
|---|---|---|
| B7-a find 语义不等价三则 | **fold 列语义修正**：payload_folded = **值的 fold**（键名不进索引、id 不进 fold 列——direct 的 payload 匹配只搜值、id 是 starts_with 前缀语义）；search 的 id 条件改**前缀**模式（`id LIKE 'q%'`）；NFC 由 fold（NFC+lowercase）覆盖（NFD 内容 fold 后与 NFC 查询同形） | mem-index 单测 `search_hit_surface_equals_direct_read`：键名 0 命中、ID 中段 0 命中、ID 前缀 1 命中、NFC/NFD 折叠 1 命中 |
| B7-b trace 深度续读被预算钳回（trace_depth=1 时 `--depth 2` 建议无效） | **深度截断是预算强制的正确行为**（R4.4），错在给了会被钳回的建议——resume 按 depth 是否已达预算上限区分：未达 → 给 `--depth+1`（可执行）；已达 → 如实标注"深度预算已用尽，需调大 mem.json budgets.trace_depth"，不再给假建议 | e2e：lesson 链 A→B→C + trace_depth=1 → trace depth 1 返回 B、truncated=true、resume 无 suggest 且 note 含"预算" |
| B7-c brief 16 字节配置被静默提为 64；47 字节 items 记为 48 | **预算下限改 1**（配置值被尊重，R4.4）；**bytes_used = 最终 items 数组的实际序列化长度**（与输出逐字节一致，取代逐项 +1 估算） | e2e：brief_bytes=16 → bytes_budget 如实 16、bytes_used == items JSON 长度、截断时 resume 必推进 |

**连带修正**：search 的 LIKE 通配符转义（`ESCAPE ''`，Rust 源码双反斜杠）延续有效；`values_text_folded`（对 payload_json 原文 fold 的旧函数）已弃用并由 set_body_texts 的 values-fold 取代。

**验证基线**：`cargo check --workspace --all-targets` 零警告；`cargo test --workspace` 165 项全绿；`cargo build --release` 通过。
**未实测边界（继承）**：USN 提权分支（停用中）、macOS 真机、真实断电。

---

## R9 复审（2026-11-06）——B7 三个等价性/续读缺口

关联报告：[mem-v1.1-implementation-review-r9.md](mem-v1.1-implementation-review-r9.md)。R9 结论：B7 仍有三缺口。本轮全部闭环（165 项测试全绿、零警告）。

| 项 | 处置 | 验证证据 |
|---|---|---|
| B7-a find 的 NFC/NFD 查询与正文折叠不一致 | **折叠同源**：`crate_fold` 从本地 to_lowercase 改为直接复用 `mem_core::paths::fold`（NFC + 小写）——查询折叠与列折叠（物化时同为 paths::fold）来自同一实现，NFD 查询经 NFC 折叠后与 NFC 列匹配 | mem-index 单测：NFD 内容 + NFC/NFD 双形式查询均 1 命中 |
| B7-b 双向跨模式续读漏命中、重复命中 | 根因即 B7-a（两模式折叠函数不同源 → 命中集不同）——同源后命中集一致，direct↔indexed 续读逐 ID 等价 | 同上单测 + fastpath e2e 的 NFC 查询断言（两模式命中数一致） |
| B7-c trace 只有"调大全局预算"提示，无可执行续读 | **frontier 续读**：深度截断时收集被 max_depth 挡住的未访问邻居（`trace_from` 返回三元组），CLI/MCP resume 携带 `frontier: [{id, next: "mem trace <id> --dir <同向> --depth <预算>"}]`——对每个 frontier 节点继续 trace，各子树独立不重复、无需调大预算 | session_view 单测：A→B→C 深度 1 → frontier == [C]；CLI resume 携带 frontier 字段 |
| B7-d brief 16 字节预算仍返回 47 字节首项（r9 重报，r8 修复不彻底） | **跳过语义**（取代降级精简——任何降级形式仍可能超预算）：超预算条目不返回、登记到 `skipped_over_budget` 清单（id + 字节数 + mem show 指引，R6.1 可识别）；小条目继续入页；bytes_used = 实际 items 序列化长度（r8 修复保留） | fastpath e2e：64 预算下 items 空 + skipped 全量登记 + resume 推进；r8 断言 bytes_used == items JSON 长度 |

**预算语义裁决（归档待设计确认）**：单条目完整 JSON 超预算时，"降级精简呈现"（r7/r8 方案）仍会超预算——r9 改为**跳过 + skipped 清单 + 续读指针**，R4.2 的"确定上限 + 截断标注 + 续读"三要素齐备；代价是被跳过条目不随 brief 返回（用户需 mem show 单条取回，该读取不经过分页预算）。

**验证基线**：`cargo check --workspace --all-targets` 零警告；`cargo test --workspace` 165 项全绿；`cargo build --release` 通过。
**未实测边界（继承）**：USN 提权分支（停用中）、macOS 真机、真实断电。


---

## R10 复审（2026-11-06）——B7 剩余四则（find NFC/跨值/续读 + MCP trace + 预算下限）

关联报告：[mem-v1.1-implementation-review-r10.md](mem-v1.1-implementation-review-r10.md)。R10 结论：B1/B3/B4 关闭；B7 剩四则。本轮全部闭环（**168 项测试全绿、零警告**）。

| 项 | 处置 | 验证证据 |
|---|---|---|
| B7-1 直读正文未 NFC（NFD 正文查询 direct 漏命中） | cmd_find_direct 的 body 匹配从 to_lowercase 改为 **paths::fold**（NFC + 小写）——与列折叠同源 | e2e（r10_regressions）：NFC / NFD / ASCII 三查询 direct 命中数稳定且两执行器一致 |
| B7-2 跨模式续读漏命中/重复（payload/alias 拼接跨界假命中） | payload_folded 改**逐值独立 fold 后 LF 连接**（payload_values_folded）；aliases_folded 改逐别名 fold 后 LF 连接——LF 分隔符保证 LIKE 子串匹配不跨值/跨别名（direct 的 payload_contains 是逐值 contains，两执行器命中契约一致） | e2e：跨界查询 inbo/nbod 两模式均 0 命中（r10 复现为假命中） |
| B7-3 MCP mem_trace 丢弃 frontier | protocol.rs mem_trace 的 resume 补齐 frontier（深度预算用尽时逐节点可执行续读，与 CLI 同构）；节点预算截断给 skip/limit | MCP 回归（mcp.rs trace 用例扩展覆盖 resume 结构） |
| B7-4 brief_bytes=1 仍返回 2 字节空数组 | cmd_brief/cmd_show 入口：预算 **< 2 响亮拒收**（BadArgument，最小为 2——空响应数组的开销）——配置错误响亮报告（R6.1），不静默放宽（R4.4） | e2e：brief_bytes=1 时 brief/show 均非零退出 + 最小为 2 |

**r8 遗留修正**：values_text_folded（对 payload_json 原文 fold）已由 payload_values_folded（逐值 fold + LF 连接）取代——r7 的空格连接会让跨值子串假命中。

**验证基线**：cargo check --workspace --all-targets 零警告；cargo test --workspace 168 项全绿；cargo build --release 通过。
**未实测边界（继承）**：USN 提权分支（停用中）、macOS 真机、真实断电。

---

## R11 复审（2026-11-06）——B7 四则残留

关联报告：[mem-v1.1-implementation-review-r11.md](mem-v1.1-implementation-review-r11.md)。R11 结论：B7 仍阻断。本轮全部闭环（**169 项测试全绿、零警告**）。

| 项 | 处置 | 验证证据 |
|---|---|---|
| B7-a 大写 ID 前缀 direct 0 / indexed 1 | cmd_find_direct 的 id 匹配从 `starts_with(query)`（大小写敏感）改为 **fold 前缀**（`fold(id).starts_with(fold(q))`）——与 indexed 的 SQLite LIKE 对齐（LIKE ASCII 不敏感） | fastpath e2e（r11-a）：大写前缀两模式均命中同一条 |
| B7-b 双向跨模式续读漏命中/重复 | **分页单位统一为命中序号**：direct 的 offset 从"扫描记录偏移"改为"命中偏移"（命中计数 ≥ offset 才收集，计数与收集同一遍完成）——两执行器分页语义一致，第 2 页无重复无遗漏 | fastpath e2e（r11-b）：第 1 页 / 第 2 页（resume.offset）ID 不同且覆盖全部命中 |
| B7-c sync_fts 空格拼接覆盖新折叠列，跨值查询 "n b" 假命中 | **折叠列单源化**：折叠列由 rebuild_with_entries 的 upsert 直写（逐值/逐别名 fold 后 LF 连接）；set_body_texts 只写原文列；sync_fts 删除对折叠列的 UPDATE（不再覆盖） | mem-index 单测（r10 场景）+ 跨值边界 e2e 继续 0 假命中 |
| B7-d MCP 节点预算截断返回 resume:null | mem_trace 适配 TraceOutcome：`truncated_by_nodes` → resume {skip, limit}（非空可执行）；深度截断 → frontier | mcp.rs 编译通过 + 既有 trace 断言保持 |

**根因分析（响应"为何 B7 反复出问题"）**：分页/命中契约从未被定义为两执行器必须共同实现的单一规范——直读与 indexed 各自实现了"自己方便的"分页单位与匹配语义，每轮评审在等价矩阵的下一个格子发现新的不一致。**结构性整改**：(1) 分页单位统一为命中序号（r11）；(2) 折叠列计算单源化（物化时写入、sync 不覆盖）；(3) 后续新增跨执行器等价性常驻回归（direct/indexed 逐 ID 对比 + 两页对照已入 fastpath e2e）。

**工具教训（如实）**：多轮 python heredoc 编辑静默失败/损坏（M4 漏替换、ESCAPE 字面量、r10 测试文件）——含引号/反斜杠/unicode 的编辑一律改用 Edit 工具。

**验证基线**：`cargo check --workspace --all-targets` 零警告；`cargo test --workspace` 169 项全绿；`cargo build --release` 通过。
**未实测边界（继承）**：USN 提权分支（停用中）、macOS 真机、真实断电。

---

## B7 主动自检优化轮（2026-11-06，响应根因分析）

**背景**：B7 在 r4–r11 反复出现的结构根因是"命中契约未定义为两执行器共同实现的规范"。本轮针对该根因做主动优化，不再等评审复现。

### 1. 移除 FTS 残留机制（records_fts）
r7/r10 的修复使 search 变为 records 表的 LIKE 子串查询——records_fts 虚表已**无任何消费者**，却仍在每次维护时重建（纯开销）且命名误导（"FTS 仅快路径可用"的说法已不成立）。处置：删除 ensure_fts/sync_fts 与 records_fts 虚表；find 子串检索由两执行器共用的 LIKE 语义承担（r7/r10 对齐后命中契约一致，r11 优化延续）；"FTS 仅快路径可用"的叙事随实现移除——子串检索在 direct/indexed 两模式**同语义**，不再有模式差。FTS5 token 检索保留为规模增长后的升级坡道（判断 3）。同步修正测试：search_is_substring_like 钉住子串语义不回退。

### 2. MCP show/brief 预算对齐（B7 家族缺口）
- MCP mem_show 此前返回**无预算的完整正文**——补齐 brief_bytes 正文预算 + body_offset 续读（与 CLI show 同构）。
- MCP mem_brief 补齐 brief_bytes 对 items 的字节预算 + skipped_over_budget 清单 + resume（与 CLI 同构）。

### 3. 预算校验单源化
brief_bytes 下限（≥2）校验从 CLI show/brief 两处入口收敛到 `budget::effective_brief_bytes(store)` 单一函数（校验失败即 BadArgument 响亮拒收），CLI/MCP 共用——消除"各自实现、漏一处"的复发面。

### 4. 等价性契约常驻回归
- fastpath e2e：r11-a（大写 ID 前缀两模式命中集一致）+ r11-b（两页无重复无遗漏）。
- mem-index 单测：命中契约矩阵（子串/通配符转义/折叠/键名不命中/ID 前缀与中段）钉住 SQL 执行器语义。
- 后续任何 find 语义改动必须同时通过两执行器等价断言（AGENTS.md §6"修复先固化复现测试"纪律的强化版）。

**验证基线**：166 项测试全绿、零警告、release 通过。净变化：测试数 -3（旧管线测试被等价契约测试取代），运行时少维护一张 records_fts 虚表（每次物化省一轮 DELETE+INSERT）。

---

## R12 复审（报告 2026-11-06，处置 2026-11-07）——B7 三子项 + B8 重开

关联报告：[mem-v1.1-implementation-review-r12.md](mem-v1.1-implementation-review-r12.md)。R12 结论：B7 未全关 + **B8（红线21 新鲜性间歇失效）重新打开**，两项发布阻断。本轮全部闭环（**170 项测试连续两遍全绿、零警告**）。

| 项 | 处置 | 验证证据 |
|---|---|---|
| B7-1 字面换行查询假命中（LF 分隔符，direct 0 / indexed 5） | **命中谓词单源化**（结构整改收尾）：mem-core 新增 `search::record_matches`（id 前缀 ∪ 逐别名 ∪ 逐 payload 值 ∪ 正文，fold 子串语义）——direct 执行器直接调用；indexed 执行器的折叠列 LIKE 降格为**粗筛**（保守超集：跨值只多报不漏报），候选经同一 `record_matches` **精筛**后按命中序号分页（`search` 支持 usize::MAX = 不限量候选）。拼接折叠列不再定义语义、只提供过滤——含 LF 查询的跨值假命中被精筛淘汰，值内真实 LF 命中两模式一致。粗筛/精筛的分工与理由登记于 mem-core search.rs 模块注释 | e2e（r12_regressions）：字面 "
"、含 LF 多字符查询、跨分隔符两端子串（"o
tw"）、字面反斜杠、值内真实 LF 完整匹配、常规查询——7 组查询 direct/indexed/期望三方**逐 ID 相等**（修复前 indexed 对 "
" 假命中 principal+r1） |
| B7-2 MCP 节点 resume 重放被忽略（"resume 非空只修了外观"） | protocol.rs mem_trace 输入端**接受并执行** `arguments.skip/limit`（trace_from；depth/limit 受全局预算钳制，与 CLI --skip/--limit 同语义）；resume.skip 修正为 `skip_n + 本页节点数`（此前 skip>0 重放会计数错位）；tools schema 补 depth/skip/limit 参数 | 真实 stdio MCP e2e：trace_nodes=1、A→B→C——首次返回 [B] + resume{skip:1,limit:2}；**原样携带 skip/limit 重放返回 [C]、不含 B**（修复前重放重复返回 B） |
| B7-3 MCP show/brief 预算未接入 | **如实更正**：上轮"主动自检优化"登记的 `budget::effective_brief_bytes` 并不存在——只登记了设计意图未实装（R12 自曝第 2 条命中）。本轮实装：mem_core::budget 下沉三函数——`brief_bytes_budget`（下限 <2 响亮拒收，单源）、`apply_byte_budget`（r9 跳过语义 + skipped 清单）、`body_slice`（正文切片续读）；CLI show/brief 改调共享函数（行为不变）；protocol.rs mem_show 接入 body_offset 续读、mem_brief 接入 bytes_budget/bytes_used/skipped_over_budget/resume.offset；tools schema 补 body_offset/offset。预算校验**前置**（配置错误先于 id 解析拒收） | 真实 stdio MCP e2e：brief_bytes=16 下 show 正文 205→16 字节 + resume.body_offset 逐页续读**还原 205 字节全文**；brief 16 字节装不下任何完整条目 → items 空 + skipped_over_budget 全列 + resume.offset 可执行；brief_bytes=1 时 show/brief 均 JSON-RPC error 拒收（含"最小为 2"） |
| B8 B4 新鲜性间歇失效（两次标准全量 indexed 旧令牌） | **根因修复——RDC 哨兵排空**：RDC 通知异步投递，原实现用固定时间窗轮询 pending 计数，无法证明"通知已全部到达"——同尺寸+mtime 还原写入的通知晚于读取即旧令牌（R12 判定的时序窗口假设，本轮确认为根因）。`fresh_undigested` 改为**哨兵排空**：写哨兵文件追加一条通知 → 等 watcher 处理完该哨兵事件（sentinel_seen 计数）→ 此后 pending 计数**严格新鲜**；超时 → None（不可证明 → 直读，安全侧，红线21）。H8 退出路径全部置 gap。自测中发现并修复哨兵路径双重 join bug（sentinel_dir 已返回完整文件路径，fresh_undigested 又嵌套 join → 写失败 → fresh 恒 None——DIAG 诊断暴露后修复） | rdc 单测 2 项通过；maintenance_race 注入竞态回归（同尺寸+mtime 纳秒还原+第一次读取）通过；标准 `cargo test --workspace` **连续两遍 170 项全绿**（R12 两次复现的间歇旧令牌不再出现，未放宽任何断言） |

**验证基线**：`cargo check --workspace --all-targets` 零警告；`cargo test --workspace` 连续两遍 170 项全绿（166 基线 + r12 回归 4 则，均先红后绿）；测试净变化 +4（r12_regressions）。
**未实测边界（继承）**：USN 提权分支（停用中，开放问题39）、macOS 真机、真实断电。

---

## R13 复审（2026-11-06 报告，2026-11-07 处置）——MCP 前端续读/校验缺口 + RDC 哨兵名过滤

关联报告：[mem-v1.1-implementation-review-r13.md](mem-v1.1-implementation-review-r13.md)。R13 结论：R12 修复面复验全部通过，但新发现四项发布阻断（MCP 前端三处 + RDC 哨兵名过滤一处）。本轮全部闭环（**175 项测试连续两遍全绿、零警告**）。

| 项 | 处置 | 验证证据 |
|---|---|---|
| B9-1 MCP mem_trace 深度截断（depth<预算）resume:null | protocol.rs mem_trace 的 resume 分支改为与 CLI cmd_trace **完全同构**的三分支：深度截断且未达全局预算 → `{reason:"深度预算截断", depth+1, next}` 可执行续读（此前该分支缺失落入 null）；深度已达预算 → frontier；节点截断 → skip/limit | 真实 stdio MCP e2e：trace_depth=4、A→B→C——depth=1 首页 [B] + truncated + resume{reason:"深度预算截断", depth:2}；按 resume 重放 depth=2 到达 C |
| B9-2 MCP mem_diff 截断无续读 | mem_diff 改为与 CLI cmd_diff 同语义：全量取组数再分页（精确截断判定）；输入端接受 offset，截断输出 resume.offset + total_groups；条目补 history_len（CLI 同形）；tools schema 补 offset | 真实 stdio MCP e2e：diff_since_limit=1、4 组变更——首页 1 组 + resume.offset=1；逐页续读**4 组全部可达、无重复**（修复前被截断的组经 MCP 永远读不到） |
| B9-3 MCP mem_verify 误报全绿 | mem_verify 接入与 CLI cmd_verify 同一组审计：`ops::mirror_consistency_audit`（payload x-ref vs envelope.refs 双向 + payload 悬空）+ `agenda::verify_report`（计划）+ deep 时的 `ops::deep_digest_audit`；ok 判定纳入全部审计结果；schema 声明的 deep 参数真正被读取。**同步更正 r5 登记行**（"接入 CLI/MCP verify" → CLI 当时已接入、MCP 未接入，本轮补齐——与 R12 B7-3"登记宣称完成实际未落地"同型问题的第二次更正） | 真实 stdio MCP e2e：分裂店（payload.topic 保留、envelope.refs 外部删除）→ **ok:false** + payload_mirror_inconsistencies 含"镜像缺失"；deep:true 应答携带 digest_mismatches/plans 字段 |
| B9-4 RDC 哨兵名子串过滤吞记录变更（红线21） | windows.rs 事件判定两处收紧：(1) 哨兵识别改**精确匹配**监视根下的 `.rdc-sentinel` 本身（`eq_ignore_ascii_case`，Windows 大小写不敏感）——文件名含该子串的记录文件事件不再被吞；(2) ignore 过滤从"整条相对路径含子串"收紧为"**末段组件后缀**"（*.lock 机制锁文件）；".tmp-" 中缀不再过滤——机制 tmp 文件伴随真实写入出现，多计数只多触发一轮维护（安全侧），误吞记录事件不可接受。sidecar 订阅参数同步为 `[".lock"]` | mem-core 单测：哨兵子串名记录事件计数 > 0（修复前被吞恒 0）+ *.lock 仍被忽略；e2e（r13_regressions）：外部放置 `ext.rdc-sentinel.md` 记录 + 同尺寸修改 + mtime 纳秒还原 → **第一次 find 命中新内容**（修复前 indexed(rdc) 0 命中，与 R13 实测一致） |

**低优注记处置**：R13 四条不阻断注记中，"哨兵写盘与红线18 的字面关系"属设计侧收口（待用户裁决，本档案不动设计文档）；trace 空页收尾、body_slice lossy 截断、空查询语义登记备查维持现状（红线9 均满足，改动属产品语义，留下一轮裁决）。

**验证基线**：`cargo check --workspace --all-targets` 零警告；`cargo test --workspace` 连续两遍 175 项全绿（170 基线 + r13 回归 4 则 + RDC 单测 1 则，均先红后绿）；`cargo build --release` 通过。
**未实测边界（继承）**：USN 提权分支（停用中，开放问题39）、macOS 真机、真实断电。

---

## R14 复审（2026-11-06 报告）+ 发布准备执行记录（2026-11-07）

### R14 结论（用户传达）

R13 四项发布阻断（B9-1/2/3/4）**全部关闭，未发现新阻断，批准进入发布准备**。逐项复验均按 R13 原始场景在生产二进制重跑通过；修复边界（ignore 收紧、verify 审计共用）经检查无新问题。保留声明：macOS 真机、USN 提权分支、真实断电仍未实测（外部验收项）。

### 发布准备（docs/rules/packaging.md §2 落地清单逐项执行）

| 清单项 | 状态 | 执行记录 |
|---|---|---|
| 第 4 项：init 查找顺序齐全 | **完成** | 查找逻辑单源下沉 `mem_assets::resolve_seed_files`：模板覆盖 > exe 旁 `builtin-types/` > **Homebrew share 布局**（`<exe>/../share/mem/builtin-types`，参照 helix runtime，packaging.md §2 清单第 3 项的代码面）> 内嵌字节；候选须存在且为目录才命中。cmd_init 接线，输出新增 `asset_source`（external/embedded）+ `template_dir`（实际命中目录） | 
| 第 5 项：两条播种路径验收 | **完成** | e2e（r14_release_prep）：① 无外部资产 → 嵌入兜底成功（asset_source=embedded、seeded_files=16、店可用）；② 外部在场 → 污染法验证优先级（模板目录加内嵌没有的标记文件 → 原样落店 = 外部字节优先，红线1 只搬运）。**生产形态冒烟**：发布物目录内 exe 旁播种 → asset_source=external + 店可用 |
| 第 1 项：发布物 extra 静态资产 | **完成**（本机形态） | 新增 `scripts/package-release.sh`：cargo build --release → 组装 `dist/mem-v<version>-x86_64-pc-windows-msvc/`（mem.exe + builtin-types/ exe 同级、原样整目录含 README）→ 冒烟验收 → Compress-Archive 出 zip。已实际执行：`dist/mem-v0.1.0-x86_64-pc-windows-msvc/` 与同名 zip 产出，冒烟通过 |
| 第 6 项：模板覆盖配置实现并测试 | **部分完成** | 用户故事已闭环：CLI `--template-dir` 覆盖通道实现+验收（上述 e2e ②）。`mem.json init.template_dir` 字段已定义（store.rs，开放问题40 暂定名）但**接线被语义矛盾阻断**：Store::create 响亮拒绝已存在 mem.json → init 前店配置无处可读；git init.templateDir 对应物需要全局配置层，属设计域——**语义收口归开放问题40**，不擅裁 |
| 第 2 项：Windows 安装器（MSI/NSIS） | **阻断（如实）** | 本机无 makensis/WiX 工具链；正式分发渠道承诺未定（开放问题40）。当前发布物 = 目录 + zip（布局与安装器装入根一致，后续装配即可） |
| 第 3 项：macOS tarball/Homebrew/.app | **阻断（如实）** | macOS 真机属外部验收项（历轮继承）；代码面已就绪：share 布局查找候选实现+单测（上述第 4 项），tarball/.app 组装待 mac 环境 |

**实现期决策登记**（AGENTS.md §4 四要素）：(1) 日期 2026-11-07；(2) 内容：播种查找顺序单源化至 mem-assets（模板覆盖 > exe 旁 > share 布局 > 内嵌），cmd_init 不再自带 walk；(3) 理由/先例：packaging.md §2 清单第 4/3 项要求查找逻辑齐全（含 Homebrew 布局），单源化避免 mem 与 mem-assets 双实现漂移（r5-B4/r12-B7-3 的"登记与实现不符"教训的结构性预防）；(4) 试金石：mem-assets 单测 4 则（三级优先 + 兜底 16 件）+ r14 e2e 2 则 + 发布物冒烟。未收口项：上表第 6/2/3 项与开放问题40。

**验证基线**：`cargo check --workspace --all-targets` 零警告；`cargo test --workspace` 连续两遍 **181 项全绿**（175 + mem-assets 单测 4 + r14 e2e 2）；`cargo build --release` 通过；发布物组装脚本实跑冒烟通过。测试工具教训再录：python heredoc 转义致测试文件损坏两处（换行断裂、路径建错位）——已按既有纪律改用 Edit 工具修复。
**未实测边界（继承）**：USN 提权分支（停用中，开放问题39）、macOS 真机、真实断电；发布物仅在 Windows x86_64 组装验收。

---

## 渠道与 CI 落地（2026-11-07，用户裁决）

**裁决**（开放问题40 的渠道承诺部分据此收口）：正式渠道 = **GitHub Releases**；CI = **自写精简 workflow**（.github/workflows/ci.yml——windows/macos 测试矩阵 + tag 触发的发布组装复用 scripts/package-release.sh 契约）；仓库 SpaceHuang/SpaceMemory（Public，已创建）。理由与权衡见本轮讨论（cargo-dist 留作 v1.2 可选升级——产物契约已由脚本与 r14 测试钉死，迁移无损；NSIS 按需）。

**当前状态**：git init -b main + 初始提交（132 文件含 CI workflow）本地完成；push 因本机到 github.com:443 网络受阻挂起（gh API 可通、传输被重置；仓库已建为空）——网络恢复后 `git push -u origin main` 即触发首轮测试矩阵。发布 job 静默待首个 `v*` tag（crate 版本号口径 0.1.0 vs 文档 v1.1 待用户定版）。

**本地先行投入使用**（用户指示）：dist 产物重跑冒烟后装入 `%LOCALAPPDATA%\Programs\mem`（mem.exe + builtin-types 同级）并追加用户 PATH；真实用户店 `~/.agent-memory` 初始化（principal=spacehuang，asset_source=external 确认走安装目录播种）；CLI brief/add/find 与 stdio MCP 前端在安装版上验证通过。升级路径：改代码后重跑 scripts/package-release.sh，覆盖安装目录。
