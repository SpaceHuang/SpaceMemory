# 写路径归属绑定——执行计划

- 版本：v1.0（2026-11-20；方案 `principal-binding-design.md` v0.6 终审通过后立项）
- 真源纪律：验收与不变量真源在方案 §2/§4，本计划只引用锚点、不重定义验收；本计划内不出现"验收标准"新条目。
- 门禁二分：T1–T7 全部本机可执行（cargo check / cargo test），**无外部验收项**——本切片不触碰平台差异代码（platform/ 不改），Windows 行为即全部行为。

## 前置（已完成）

- 方案五轮评审通过（B1–B7 关闭，决策①–⑦ 定稿）；评审记录 `docs/review/principal-binding-design-review.md`。
- 代码事实核验：`ops::add` 现序 = check_writable → **gitattrs** → 声明/校验 → 门闩 → 新记录锁 → 唯一性 → 写 → 事件；`delete_locked` 现序 = gitattrs → 依赖计算（锁外）→ 依赖阻止/告警 → principal 记录锁 → CAS → rename → 事件；`dangling_refs` 由两前端各自组装（又一处双前端重复，本计划单源下沉）；暂停先例 = `event.rs` `PauseHook`（显式参数）；锁协议 Drop 不删锁文件、获锁重写持有者时间戳。

## 任务（依赖只指向更早编号）

- [x] **T1 核心库·归属解析单源与两阶段校验**（方案 §2.1/§2.2，决策①②⑥）
  - `resolve_principal` 下沉 mem-core（CLI/MCP 现存两份重复实现改为调用核心）；
  - `ops::add` 重排：check_writable → 声明查找 → **归属解析链**（显式 > 台账[ctx.session_id → session::get] > 响亮拒绝；principal 类型豁免）→ 冲突告警（显式≠台账 → 显式胜出 + 中性告警）→ 既有校验 → 门闩 → 新记录锁 → **principal 记录锁**（锁序：新记录锁 → principal 锁 → 事件序列锁，单向）→ 锁内复核（scan 确认仍在 records/）→ gitattrs → 写 → 事件 → trail；
  - `WriteContext` 增 `pause_hook: Option<PauseHook>`（预检后暂停点，验收用；生产前端 None；先例 event.rs）；
  - 锁内复核失败 = 响亮拒绝（真相层净零，决策⑥分层）。
- [x] **T2 核心库·删除侧归属依赖**（方案 §2.1 删除侧对称闭环，决策⑦）
  - 依赖集合 = 引用边 ∪ 归属字段匹配（`envelope.principal == 目标 id`）；默认阻止（错误信息列依赖者）；`force` 越过 + 显式告警"归属悬空"；
  - 依赖复核移入被删 principal 记录锁内（锁外预检保留为快速失败）；
  - 活跃台账不阻止删除（决策⑦）。
- [x] **T3 核心库·verify 归属完整性审计**（方案决策⑦，r4 B7）
  - 新核心函数：非空归属 → 目标必须为 records/ 现存 principal 记录；断链条目三要素（来源记录、目标身份、修复入口——attic → 恢复身份记录；absent → 如实报告 + 历史/备份指引）；
  - 执行层统一扫描引用镜像与归属两字段域，不改信封字段；
  - 两前端 verify 输出并入 `dangling_refs`（单源调用核心，消除现存手工组装重复）。
- [x] **T4 前端·raw 透传与描述性更新**（方案 §2.2）
  - CLI `--principal` 帮助文本 + MCP instructions 各补一句（教学）；
  - CLI add / MCP mem_add 不再前端解析归属（raw 透传核心）；
  - MCP mem_session_start 改调核心 resolve（单源收口）。
- [x] **T5 破坏面修复·既有测试补显式归属**（方案 §1.2 F9）
  - 测试夹具增"播种身份记录"helper（豁免路径），`principal: None` 的 ops::add 调用逐一补 `Some(id)` 或 ctx 带 session；WriteContext 字面量构造点补 `pause_hook: None`（18 处，多在每文件 ctx helper 内）；
  - e2e（CLI/MCP）店经 init 自举已有首条身份，传 slug 即可。
- [x] **T6 新验收测试**（方案 §4.2 十组核心 + 双端 e2e，修复缺陷先写复现测试的纪律适用）
  - mem-core 新测试文件：十组逐组落位（锁外拒绝全店逐字节一致必测"有 .git 缺 .gitattributes"反例；并发双分支用 pause_hook 可控暂停；死锁守护多轮交错超时内完成）；
  - CLI e2e 三态 + MCP e2e 三态（断言逐字对齐，拒绝用例断言前置反例店无文件变化）。
- [x] **T7 全量门禁**
  - `cargo check` 零警告（全 workspace）；`cargo test` 全绿（全 workspace）；如实报告结果。

## 回退

实现按 T1→T2→T3 分立提交面，任一环节失败可独立回退到上一任务末态；核心行为开关无（决策①不做模式开关），回退 = git 层面还原对应任务改动，无数据迁移问题（B1 契约：格式层零变更）。

## 完成记录（2026-11-20）

- 全量门禁：42 个测试套件全 ok、0 失败；`cargo check --workspace` 零警告（T7，实跑证据）。
- 破坏面实修数字：测试面 `principal: None` 的 ops::add 调用约 61 处补显式归属；CLI e2e spawn 53 处插 `--principal codex_main`（init 自举身份短名）；13+2 处 WriteContext 构造补 `pause_hook: None`；Envelope 直构与豁免调用点保持不动（与 F9 对账）。
- 实现期裁决两则（就近记录于此，未触设计领土）：
  1. **测试播种 helper 采用直写形态**（同 seed_project 先例）：不经受管创建播种身份记录——不产生事件，避免污染既有"事件数"断言；直写身份记录以漂移形态出现的三处（acceptance diff、r11 回填、mcp diff）断言按"过滤播种记录"适配，测试本意不变；
  2. **add 路径不再向 validate_payload_and_refs 传归属**：归属存在性由两阶段校验独自承担（§2.1），移除暂停点之后的重复扫描——否则并发删除先完成时会先触发旧检查点、抛出与锁内复核不一致的错误文案（principal_binding 并发测试抓出，修复后锁内复核文案唯一）。
- 新增验收面：`crates/mem-core/tests/principal_binding.rs`（9 个测试覆盖 §4.2 十组，含全店快照断言、pause_hook 可控暂停双分支并发、死锁守护）+ CLI e2e 三态 + MCP e2e 三态。

## 实施后评审修复记录（2026-11-20，重审通过）

1. **同路径自锁（高）**：重复创建同一 slug 的 principal 且显式指定自身为归属时，归属锁与新记录锁同路径、记录锁不可重入 → 30 秒超时绕过输者协议。修复：获取第二把锁前判定 `same_path`，同路径复用已持记录锁完成复核；输者协议照常。回归：`duplicate_principal_self_ownership_returns_loser_protocol`（输者断言 + 非同路径对照）；评审 CLI 复现命令实测 0 秒返回 `created: false`。
2. **锁内拒绝创建 .gitattributes（中）**：重放实现保留了 `ensure_for_write` 的 HEAD 最前位置，先于锁内复核执行——违反决策⑥"锁内拒绝真相层净零"。修复：移至锁内复核成功之后、记录文件创建之前（BD5 纪律保留）。反例并入并发测试（删除先完成分支）：`git init` + 移除该文件 → 锁内拒绝 → 断言未创建且真相层快照不变。
3. **非阻断四项**：CLI `--principal` 帮助文本补继承/拒绝规则（T4 补位）；envelope.rs 空归属注释改 §2.5 定稿文案；死锁守护改 `is_finished()` 轮询 + 30 秒 deadline；ops.rs 末尾空行清除。
4. 门禁复验：42 套件全绿、零警告、principal_binding 10 项全过、`git diff --check` 通过。
