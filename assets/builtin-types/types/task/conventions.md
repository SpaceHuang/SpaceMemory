# task 约定

## 载荷字段语义
- `slug`：短标识，同 project 内唯一（机制按唯一键强制）。命名用小写连字符。
- `status`：`todo` → `in_progress` → `done|cancelled`；`blocked` 可从任意非终态进入。终态记录不再改载荷，只允许补正文（复盘）。
- `project`：归属项目；项目变更意味着移动语义（v1 拒收，走维护例程）。
- `due`/`priority`：可缺省；优先级 0（最高）–9。

## 生命周期
- 创建即 `todo`；不要为"想到的事"建 task（先记 memory，确认要做再建）。
- 完成时在正文尾部追加一行结论（做了什么、证据在哪），便于派生提炼。

## 提炼与引用
- `derived_from`：任务对教训/决策记录的引用（lesson/decision 类型出现后启用）。
- 引用只填 ID；名字解析交给别名层。
