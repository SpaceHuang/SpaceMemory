# R11 二进制内容承载实施评审——第四轮（r3 修复复审）

- 评审对象：`worktrees/r11-binary-content`，HEAD `1c65137`；修复提交 `2dfec8e`（H1/H2/L1/L2）＋`1628dcd`（观察项 2）
- 评审日期：2026-11-10
- 前序报告：`binary-content-implementation-review-r3.md`（r3：H1/H2 高优、L1/L2 低优、观察 2 项）
- 评审环境：Windows 11，真实 CLI 与 git 子进程
- 评审方式：修复差异读码、r3 全部原场景生产重放（不采信在库测试独证）、重写新边界探针、全工作区检查与测试

## 评审结论（TL;DR）

**H1/H2/L1/L2 全部关闭，本轮未发现新阻断或新高优；批准 R11 实施通过复审。**

结论以生产 `debug mem.exe` 的真实行为为主：r3 的全部原始复现场景重放关闭，包括当时需要 1MB 样本、cwd 在店内、兄弟目录三个触发形态；`path_is_inside` 重写引入的新边界（大小写变体、正斜杠、不存在深层目录）另做探针，方向与语义均正确。观察项 1 维持备查口径；本轮新增 1 条既有代码观察项（不属 R11 范围，登记）。

## 逐项复验

| 项 | 结果 | 代码与行为证据 |
|---|---|---|
| H1 描述符超 128 上界 → debug 读路径 panic | **关闭** | `body.rs` 新增 `byte_count_display`（<1MB 原数字 ≤6 位；≥1MB 整数工程缩写 ≤6 字节，`usize::MAX` 落 "18EB" 档）＋模板收紧（摘要前缀 8→6 hex、破折号 6→3 字节），任意 usize 总长 ≤126；`debug_assert` 保留。生产重放：1MB 记录 `show` 默认形态正常输出，描述符 122 字节、显示"1MB"、无 panic |
| H2 裸相对路径绕过店内拒绝（`--body-out` 与 `file:` 源两点） | **关闭** | `paths.rs` 重写：目标与店根均按进程 cwd 绝对化（cwd 不可得保守判内）→ 词法折叠＋分隔符边界前缀 → canonicalize 兜底（缺失目标沿祖先链取最近已存在祖先）。生产重放：cwd=店根 `--body-out smuggled.png` 结构化拒绝且店内无文件；`restore-body --source file:inside-backup.md`（店内相对路径）拒绝；对照组（cwd 店外同名相对路径、店外备份源）均放行且字节一致 |
| L1 词法前缀误拒（兄弟目录；相对店根 `./x`、`../x`） | **关闭**（与 H2 同面重写） | 生产重放：`./store-backup/out.png` 兄弟目录放行导出成功；`--store .`、cwd=店根的 `../up-level.png` 上级路径放行。`lexically_within` 的 `== root 或 starts_with(root + 分隔符)` 消除字符串前缀误命中 |
| L2 `file_bytes_at` 依赖 stderr 英文文本 | **关闭** | 判据全改退出码三分支：`rev-parse --verify --quiet`（版本有效）→ `cat-file -e`（存在性，失败 = Ok(None)）→ `cat-file blob`（取字节）；全文件 grep 无 `stderr.contains` 残留；三分支语义回归在三类 locale 声明下重跑（判据已与消息文本机械无关） |
| 观察项 2（base64 逐页拼装说明） | **已处置** | `mem_show` 工具描述补"逐页独立编码（含各自 padding）——宿主必须逐页解码后按字节拼接，不得直接拼接各页 base64 文本" |

## 重写新边界探针（本轮追加，不依赖在库测试）

1. **大小写店根变体＋正斜杠**（cwd=店根，`--body-out ./STORE/smuggled3.png`）：词法小写折叠判内 → 拒绝、未落盘——Windows 大小写不敏感不可经大小写绕过；
2. **不存在的深层新目录**（`./store-backup/newdir/deep/out.png`）：判定放行（祖先链兜底方向正确），落盘如实报 IO"找不到路径"而非误报"店内"——误拒方向已消除且错误成因不误导；
3. **正斜杠绝对路径导出**：成功且字节一致——常规形态无回归。

## 验证记录

1. `cargo check --workspace --all-targets`：通过，零 Rust 警告；
2. `cargo test --workspace --no-fail-fast`：**258 通过 / 0 失败**（r2 基线 244 ＋修复新增 14：单元 10、CLI e2e 4——含 cwd=店根真实子进程 `current_dir` 双向对照）；
3. r3 全部原场景生产重放关闭（见逐项复验），临时店与样本随评审清理；
4. 处置登记形态合规：实施侧响应以追加节落在 r3 报告文末，评审正文一字未动；
5. 本轮复审未修改实现代码。

## 新增观察项（登记备查，不要求动作、不属 R11 范围）

**`commit_paths` 的 stderr 文本判据是 `main` 既有代码**（`"nothing to commit"` / `"index.lock"`，`git show main:crates/mem-git/src/process.rs` 可证先于 R11 存在）——与 L2 同根源：非英文 locale 下"无 diff 提交"（如 fix 确定性再生成后字节与 HEAD 相同）会被误判失败转 commit-pending，reconcile 重试同样失败。建议后续按 L2 的退出码判据理念统一收口（`git commit` 可用 `--dry-run` 或退出码＋空 SHA 判定）；本轮不构成 R11 缺陷。

## 自曝三处

1. **H2 探针未做符号链接/junction 对抗**：canonicalize 兜底的链接解析在 Windows junction 与 Unix symlink 下行为读码确认、未构造链接农场式对抗复现；该面由 canonicalize 系统语义承担，风险低。
2. **平台实测仅 Windows**（与 r1–r3 同边界）：macOS 的 `link(2)` 独占发布、路径规范化、git locale 行为未在 mac 实机验证；外部验收边界不因本轮通过消失。
3. **本轮为定向复审**：覆盖 r3 四项修复＋重写新边界＋全量自动化，未重新逐条人工执行 R11 验收 1–7；其余行为由 258 项测试与 r1–r3 已确认通过面承接。

## 待批准清单

**本轮请求批准**：H1/H2/L1/L2 关闭；观察项 2 处置确认；R11 实施通过复审，可进入后续合并/发布流程（发布仍需遵守既有打包规则）。

**本轮明确不批**：macOS 实机验证已完成；`rustfmt` 已验证；R11 需求、设计或规范语义变更；新增观察项（既有代码）的修复承诺。
