# SpaceMemory 约定（CONVENTIONS）

- version: 1

## 1. 通用纪律

1. **依赖前验证（R5.3）**：依据记忆行动前先验证现状；记忆承诺"写入时为真"，不承诺"此刻为真"。
2. **收敛不增殖（R5.4）**：同一事实的新认知 → 更新既有记录（携带修订令牌），不新建平行记录。
3. **引用只填 ID（D11）**：名字解析交给别名层；slug 只是键成分。
4. **更新带令牌（R3.2）**：读改写必须携带 `--expect v<N>@<64hex>`；冲突是提示不是失败——先读当前成果再重放你的变更。
5. **增量刷新（R4.3）**：会话恢复用 `mem diff --since <上次水位>`；`current_drift` 里的项说明有未登记变更，先回填（`mem reconcile`）再继续。
6. **预算内取用（R4）**：brief → list → show → trace 分层；截断标注是续读指令，不是错误。
7. **过时可见（R5.2）**：项目类信息超过存疑规则时限，行动前重验。

## 2. 类型角色段

（由 `mem init`/`mem reconcile` 从 types/<t>/conventions.md 机械拼装；修改类型约定请改两件套源文件。）

### task
见 types/task/conventions.md（播种时拼装）。

### memory
见 types/memory/conventions.md（播种时拼装）。

### project
见 types/project/conventions.md（播种时拼装）。

### topic
见 types/topic/conventions.md（播种时拼装）。

### principal
见 types/principal/conventions.md（播种时拼装）。

### view
见 types/view/conventions.md（播种时拼装）。

### edge
见 types/edge/conventions.md（播种时拼装）。

## 3. 角色段（role）

（D15：principal × view × 角色约定在会话建立时组合；需要时以类型声明引入角色类型。）
