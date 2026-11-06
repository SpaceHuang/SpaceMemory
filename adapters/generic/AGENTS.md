# 通用宿主适配模板（L1/L2 档，任意 Agent 环境可用）

## L1 裸文件访问（底座，R8.10 最低档闭环）

没有 mem CLI 时的约定：

- 记忆库目录：`<store>`（单副本；迁移 = 复制目录）；
- 一条记录 = 一个 `.md` 文件：frontmatter（JSON：envelope+payload）+ Markdown 正文；
  **人手只改正文与 payload**，envelope 是机制所有的公共字段；
- 读：直接浏览 `records/<type>/…`；写：新增记录文件（id 用 uuid v7，version 从 1）；
  并发写同一记录请先用 `mem update --expect`（有 CLI 时）或人工确认；
- 约定：读 `<store>/conventions/CONVENTIONS.md` 并遵守（单一事实源，R7.4）。

## L2 约定注入模板

任意支持启动指令/CLAUDE.md/AGENTS.md 类机制的宿主，注入以下指针段（不拷贝全文）：

```markdown
## 第二记忆（SpaceMemory）
1. 会话开始：`mem session start --principal <slug> --task-ref <任务>` → 记下 session 与 watermark。
2. 读取：`mem brief` → `mem list --type <t>` → `mem show <id>`（令牌在响应里）。
3. 增量：`mem diff --since <watermark>`；有 current_drift 先 `mem reconcile`。
4. 写入：`mem add/update --expect v@…`；冲突先重读再重放。
5. 会话结束：`mem session flush --session <id> --trail <ids…>`。
6. 行为规则单一事实源：<store>/conventions/CONVENTIONS.md。
```

## 降级路径（R8.9）

| 缺失能力 | 降级行为 |
|---|---|
| 无 hook | 用上面的指针段 + Agent 自律执行 start/flush |
| 无 MCP | 全部走 CLI 命令面（§3.3 同构） |
| 无 mem CLI | L1 裸文件访问（闭环成立；并发保护与校验缺失，尽快安装 mem） |
| sidecar 不在 | 嵌入模式 = 直读（正确性不受影响，仅慢） |
| 存储只读 | 五个读命令可用；写命令结构化报错 |
