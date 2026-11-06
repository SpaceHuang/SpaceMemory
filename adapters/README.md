# 宿主适配层（每宿主一份薄壳）

设计文档 §3.4：宿主集成能力分四档渐进增强，**任一高档缺失都不破坏闭环**；价值锚定在数据本身，不同宿主访问同一份状态（R8.11）。

| 档 | 能力 | 降级行为 |
|---|---|---|
| L1 原始数据访问（底座） | 裸读目录 | 无台账/身份块/hook；约定直读；闭环成立（R8.10） |
| L2 约定注入 | hook 机械注入（start --handoff + brief；end flush）+ 行为指针 | 无 hook 降指针；台账退化为写路径兜底 |
| L3 行为教学 | Skill / instructions 包装 | 同 L2 |
| L4 完整工具协议 | CLI + MCP（initialize 建/接手台账；schema 携带 expect 全令牌） | MCP 断连降 CLI；sidecar 不在降嵌入模式 |

跨档：只读存储下五个读命令全部可用（零写盘）；写类操作结构化错误。

## 原则

- 约定注入用 hook 机械注入或指针，不用全文拷贝（红线4）；
- 单一事实源：约定住数据目录 `conventions/CONVENTIONS.md`（R7.4），适配层只做注入与指针；
- 台账接手走 `mem session start --handoff`（D13/红线25），不依赖宿主传参。

## 规划子目录

- `zcode/`：ZCode 宿主（hook 注入 + Skill/AGENTS.md 模板 + MCP 配置）；
- `generic/`：通用模板（L1/L2：裸文件访问约定 + AGENTS.md 注入模板，任意宿主可用）。
