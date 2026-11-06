# 约定文档单一事实源结构规范 v1（Conventions Source）

- 状态：v1（§8 第 1–3 步配套；R7.4 落规范）
- 上游：R7（强制/教学分离）、D6/D14/D15

## 1. 单一事实源

`conventions/CONVENTIONS.md` 是全部判断型行为规则（R7-b）的**唯一事实源**。规则不得散落：任何宿主适配、Skill、提示词若需行为规则，**引用此处**，不拷贝全文（红线4：hook 机械注入或指针，不用全文拷贝）。

## 2. 结构（v1 规范）

```markdown
# SpaceMemory 约定（CONVENTIONS）
- version: <int>            ← 机制维护（台账 conventions_version 记的是它）
## 1. 通用纪律              ← 所有角色遵守（R5.3 依赖前验证、R5.4 收敛不增殖…）
## 2. 类型角色段             ← 每种类型一节，与 types/<t>/conventions.md 同源：正文即该文件内容
## 3. 角色段（role）          ← principal×view 组合的行为差异（D15：会话建立时组合，暂不实体化）
```

- **类型角色段同源**：`mem init`/`mem reconcile` 将 `types/<t>/conventions.md` 内容并入 CONVENTIONS.md 第 2 节对应小节（机械拼装，标明来源与版本）；用户改 CONVENTIONS.md 第 2 节会被对账覆盖——类型约定改两件套文件，店级约定改第 1/3 节；
- `version` 由机制在拼装时递增；台账记录 `conventions_version`，会话建立时注入的约定以此版本为准（可检测"会话用的约定已过期"）。

## 3. 多级下发（R7.4）

| 宿主能力 | 下发方式 |
|---|---|
| L4 MCP/CLI + hook | `mem session start` 响应携带约定（按角色段裁剪） |
| L2 约定注入 | start hook 脚本机械执行同上 |
| L1 裸访问 | 指针文件/AGENTS.md 写明"读取 `<store>/conventions/CONVENTIONS.md`" |

## 4. 首条 principal 自举（D9/红线12/13）

`mem init`：播种两件套（含 principal/view）→ 以类型声明引入首条 principal 记录（payload 存宿主标签与描述）→ 自举试金石：principal/view 记录与普通记录同走信封 + 类型声明路径，机制代码不含其字段结构。

### 修订表

| 版本 | 日期 | 变更 |
|---|---|---|
| v1.0 | 2026-11-05 | 初版 |
