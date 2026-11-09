# 类型声明两件套格式规范 v1.1（Type Declaration）

- 状态：v1.1（x-filter 词汇随 R10 实施落表，keyword-search-design.md §9 第 3 条；v1 = 设计文档 D19 的规范定稿）
- 上游：R7/R8（声明式引入、零知识、校验强制与分档、两件套）、R10（载荷字段过滤）、D9/D10/D16/D19
- 每种内容类型 = **两件套**：`types/<t>/schema.json`（(a) 机器可判定）+ `types/<t>/conventions.md`（(b) 判断型教学）。同源交付，分别承载 R7 的两类规则（R8.8）。
- **新增类型 = 新增两件套，基础设施零改动**（R8.1）；机制对 payload 结构零知识（红线1/R8.2）。

---

## 1. schema.json：JSON Schema draft 2020-12 + x-* 词汇表

`schema.json` = **JSON Schema draft 2020-12** 文档，校验对象 = 记录 frontmatter 的 `payload` object。自定义关键字（`x-*`）由 `mem` 的校验器机械执行；对标准校验器它们是普通注记，不参与 draft 校验语义。

### 1.1 x-* 关键字词汇表（机制认识并强制的全部）

| 关键字 | 位置 | 语义 | 强制点 | 归类 |
|---|---|---|---|---|
| `x-grade`: `"strict"` \| `"lenient"` | 根 | 校验分档：strict 坏字段拒收 + fix 通道；lenient 未知字段容忍 + 告警（R8.6） | mem-schema 校验器 | (a) |
| `x-unique-key`: `[字段名…]` | 根 | 自然键（作用域限定的引用字段 + 本记录字段）；规范路径推导依据（D11）；可为空 = 无自然键 | mem-core 路径独占 | (a) |
| `x-ref`: `{"target_type": "<t>", "existence": true}` | payload 属性 | 引用字段：目标类型 + 存在性校验（R8.5/红线12 同族）；字段值必须是该类型现存记录 ID | mem-schema × mem-core 记录表 | (a) |
| `x-dag`: `true` | 根 | 派生类引用强制无环（D16/红线17）：该类型实例经其派生 rel 边构成的图必须为 DAG；写路径检出环 → 结构化拒收 | mem-core 引用图 | (a) |
| `x-topics`: `[主题面…]` | 根 | 必挂主题面清单（开放问题33 边界；v1 仅声明 + 校验 payload 相应字段存在） | mem-schema | (a)/(b) 边界 |
| `x-filter`: `true` | payload 属性 | 声明该字段值可作清单与检索的过滤条件（R10 决策③）；取值形态 = 该属性本体的 schema 定义（`type` 关键字，不从 `enum` 推导） | mem-core 过滤执行器 | (a) |

规则：

- 词汇表属**基础设施**（机制认识这些关键字并机械执行）；**类型本身仍是纯数据**——新增类型零代码（红线13 不破：机制新增的是"认识关键字"这一信封级能力，载荷字段含义仍完全由声明提供）；
- `x-ref` 字段的 JSON Schema 类型约束必须与之一致（`"type": "string"`，uuid 形式）；不一致由 verify 报告；
- `x-dag: true` 的类型，其 `x-ref` 引用中语义为"派生自"的 rel（默认 `derived_from`，可被 schema 根级 `x-derived-rel` 覆盖）参与 DAG 检查；
- `x-grade` 缺省 = `lenient`（保守默认；strict 必须显式声明）；
- `x-filter` 点名的属性必须存在于 `properties`（点名缺失 = 声明自身坏写，加载即响亮失败，与 x-unique-key/x-ref 同族）；
- `x-filter` 的 **v1 支持形态** = `string` / `number` / `boolean` / 元素为以上标量的 `array`（任一元素等值即命中），形态从属性的 `type` 关键字读取（`type` 数组剔除 `null` 后恰剩一个可判定；`type: "integer"` 不在支持集）；支持形态之外的属性声明了 `x-filter` → verify 告警 `filter_shape_unsupported`，运行时对该字段过滤 → `invalid_filter_value`（声明不失败，前向容忍）；
- `x-filter` 的等值语义由声明形态承担：string → 折叠等值（NFC+小写，与检索共用折叠单源）；number → 过滤文本按 JSON 数值文法严格解析后数值等值（"1" 与 "1.0" 同值）；boolean → 仅认字面 `true`/`false`；array → 任一元素按其实际 JSON 形态等值即命中。规范过滤条件 = 字段＋标量文本（CLI `--filter 字段=值` 值原样、MCP `filters` 值一律 JSON 字符串），两端合法条件空间逐字相同（R10 决策④）。

### 1.2 根级约定形状（v1）

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "title": "task",
  "type": "object",
  "x-grade": "strict",
  "x-unique-key": ["project", "slug"],
  "x-dag": false,
  "x-topics": ["area"],
  "properties": { … },
  "required": [ … ]
}
```

- `properties` 中被 `x-unique-key`/`x-ref`/`x-topics`/`x-filter` 点名的字段必须在 `properties` 内有定义；
- 引用字段（x-ref）的值 = 目标记录 uuid v7 字符串——**引用只填 ID 不填名**（D11）。

## 2. 唯一键与路径推导（D19/D11）

- `x-unique-key = ["project","slug"]`：`project` 是 x-ref 字段（作用域）→ 路径成分取**目标 ID**；`slug` 是本记录字段 → 取折叠值；推导 = `records/task/<project-id>/<slug折叠>.md`；
- 唯一键字段值先 NFC，再 casefold 折叠（细则见 envelope-format.md §2.4）；
- 唯一键字段缺失/为 null → create 拒收（结构化错误，指明字段）；update 不得改动唯一键字段（改动 = 视为移动，v1 拒收，转 agenda 提案走显式迁移）；
- 无 `x-unique-key`（或空数组）→ 路径 = `records/<t>/<id>.md`。

## 3. conventions.md（(b) 件）

约定文档为 Markdown，建议结构（约定事实源总规范见 conventions-source.md）：

```markdown
# <type> 约定
## 载荷字段语义   （每个字段何时取什么值；引用什么）
## 生命周期       （状态机、何时建/合/删）
## 提炼与引用     （rel 命名空间的使用规则）
```

## 4. 演化容忍（R8.7）

- schema 变更后既有记录仍可读：按新 schema 校验失败 → **降级标注**（`degraded` 告警，字段路径列出），不拒绝服务、不隐式改写；
- 迁移 = 显式例程（`mem migrate` 类操作，走 agenda 计划 + 确认；v1 未内置，类型作者可发计划）；
- 删除类型目录 → 该类型存量记录读路径降级为"类型未声明"告警 + 信封级操作仍可用（跨类型操作只依赖信封，R8.4）。

## 5. 折叠实现细则（对 envelope-format §2.4 的实现注记）

- 规范语义：Unicode **full casefold**（NFC 后折叠）；
- 参考实现（Rust）：NFC（unicode-normalization）+ Unicode 简单小写映射（`str::to_lowercase`）。与 full casefold 的差异（如 `ß→ss`）不构成唯一性漏洞——唯一性由路径原子独占保证，折叠只影响"哪些键共享路径"；两键因折叠差异分到两条路径时，是两个合法记录（用户可见、可 merge）。

## 6. task 试金石（§8 第 2 步验收契约）

`task` 是第一个落地类型，验收判据：

1. 两件套入库后，`mem add task` 按 schema 校验 payload：缺 `slug`/`status` 拒收；`status` 取值域 `todo|in_progress|blocked|done|cancelled` 拒收越值；
2. `x-unique-key = ["project","slug"]`：同 project 同 slug 并发双 create 一胜一负，输者得既有记录 id + 令牌；不同 project 同 slug 各自成径；
3. `project` 字段 x-ref → target 必须是现存 project 记录；指向不存在 ID → 拒收；
4. 改 task 字段定义**只需改两件套文件，零 Rust 改动**（红线1 试金石）；
5. strict：坏 payload 拒收且可经 fix 通道修复；lenient 类型（memory）未知字段容忍 + 告警。

### 修订表

| 版本 | 日期 | 变更 |
|---|---|---|
| v1.0 | 2026-11-05 | 初版：x-* 词汇表、唯一键/路径推导、演化容忍、task 试金石契约 |
| v1.1 | 2026-11-09 | x-filter 词汇（R10 决策③④，keyword-search-design.md §2.4）：属性级过滤声明、v1 支持形态与等值语义、点名缺失加载响亮失败、形态不支持 verify 告警 `filter_shape_unsupported`；词汇表版本 1→2（机制关键字 6→7） |
