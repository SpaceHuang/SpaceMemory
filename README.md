# SpaceMemory —— Agent 记忆与状态层（第二记忆）

> 为 Agent 建设一个外置的**第二记忆**：持久的、用户自有的、多方可共享的、访问有预算的、可信可验证的、对错误写入者健壮的、强制与教学分工明确的、能以最低门槛覆盖一切宿主的。
> —— [需求文档](docs/requirement/agent-memory-requirements.md) §1.3

**形态**（[设计文档](docs/develop/agent-memory-design.md) §1）：用户级本地数据目录（唯一真相）＋ 一个 Rust 多调用二进制 `mem`（CLI / sidecar / MCP server 三前端合一）＋ 随仓库走的类型声明与约定（规格资产）＋ 各宿主可选的薄适配。

## 文档

| 文档 | 位置 | 状态 |
|---|---|---|
| 仓库工作规则（Agent 约定） | [AGENTS.md](AGENTS.md) | 持续维护 |
| 需求基线（起源与评审标尺） | [docs/requirement/agent-memory-requirements.md](docs/requirement/agent-memory-requirements.md) | v1.2 |
| 设计文档 | [docs/develop/agent-memory-design.md](docs/develop/agent-memory-design.md) | v0.9 评审通过（零阻断） |
| 公开规范（D8：信封格式 / 类型两件套 / 约定事实源） | [docs/specs/](docs/specs/README.md) | v1 定稿（2026-11-05） |
| 操作指南（打包分发等流程性规则，按需加载） | [docs/rules/packaging.md](docs/rules/packaging.md) | 随任务触发 |
| 评审记录 | [docs/review/](docs/review) | 归档处（历史评审文件未随仓库提供） |

## 目录结构

```
SpaceMemory/
├── Cargo.toml                  # workspace 根（members = crates/*）
├── rust-toolchain.toml
├── crates/
│   ├── mem-core/               # 机制核心库（设计文档 §1.1 机制层）
│   │   └── src/
│   │       ├── envelope.rs     # 信封：机制拥有的公共字段（D18/R8.3）
│   │       ├── record.rs       # 记录文件读写；机制只再生成 frontmatter，正文按字节保持（红线27）
│   │       ├── token.rs        # 修订令牌 v<版本>@<全SHA-256> CAS（红线19）
│   │       ├── paths.rs        # 规范路径与键字符集：NFC/casefold/黑名单/保留名（D19/D11）
│   │       ├── store.rs        # 数据目录布局（§3.1）
│   │       ├── lock.rs         # 门闩→记录锁→序列锁；锁序全库统一；陈旧锁判定（红线20/22/24）
│   │       ├── event.rs        # events.jsonl + 状态三元组 + 状态覆盖回填（D12/红线24）
│   │       ├── drift.rs        # current_drift 漂移扫描（D12/红线26）
│   │       ├── freshness.rs    # 证明式新鲜性协议（D5/红线21）
│   │       ├── session.rs      # 会话台账与接手（D13/红线25）
│   │       ├── agenda.rs       # 提案队列（D6/R5.5）
│   │       ├── budget.rs       # 硬预算与截断标注（R4/红线9）
│   │       ├── refgraph.rs     # 引用完整性/派生 DAG/未消化证据（D16/D17/R9）
│   │       ├── view.rs         # 视图（D10）
│   │       ├── sidecar.rs      # 单实例握手/懒拉起/空闲自退（D2/红线5）
│   │       ├── platform/       # 平台原语：windows（LockFileEx/USN）/ macos（fcntl/FSEvents）
│   │       └── error.rs        # 统一错误；冲突是一等结果（D3/R6.1）
│   ├── mem-schema/             # 类型声明两件套：加载 + x-* 关键字校验（D19/R8.5–8.8）
│   ├── mem-index/              # index.sqlite 派生索引 + 检查点 + FTS（D5/红线8/21）
│   ├── mem-git/                # Git trait（D4）：CLI 子进程首发 → git2
│   ├── mem-mcp/                # MCP 前端（rmcp；工具面 schema 携带 expect 全令牌）
│   ├── mem-assets/             # 播种资产字节载体（混合分发：外部优先、嵌入兜底；只搬运永不 parse）
│   └── mem/                    # `mem` 多调用二进制：CLI / serve / mcp（D7）
├── assets/
│   └── builtin-types/          # `mem init` 播种用的类型两件套（红线13 自举检验）
├── adapters/                   # 宿主适配薄壳（zcode/、generic/；§3.4 四档渐进增强）
└── docs/
    ├── requirement/            # 需求基线
    ├── develop/                # 设计文档
    ├── specs/                  # 公开规范（D8）
    ├── rules/                  # 操作型指南（打包分发等；按需加载）
    ├── explain/                # 解释层：架构图与网页易读说明（非权威）
    └── review/                 # 评审记录
```

## crate 一览

| crate | 形态 | 职责 | 锚点 |
|---|---|---|---|
| `mem-core` | lib | 机制核心：信封、锁、令牌 CAS、事件日志与回填、新鲜性证明、台账、agenda、预算、引用/派生维护、平台原语 | §1.1 机制层 |
| `mem-schema` | lib | 类型声明两件套加载与校验（strict/lenient 分档、唯一键、引用存在性、DAG） | D19、R8 |
| `mem-index` | lib | index.sqlite 派生缓存：过滤/排序/分页/FTS/边表；约束仅对账诊断，永不拒收 | D5、红线8 |
| `mem-git` | lib | Git trait：path-scoped 提交、事件日志同批提交 | D4 |
| `mem-mcp` | lib | MCP server 工具面（L4 档） | D7、§3.4 |
| `mem-assets` | lib | 播种资产字节载体：`assets() -> [(路径, 字节)]`，只搬运、永不 parse | 红线1 裁决（assets README）、开放问题40 |
| `mem` | bin | 多调用入口：`mem`（CLI）/ `mem serve`（sidecar）/ `mem mcp` | D7、§3.3 |

## 平台

Windows 与 macOS。平台差异集中在 `crates/mem-core/src/platform/`：字节范围锁（LockFileEx / fcntl F_SETLK）、原子改名、OS 变更日志订阅（NTFS USN journal / macOS FSEvents —— 证明式新鲜性的覆盖来源，D5）。

## 开发

```sh
cargo check     # 编译检查
cargo test      # 测试（含 mem-core 验收测试占位）
cargo build     # 构建
```

实施顺序见设计文档 §8。当前状态：**§8 第 1–8 步已全部实施（TDD，2026-11-05）**——

```sh
cargo test      # 全工作区测试绿（约 140 项：规范/类型/核心/CLI/索引/sidecar/MCP/维护）
cargo build --release
```

已交付能力（对应 §8 步骤）：
1. 格式规范 v1 三件套（docs/specs/）+ 内置类型两件套播种资产（assets/builtin-types/，principal/view 自举经类型声明引入，红线13）；
2. 机制核心（mem-core）：门闩→记录锁→序列锁、全摘要令牌 CAS、events.jsonl 与状态覆盖回填（absent 一等状态、锁内判定、并集枚举）、证明式新鲜性（USN FRN 快照订阅 / 直读回退）、漂移扫描 diff 契约、会话台账与接手（幂等键含 task_ref）、视图/预算/引用图/merge；
3. `mem` 多调用二进制：CLI 命令面（§3.3）+ `mem serve`（sidecar：单实例握手/空闲自退/索引热维护）+ `mem mcp`（stdio JSON-RPC 工具面，expect 全令牌约束）；
4. 宿主适配薄壳：adapters/zcode（Skill + AGENTS.md 指针 + hook + MCP 配置）、adapters/generic（L1/L2 模板）；
5. 维护例程：agenda 提案扫描（重复/断引用/环/未消化证据/存疑规则）、apply（独占门闩 + executing 检出）、fix（git 历史重建 + 正文按字节保持）、verify/reconcile。

已知边界（如实）：macOS FSEvents 订阅未接（永久直读形态，正确性不受影响，红线21 不破）；FTS5 unicode61 对 CJK 为前缀匹配；MCP v1 为手写最小协议（rmcp 装配推迟至开放问题29 工具面定稿）。
