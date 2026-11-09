# R11 二进制内容承载实施评审——第二轮（B1/B2 修复复审）

- 评审对象：`worktrees/r11-binary-content`，HEAD `be6723e`；修复提交 `e32d8a5`
- 评审日期：2026-11-10
- 前序报告：`binary-content-implementation-review.md`（B1/B2）
- 评审环境：Windows 11，真实 CLI 与 git 子进程
- 评审方式：修复差异读码、原始场景与补充边界生产复跑、新增回归及全工作区测试

## 评审结论（TL;DR）

**B1/B2 均已关闭，本轮未发现新阻断；批准 R11 实施通过复审。**

结论以生产 `mem.exe` 的真实行为为主，不仅采信实施侧处置登记：B1 的两种修复入口均能完成后续 `history` 自动恢复；B2 在顺序、并发和只读店场景下均保持不覆盖与字节保真。

## 逐项复验

| 项 | 结果 | 代码与行为证据 |
|---|---|---|
| B1 `fix` 修复版本未入史 | **关闭** | `crates/mem-core/src/ops.rs:743-781` 提取统一 `commit_batch`；`fix` 在事件后调用同批提交（`ops.rs:1149-1151`），CLI 在有 `.git` 时接入提交汇。生产复跑确认修复文件与 HEAD 逐字节一致；随后正文损坏，`restore-body history` 成功、版本 +1、正文恢复原字节、`verify --deep` 不再漂移 |
| B1 commit-pending 收敛 | **关闭** | 核心测试锁定批次包含记录、`events.jsonl`、`.gitattributes`，失败生成 pending；真实 CLI 回归 `b1_fix_commit_failure_creates_pending_and_converges` 覆盖陈旧 `index.lock` → pending → `reconcile` → HEAD 入史 → `history` 可用，全量执行通过 |
| B2 `--body-out` 覆盖竞态 | **关闭** | `crates/mem/src/commands/mod.rs:930-945` 改用 `platform::create_new` 原子独占发布，不再执行“exists 后 fs::write”。Windows 使用不带覆盖标志的 `MoveFileExW`，macOS 使用 `link(2)`，目标存在均不替换。既有目标拒绝且原字节不变；12 组双进程并发导出均为单赢家，输出字节完整 |

### B1 原始场景与补边界

1. 合法手改载荷 → `fix --expect` → 翻转正文 → `restore-body history`：通过；修复字节 == HEAD，恢复正文 == 原图字节，深档审计无漂移。
2. 结构头损坏后经显式 envelope/payload 修复为不同内容 → 翻转正文 → `restore-body history`：通过；修复字节同样进入 HEAD，恢复正文逐字节一致。
3. 上述场景均使用系统临时目录和真实 git，不经过 fake 历史通道；临时店已随测试进程清理。

### B2 补充边界

- 既有文件包含二进制字节：拒绝后逐字节不变；
- 两个进程同时向同一不存在目标导出：12/12 均为一成一拒，成功输出 == 原正文；
- 强制只读探测形态：店外 `--body-out` 仍成功且字节一致，符合 G3。

## 验证记录

1. `cargo check --workspace --all-targets`：通过，零 Rust 警告；
2. `cargo test --workspace --no-fail-fast`：**244 通过 / 0 失败**，最终退出码 0；
3. `git diff --check main...HEAD`：通过；
4. 首次测试执行曾遇 Windows `LNK1104`（无法打开测试输出 exe），未进入测试；通过串行完成测试目标链接后重跑标准命令全绿。该环境故障不计为代码失败，也不替代最终通过记录；
5. `cargo fmt --all -- --check` 未执行成功：当前工具链未安装 `rustfmt` 组件；它不是本轮新增门禁，未安装组件或修改环境；
6. 复审未修改实现代码；工作树无评审产生的临时文件。

## 自曝三处

1. **B2 并发实测不是对旧实现的反证实验**：旧实现的微小 TOCTOU 窗口未做时序注入；本轮关闭依据是原子独占原语的系统调用语义，加上新旧顺序/并发行为观测，而非声称每次调度都命中原窗口。
2. **macOS 仅做实现读码**：`link(2)` 独占路径与既有原语一致，但本轮没有 macOS 实机执行；该外部验收边界不随 Windows 通过消失。
3. **本轮是定向复审**：覆盖 B1/B2 修复、相邻提交/只读边界及全量自动化，不重新逐条人工执行 R11 的全部验收；其余行为由 244 项测试承接。

## 待批准清单

**本轮请求批准**：B1/B2 关闭；R11 实施通过复审，可进入后续合并/发布流程（发布仍需遵守既有打包规则）。

**本轮明确不批**：macOS 实机验证已完成；`rustfmt` 已验证；需求、设计、规范或最终命令命名变更。
