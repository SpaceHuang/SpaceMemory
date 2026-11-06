# view 约定

## 载荷字段语义
- view 是**查询模板**，不是物理分区（R8.11 隔离靠视图）：filter 声明类型/引用/principal/时间四维约束。
- 引用一律填 uuid v7（D10）；不写名字。

## 生命周期
- 角色视角（如 codex-backend-reviewer）一条 view；多角色共用基础过滤时，宁可建两个 view 也不要一个 view 塞多种语义。

## 提炼与引用
- view 不参与派生链；它是读取侧声明。
