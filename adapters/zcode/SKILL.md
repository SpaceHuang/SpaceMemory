---
name: space-memory
description: Agent 第二记忆（SpaceMemory）使用规范。当会话开始/结束、需要跨会话记忆（任务/偏好/教训/决策）、需要增量刷新工作状态、或要写记录/改记录/提炼沉淀时使用。适用于本机已安装 mem（SpaceMemory 机制层）的环境。
---

# SpaceMemory 使用规范（L3 行为教学）

机制强制由 `mem` 二进制承担；本 Skill 只教判断型行为（R7-b）。约定的单一事实源在
`<store>/conventions/CONVENTIONS.md`——本文件只是指针 + 使用次序，规则冲突以 CONVENTIONS 为准。

## 会话生命周期（机械部分照做即可）

1. **会话开始**：执行 `mem session start --principal <你的 principal slug> [--task-ref <任务>] [--handoff auto]`。
   - 响应含 `session` id、`watermark`（事件序号水位）、`handoff_from`（接手来源）。
   - 首次使用先 `mem init <目录>`（自举 principal）。
2. **冷启动视图**：`mem brief`（纯读；无台账会如实提示）。分层：brief → list → show → trace。
3. **增量刷新**：`mem diff --since <watermark>`。
   - `changes_since`：已登记变更（按记录去重附变更史）；
   - `current_drift`：未登记变更（外部手改/删除），先 `mem reconcile` 回填再继续。
4. **会话结束**：`mem session flush --session <id> --trail <读过的记录id…>`（置 flushed 可接续）。

## 写入纪律

- **创建**：`mem add <type> --payload '<json>' --body <file>`。
  输者协议：同唯一键并发创建会返回既有记录 id + 令牌——收敛，不是错误。
- **读改写**：`mem update <id> --expect v<N>@<64hex> --payload '<json>'`。
  令牌从 show/list 响应原样带回（全摘要，禁止截断）。冲突（退出码 2）= 别人先写了：
  重读当前成果，把你的变更重放到新版本上。
- **唯一键字段不可变**（如 task 的 project+slug）；要移动走维护例程。
- **引用只填 ID**；过时记忆更新原记录（收敛不增殖）；正文（--body）是你的字节领土，
  机制永远不重写它。

## 提炼与证据（R9）

- 从多条实例沉淀规则/原则：建新记录 + `mem link <产物> <证据> --rel derived_from`；
  产物记录设置 `evidence_cutoff_seq`（证据截止序号）。
- 增量再提炼：`mem trace <产物> --dir up` 看证据链；未消化证据查询由维护例程提示。
- 重复提炼收敛到既有产物（update），不新增平行产物。

## 边界（不要做）

- 不绕过 mem 直接改 records/ 里的 frontmatter（正文人手可改，frontmatter 机制所有）；
- 不把约定全文拷进提示词——用指针（红线4）；
- 不在 brief 中期待台账创建（纯读；建立走 session start）；
- 只读存储：五个读命令照常，写命令会得到结构化"只读"错误——如实转告用户。
