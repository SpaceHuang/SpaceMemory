# ZCode 宿主适配（L2/L3 档）

## 约定注入（指针，红线4）

项目指令文件加入以下指针段（不拷贝约定全文）：

```markdown
## 第二记忆（SpaceMemory）
- 本机记忆库：<store 路径>（用户级；单副本，R8.11）
- 行为规范单一事实源：<store>/conventions/CONVENTIONS.md —— 行动前读取
- 使用次序与会话协议：见 space-memory Skill
```

## Hook 注入（L2 机械档）

- session start hook：
  `mem session start --principal <principal> --handoff auto`
  （stdout JSON 注入上下文：session id / watermark / 约定指针）
- session end hook：
  `mem session flush --session <id> --trail <ids…>`
- 工具响应 footer 提示（D14）：由 Skill 教学层要求 Agent 依从 mem 输出里的
  `next` 字段。

## MCP 配置（L4 完整档）

服务器键用 `spacememory`（仓库名——MCP 生态里 memory/mem0/mem.ai 等近名产品
众多，短名 "mem" 在同名覆盖规则下易被静默替换；协议层 serverInfo.name 标识
程序本体，不受此键影响）。

```json
{
  "mcpServers": {
    "spacememory": {
      "command": "mem",
      "args": ["mcp"],
      "env": { "MEM_STORE": "<store 路径>" }
    }
  }
}
```

- initialize 后先调 `mem_session_start`（principal 必填——防错报不防伪装，红线12）；
- 写工具必须携带 `expect` 全令牌（协议 schema 约束 `^v[0-9]+@[0-9a-f]{64}$`）；
- MCP 断连：降级为 CLI 命令面（同构工具面，§3.3）。
