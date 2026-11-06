# AGENTS.md —— SpaceMemory 仓库工作规则

> 适用对象：在本仓库工作的 AI Agent 与人。
> 规则本体住 `docs/` 与各 README，本文件只做指针与纪律——不复制内容，避免双源漂移（红线4"用指针不用全文拷贝"在本仓库自身的应用）。
> 注意区分：本文件 ≠ 产品资产 `adapters/*/AGENTS.md` 模板——那是记忆系统的宿主注入物（§8 第 7 步才产出），两者勿混淆。

## 1. 项目一句话

为 Agent 建设外置的第二记忆：本地数据目录（唯一真相）+ Rust 多调用二进制 `mem`（CLI / sidecar / MCP 三前端合一）+ 类型声明两件套 + 宿主薄适配。当前阶段：**v1.1 已实施（§8 第 1–8 步）；三轮实施评审（首轮 + r2 + r3）全部阻断/高优项修复完成（2026-11-05，逐项处置登记见 [docs/review/mem-v1.1-fixes.md](docs/review/mem-v1.1-fixes.md)）**——约 151 项测试全绿、零警告，可进入发布准备。

## 2. 文档地图（权威关系）

| 文档 | 权威范围 |
|---|---|
| `docs/requirement/agent-memory-requirements.md` | 需求基线 v1.2——一切设计与验收的起源与评审标尺 |
| `docs/develop/agent-memory-design.md` | 设计现状 v1.2——决策（D#）、红线（§5）、实施顺序（§8）、开放问题（§7）、实施裁决注记 |
| `docs/specs/` | 三个公开规范 v1（envelope-format / type-declaration / conventions-source）；规范与实现冲突时以规范评审结论为准 |
| `docs/rules/` | 操作型指南（流程性规则，如打包分发 `packaging.md`）——执行对应任务时按需读取全文，本文件只登记存在与触发时机 |
| `docs/review/` | 评审记录归档（历史评审文件缺失，待补） |
| `docs/explain/` | 解释层：架构图与网页格式的易读说明/介绍——**非权威**，不承载规则；与规范/设计冲突时以后者为准，漂移修 explain 不修真源 |
| 各目录 `README.md` | 该目录的局部规则与裁决记录（如 `assets/builtin-types/README.md`） |

### 任务触发必读（做下列任务前必须先读对应文档——不得跳过，不得凭印象替代）

| 触发任务 | 必读文档 |
|---|---|
| **打包 / 发布 / 配置发布工具**（cargo-dist、MSI/NSIS、Homebrew 等） | [docs/rules/packaging.md](docs/rules/packaging.md) §2 落地清单——逐项执行，**禁止只发裸 exe** |
| **写 / 改需求、规范 / 技术方案、执行计划**（一切交人评审的文档） | [docs/rules/document-writing.md](docs/rules/document-writing.md)——§0–§1 评审友善度条款对一切评审文档生效；按篇内路由读对应小节 |
| 新增或修改 `assets/builtin-types/` 下的类型两件套 | [assets/builtin-types/README.md](assets/builtin-types/README.md)——红线1 裁决与试金石 |
| 编写锁 / 并发 / 事件日志 / 记录格式相关代码 | 设计文档 §5 红线全文（27 条逐条过；常见雷区清单见本文件 §5） |

> 维护纪律：`docs/rules/` 新增任何指南，必须同时在上面表里登记一行触发映射——**没有触发行的指南等于不存在**。

## 3. 术语锚点

引用规则时用统一锚点，代码注释、README、讨论通用：

- `R#` = 需求编号（R1–R9），分项如 `R3.2`；
- `D#` = 设计决策编号（D1–D19）；
- `红线#` = 设计文档 §5 的编号（1–27）；
- `§8-步` = 实施顺序第 N 步（如 §8-4 = 核心库）；
- `开放问题#` = 设计文档 §7 未决条目（25 起）。

## 4. 决策落档纪律

- **设计级决策** → 设计文档（新 D# + 修订历史）。设计文档的版本与修订行由用户主导，Agent 不擅自增删版本记录；
- **实现/脚手架期决策** → 记录在最近的 README（就近原则）；若触及设计领土，在设计文档 §7 开放问题补条目（编号顺延），标注"方向已裁决 + 未收口项"；
- 每条裁决四要素：**日期、内容、理由/先例、试金石或护栏**，另附未收口项。
- 范例：混合分发裁决 → `assets/builtin-types/README.md`（裁决全文）+ 设计文档开放问题40（指针）。

## 5. 代码工作规则

- 实施按设计文档 §8 顺序推进；每个占位模块的文档注释标了 D#/红线#/§8 锚点，**按图索骥，不凭记忆重设计**；
- **红线是硬约束**：写任何读写/并发/格式代码前先过设计文档 §5 全部 27 条。常见雷区：读路径零写盘（18）、机制再生成 frontmatter 时正文按字节保持（27）、锁序全库统一（20/24）、索引永不 gate 正确性（8）；
- 载荷与内容类型禁止编译进二进制（红线1，裁决解释见 `assets/builtin-types/README.md`）：字节级播种只允许发生在 `crates/mem-assets`（只搬运、永不 parse；改类型只改数据文件，不改代码）；
- 依赖随实施已启用（rusqlite / jsonschema / clap 等）；rmcp 仍为注释（MCP v1 为手写最小 stdio 协议，工具面定稿后装配，开放问题29）；
- 平台差异（Windows / macOS）集中在 `crates/mem-core/src/platform/`，其他模块不写平台代码；
- 操作型指南住 `docs/rules/`，AGENTS.md 只登记存在与触发时机，按需加载全文；触发映射统一登记在 §2"任务触发必读"表（单一登记处，勿在他处复述）；
- 注释与文档用中文，风格与既有文件保持一致。

## 6. 验证与完成定义

- 交付代码前：`cargo check` 零警告、`cargo test` 通过；**如实报告结果**——失败说失败，跳过说跳过，完成才说完成；
- 验收测试已实装并持续维护：`crates/mem-core/tests/`（acceptance / backfill / locks / ops / session_view / maintenance / foundation / sidecar）、`crates/mem/tests/`（cli e2e / fastpath e2e）、`crates/mem-git/tests/`（真实 git 子进程）。改动对应模块时必须保持对应验收面全绿；**修复缺陷时先写复现测试再修**（评审 B1/B2 的教训：测试面跟着实现走会漏边界）。

## 7. 流程边界（不做什么）

- 未经用户要求：不 `git init`、不 commit、不 push；
- 不动设计文档的版本与修订行；对设计的异议提给用户裁决，不改评审结论；
- 不在仓库内创建用户数据目录（`~/.agent-memory` 是运行时产物，与本仓库无关）；
- 不引入设计文档 §1.3 已否决的形态（云记忆服务、常驻守护、每宿主独立实现、图数据库引擎、SQLite 唯一存储等）。
