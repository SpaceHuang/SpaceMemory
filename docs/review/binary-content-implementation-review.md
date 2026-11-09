# R11 二进制内容承载实施评审

- 评审对象：`worktrees/r11-binary-content`，HEAD `9a2b678`；基线 `main` `bbf6bad`
- 评审日期：2026-11-10
- 评审环境：Windows 11，真实 CLI 与 git 子进程
- 评审标尺：需求基线 v1.4 R11；`binary-content-design.md` v0.3；主设计 D3/D4 与红线 6/19/20/27
- 评审方式：相对 `main` 的全量差异读码、全工作区检查与测试、生产 `mem.exe` 复合损坏场景复现

## 评审结论（TL;DR）

**不予通过：发现 2 项阻断（B1/B2）。** B1 使设计明确承诺的“先 `fix`、再 `restore-body history`”复合修复链在 `fix` 产生新文件字节时失败；B2 的 `--body-out` 存在检查后使用（TOCTOU）竞态，可能覆盖调用方既有文件。两项关闭前不宜合并或发布 R11。

基础字节通道、非 UTF-8 检索门槛、MCP base64 分页、显式来源恢复和常规 `history` 恢复的现有测试均通过；测试全绿不覆盖下列两个边界。

## B1（阻断）：`fix` 不提交修复版本，导致后续 `history` 自动恢复找不到锚

**位置**：

- `crates/mem/src/commands/mod.rs:1282-1319`：CLI `cmd_fix` 只传 `HistoryStore`，没有提交汇；
- `crates/mem-core/src/ops.rs:1037-1044`：`fix_record` 接口没有 `WriteContext` 或 `CommitSink`；
- `crates/mem-core/src/ops.rs:1114-1130`：原子写入并追加 `op=fix` 事件后直接返回，未调用 `commit_same_batch`；
- `crates/mem-core/src/ops.rs:1321-1353`：`history` 以最后机制事件的 `new_digest` 为锚扫描 git 历史，无匹配即拒绝。

**缺陷机制**：`fix` 是机制写入，会把修复后的全文摘要登记为最后事件，但修复后的记录与事件均未进入 git，也不会生成 commit-pending。只要修复结果与既有 HEAD 字节不同，随后发生正文外部损坏时，`history` 的可用窗口判定仍会通过（漂移仍在且最后事件 `kind=op`），但锚摘要必然无法在历史中找到，恢复以退出码 1 失败。这不是设计允许的“提交滞后”：此处没有提交动作，也没有可重试的 pending 项。

**生产复现（已确认）**：

1. 初始化带 git 的临时店，以 CLI 添加一条含非法 UTF-8 正文的 `memory` 记录；
2. 合法手改载荷 `title`，经 `show` 取当前令牌，再执行 `fix --expect <当前令牌>`，使 `fix` 产生与 HEAD 不同的新文件字节；
3. 外部翻转正文一个字节，重新经 `show` 取损坏后的全摘要令牌；
4. 执行 `restore-body <id> --expect <损坏令牌> --source history`。

实测第 2 步后修复文件摘要与 HEAD 文件摘要不同；第 4 步返回：

```text
history 锚定失败：锚摘要 … 未在 git 历史中找到——锚版本不在跟踪范围或历史被改写；
改用显式来源 git:<版本> 或 file:<店外路径>
```

失败后记录保持损坏字节不变，没有静默写盘；但自动恢复不可用，且错误成因误诊为“不在跟踪范围或历史被改写”。显式 `git:`/`file:` 绕行不能消除本项：设计 §3.6 明确把“先 fix 后 restore-body”列为复合损坏处置序，而 `history` 的可信锚依赖机制写入随批入史。

**关闭条件**：

1. `fix` 复用统一写路径提交纪律，将记录、`events.jsonl` 与适用的 `.gitattributes` 纳入同批提交；失败时生成可重试的 commit-pending，不得静默无历史；
2. 新增真实 CLI 回归：先让 `fix` 产生新字节（合法手改后规范化，或显式提供不同修复原料），再翻转正文并执行 `restore-body history`；断言恢复成功、正文等于修复后来源、版本与事件正确、复跑深档校验不再漂移；
3. 回归同时断言修复版本已进入 HEAD，且提交失败时 pending 可收敛。

## B2（阻断）：`--body-out` 的“不覆盖”检查与写入分离，可覆盖并发创建的既有文件

**位置**：`crates/mem/src/commands/mod.rs:924-941`。

**缺陷机制**：`write_body_out` 先以 `path.exists()` 检查目标，再在 `crates/mem/src/commands/mod.rs:936` 调用 `std::fs::write`。后者会创建或**截断**目标。另一进程若在两步之间创建该路径，`show --body-out` 会截断并覆盖它，直接违反 BD1“目标已存在 → 响亮拒绝（读命令不做覆盖写）”的承诺，并可能造成店外工作区数据丢失。

现有测试 `show_body_out_rejects_existing_target_and_store_paths` 只覆盖调用前已经存在的路径，无法覆盖该竞态。本项由系统调用语义与代码顺序直接确认，未做时序对抗复现。

**关闭条件**：

1. 以原子独占创建替代“先查再写”；仓库已有 `platform::create_new` 的“完整临时文件 + 不覆盖原子发布”原语，可复用于店外输出路径；
2. 已存在目标必须原字节不变并返回结构化拒绝；并发创建窗口不得依赖事后检测；
3. 新增可控交错回归，在独占发布前制造目标已存在，断言既有文件不被截断。

## 验证记录

1. `cargo check --workspace --all-targets`：通过，零 Rust 警告；
2. `cargo test --workspace --no-fail-fast`：全部测试通过，无失败；
3. `git diff --check main...HEAD`：通过；
4. B1 使用 `target/debug/mem.exe` 在系统临时目录真实复现，临时店随进程退出清理；
5. 测试日志中的 git LF→CRLF 提示来自既有测试构造的外部 git 场景，不是编译警告或测试失败。

## 自曝三处

1. **B1 复现采用“合法手改载荷后 fix”而非“结构头与正文同时坏”**：两者都产生“fix 新字节未入史”的同一状态；前者可稳定取得 CAS 令牌。修复后仍应补一个显式修复原料改变字节的结构损坏变体，防止只覆盖单一入口。
2. **B2 未做真实并发时序复现**：结论由 `Path::exists` 与 `std::fs::write` 的创建/截断语义直接推出；关闭验收必须用可控交错证明原子独占，而不是重复顺序版“目标已存在”测试。
3. **本轮平台实测仅 Windows**：通用字节与 git 逻辑经代码审查，macOS/Linux 的真实文件发布行为仍需由对应平台测试或外部验收确认；本报告不将其视为已实测。

## 待批准清单

**本轮请求批准**：评审结论“不予通过”；B1/B2 为合并与发布阻断，按各自关闭条件修复后复审。

**本轮明确不批**：R11 需求、设计或规范语义变更；最终命令命名；macOS/Linux 实机行为已验证的声明；本报告未列出的非阻断改进项。

## 处置登记（2026-11-10，实施侧响应——不改评审正文）

**B1（已关闭）**：
- `ops::commit_batch` 从 `commit_same_batch` 提取为统一提交入口（记录路径 + events.jsonl + 适用 .gitattributes；失败转 commit-pending）；`fix_record` 增 `committer` 参数接入，原子写 + `op=fix` 事件后同批提交，失败转可重试 pending（D17 纪律，不回滚真相层）；
- CLI `cmd_fix` 有 .git 时挂提交汇（红线10：无 git 形态传 None）；
- 关闭条件 2/3 回归：`r11_restore_body.rs` 核心级 2 项（批次构成含 .gitattributes / 失败落 pending）；`r11_binary_content.rs` CLI 级 2 项（评审复现链：合法手改→fix→HEAD 核对→翻转正文→restore-body history 成功 + 版本/事件/深档收敛断言；陈旧 index.lock 注入提交失败→pending→reconcile 收敛→HEAD 入史→history 恢复可用）；
- 锚无匹配的成因诊断随之自愈：pending 在场时报"commit-pending 未收敛（等待 mem reconcile 重试提交后重试）"，不再误诊为"历史被改写"。

**B2（已关闭）**：
- `write_body_out` 以 `platform::create_new` 原子独占创建替代"exists 检查 + fs::write"（完整字节写同目录临时文件并同步 → 不覆盖原子发布；目标已存在 → 发布失败、目标分毫不动、临时文件清理）——检查与发布合一，TOCTOU 窗口结构性消除，并发正确性不依赖事后检测；
- 关闭条件 3 回归：`show_body_out_rejects_existing_target_and_store_paths` 增"既有文件原字节不变"断言（独占发布前目标已存在 = 可控交错形态；时序对抗注入不可确定性复现，窗口消除由原子原语语义承担——与评审自曝 2 的口径一致）；
- 同步收敛单一来源：店内路径判定删 CLI 本地副本，统一走 `mem_core::paths::path_is_inside`。

**验证**：`cargo check --workspace --all-targets` 零警告；`cargo test --workspace --no-fail-fast` 244 通过 / 0 失败（基线 240 + 新增 4）。
