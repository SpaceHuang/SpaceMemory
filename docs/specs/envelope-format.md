# 信封与目录格式规范 v1（Envelope & Store Format）

- 状态：v1（设计文档 D18/D19 的规范定稿；§8 第 1 步）
- 上游：`docs/requirement/agent-memory-requirements.md` v1.2、`docs/develop/agent-memory-design.md`（D3/D5/D11/D12/D13/D17/D18/D19，红线 6/10/18/19/20/22/24/26/27）
- 本文是**公开规范**（D8）：存储格式的单一事实源。实现与本规范冲突时，以规范评审结论为准。
- 记号：`记录状态三元组` = `(location ∈ {records, attic, absent}, version, digest)`；`修订令牌` = `v<版本>@<完整 SHA-256 十六进制>`。

---

## 1. 存储总纲

一条记忆库（下称"店"）= 一个目录。用户级单店单副本（R8.11）；迁移 = 复制目录（R2.3）；去掉 `.git` 后存储仍完整合法（红线10，增量契约不依赖 git）。

**唯一真相层**是文件系统上的人读文本；**索引（index.sqlite）是可丢弃的派生缓存**，任何机制不变量不得依赖它（红线8）。

### 1.1 目录布局（规范）

```
<store-root>/
├── mem.json                     # 库级配置（§6）
├── events.jsonl                 # 事件日志（§4；追加式，序号 = 行序）
├── barrier.lock                 # 门闩（运行时字节范围锁文件；内容无规范意义）
├── events.lock                  # 序列锁（同上）
├── .pending-commits/            # commit-pending 对账目录（§5.4）
├── .tmp/                        # 机制临时文件区（写例程 temp 文件；崩溃残留由 verify/对账清理）
├── conventions/CONVENTIONS.md   # 约定单一事实源（含角色段；另见 conventions-source.md）
├── types/<t>/schema.json        # 类型声明两件套之 (a)（见 type-declaration.md）
├── types/<t>/conventions.md     # 类型声明两件套之 (b)
├── records/<type>/<键推导路径>/<slug>.md    # 记录文件（§2）
├── attic/<records 相对路径>                 # 受管 tombstone（§3.3）
├── sessions/<session_id>.json   # 会话台账（§7；per-file 锁 <file>.lock）
├── agenda/<plan_id>.json        # 提案/计划队列（维护例程输出）
└── index/index.sqlite           # 派生索引（可丢弃；仅在新鲜性可证明时服务读）
```

- 运行时锁文件（`barrier.lock`/`events.lock`/各 `*.lock`）**无真相内容**，可随目录复制或删除而不影响语义（锁按需重建）。
- `.tmp/` 内文件无规范意义；正式文件只能经原子改名进入真相层（红线6）。
- `.git/` 可选存在：仅承担历史/审计/撤销/同步（D4）。**增量与一致性语义完全不读它**。

### 1.2 店级保留名（禁用清单）

以下名字**不得**用作任何类型 slug / 键路径成分（D19；命中即结构化拒收，R6.1）：

```
conventions  types  records  attic  sessions  agenda  index  events  tmp
.git  mem.json  events.jsonl  barrier.lock  events.lock
```

外加 Windows 保留设备名（`con`、`prn`、`aux`、`nul`、`com1…9`、`lpt1…9`，大小写不敏感）与空串。

---

## 2. 记录文件（D18）

### 2.1 物理格式

一条记录 = 一个 `.md` 文件，三段：

1. 第 1 行**恰为** `---`（U+002D ×3，行尾可有 `\r`，见 2.5 行尾）；
2. 至下一个**恰为** `---` 的行（不含）为 **frontmatter：必须是合法 JSON**（UTF-8，`serde_json` 可解析的单一 JSON object）；
3. 其余字节为 **Markdown 正文 = 载荷自由文本**（机制零解释）。

frontmatter 顶层**恰有两个键**（所有权字面可见，R2 审计）：

```json
{
  "envelope": { … 机制所有 … },
  "payload": { … 类型声明解释 … }
}
```

- `envelope` 缺失或类型错误 → 响亮错误（strict 拒收；见 2.6 分档）。
- frontmatter JSON 解析失败 → **响亮错误**，绝不静默（D18；fix 流见 2.7）。
- 正文可以为空文件（0 字节正文合法）。

### 2.2 envelope 字段（机制所有；R8.3）

| 字段 | 类型 | 规范 |
|---|---|---|
| `id` | string | 记录唯一身份，**uuid v7** 字符串（小写、连字符格式） |
| `type` | string | 类型标识 = `types/` 下目录名 |
| `created` | string | RFC 3339 UTC（`…Z`），创建时刻 |
| `modified` | string | RFC 3339 UTC，最近机制写时刻（人手改不改此字段——它描述机制最后一次落盘） |
| `version` | integer ≥ 1 | 并发版本；create = 1，此后每次机制写 +1 |
| `principal` | string \| null | 归属 principal 记录 ID；**存在性强制**（红线12）：非 null 时必须指向存在且 type=principal 的记录 |
| `refs` | array | 类型化引用，元素 `{"rel": string, "target": uuid}`；**只填 ID 不填名**（D11）；`rel` 命名空间由类型声明约束 |
| `aliases` | array of string | 别名，仅作用于查找解析层（D11）；非空元素须过 slug 字符集（§2.4） |
| `evidence_cutoff_seq` | integer \| null | 证据截止事件序号（R9.7）；null = 未声明 |
| `commit` | string \| null | 最近一次机制 git 提交 SHA；无 git / 未提交 = null |

未知 envelope 键：**保留**。机制遇到未知 envelope 键不丢弃（原样保留，保证再生成不丢失字节信息），但 verify 报告 `unknown_envelope_key` 告警。

### 2.3 payload（类型所有）

payload 仅由 `types/<type>/schema.json` 解释（R8.4）。机制对 payload 结构零知识（红线1/R8.2）：跨类型通用操作（列清单、过滤、排序、预算、级联清理）只读 envelope。载荷特有字段的自由正文写在 2.1 的 Markdown 正文里，机制不解析。

### 2.4 键字符集与路径推导（D19）

**规范化**：所有用户提供的键成分（slug、别名、唯一键标量字段）先 Unicode **NFC**。

**路径推导折叠**：推导路径时对每成分做 Unicode **casefold**（本规范 v1 以 full casefold 为准；参考实现以 Unicode 简单小写映射近似，见 type-declaration.md §5）——Windows 大小写不敏感卷上两键一径 → 原子独占失败（EEXIST）→ 同一记录；大小写敏感卷上折叠同样把两键收敛到同一路径。**唯一性从不依赖折叠的正确性**：它由路径原子独占保证（§3.2），折叠只决定"哪些键共享一条路径"。

**slug 允许字符**：Unicode 字母（`XID_Start` 及后续 `XID_Continue`）、数字、连字符 `-`；禁用：路径敌对字符 `<>:"/\|?*`、控制字符（U+0000–U+001F、U+007F）、首尾点/空格、纯 `.`/`..`。

**长度上限**：单个键成分（slug 或唯一键字段值折叠后）≤ 128 字节（UTF-8）。超限 = 结构化拒收。

**记录路径**：`records/<type>/<唯一键推导路径>/<slug>.md`：

- 类型声明 `x-unique-key = [f1, f2, …]` 时：推导路径 = 各键成分折叠值依次组成目录层（引用字段的成分取其目标 ID，不是名字）；最后一段为折叠 slug。
- 类型无 `x-unique-key` 时：推导路径 = `records/<type>/<uuid>.md`（身份即路径成分；类型无自然键，唯一性天然成立）。
- `.md` 后缀固定；正文不是 markdown 的类型也用它（正文是"自由文本"，markdown 只是建议形态）。

### 2.5 字节细节（确定性）

- 文件编码 UTF-8，无 BOM（有 BOM = 响亮错误）。
- 行尾：机制写出的文件用 `\n`；**机制再生成 frontmatter 时正文按字节保持**（红线27）——含人手写出的 `\r\n`，原样保留。
- frontmatter 分隔符行：`---\n`；机制序列化 frontmatter 后以 `---\n` 结尾，随后紧跟原正文字节。
- 机制写出的 frontmatter JSON：**确定性序列化**——顶层键序固定为 `envelope`、`payload`；envelope 内键按字母序（`aliases, commit, created, evidence_cutoff_seq, id, modified, principal, refs, type, version`）；对象键递归字母序；**数组（refs/aliases）保持写入顺序**；缩进 2 空格；无尾随空格；UTF-8 原文不转义（除 JSON 必需者）。
- 修订令牌摘要 = 整个文件字节（从第 1 字节到最后 1 字节）的 SHA-256，64 个小写十六进制字符。令牌不存储，读响应附带（D3）。

### 2.6 校验分档（R8.6）

- `x-grade = "strict"`：payload 不合 schema → **拒收**（写路径返回结构化错误，含字段路径）；已存在的坏记录读路径跳过 + 告警（R6.2），`mem fix` 提供修复通道（R6.3）。
- `x-grade = "lenient"`：未知字段容忍，降级服务 + 告警。
- frontmatter **JSON 语法**失败在两档下都是响亮错误（语法≠载荷）。

### 2.7 fix 流（R6.3）

frontmatter 损坏的记录：`mem fix <id|路径>` 交互重建——git 历史在时取上一版 frontmatter + 保留当前正文字节；无 git 时要求调用方提供 envelope/payload JSON。修复是显式操作，留事件（`op=fix`，kind=op）。

---

## 3. 生命周期与状态三元组

### 3.1 三元组（D12/B1）

任意时刻每条已知记录的状态 = `(location, version, digest)`：

- `records/<path>` 存在 → `location=records`，version/digest 取自信封与全文件摘要；
- `attic/<同相对路径>` 存在 → `location=attic`（受管 tombstone），version/digest 取自 tombstone 文件；
- 两处皆无且曾出现在事件日志 → `location=absent`，version=null，digest=null。**absent 是一等状态**，不是"无状态"。

### 3.2 创建：唯一键路径原子独占（R3.3/D11）

- create 无令牌；唯一性由**规范路径的原子创建**强制（record 路径上的创建例程以独占语义落盘；并发双 create 必然一胜一负）。
- **输者协议**：返回既有记录的 id 与当前修订令牌（不报错误性失败；D11"输者返回既有记录及令牌"）。
- 强制点在文件系统原子原语 + 记录锁，**不在索引**（红线8）。

### 3.3 受管删除与 attic（D17 删除语义）

- 机制删除 = **移动到 attic**（同相对路径），字节不变，落 `op=delete` 事件（事件后状态 = attic）；
- 派生关系默认阻止删除（force 时全量警示标注）；一般引用级联 + 断链注记（D17）；
- **恢复（restore）** = 从 attic 移回 records（同相对路径原子独占；若 records 侧已被重建 → 结构化冲突，转人工），落 `op=restore` 事件；
- **外部删除**（人手删文件）不是移动：`external_delete` 回填事件，事件后状态 = absent，`new_digest=null`。受管删除（attic）与外部删除（absent）是**不同的状态值**，恢复/续跑不误判。

---

## 4. 事件日志 events.jsonl（D12/红线24）

### 4.1 行格式

追加式，一行一事件，UTF-8 JSON object，键按字母序，`\n` 结尾：

```json
{"by":"<principal-id or label>","id":"<uuid>","kind":"op","new_digest":"<64hex|null>","new_v":6,"old_digest":"<64hex|null>","old_v":5,"op":"update","path":"records/task/0198…/login-api.md","seq":413,"ts":"2026-11-04T11:40:00Z"}
```

| 字段 | 规范 |
|---|---|
| `seq` | 全库序号，**= 行序**（首行 seq=1），由序列锁唯一分配（叶子锁、全库串行；每记录锁不得分配序号） |
| `ts` | RFC 3339 UTC |
| `id` | 记录 ID（uuid v7） |
| `path` | 记录相对路径（`records/…` 或 `attic/…`）——**ID↔路径映射**，支撑已删除/移动记录的定位（B1 前提） |
| `op` | `create | update | link | delete | restore | external_delete | backfill | fix | merge` |
| `old_v`/`new_v` | 操作前/后版本；absent 侧为 `null` |
| `old_digest`/`new_digest` | 操作前/后全文件 SHA-256；absent 侧为 `null` |
| `by` | 写入者归属（principal ID + 宿主标签，如 `"0198…(zcode_main)"`；机制回填为 `"mem"`） |
| `kind` | `op`（普通写）| `backfill`（回填/对账补登） |

### 4.2 分配与提交纪律

- **唯一分配点**：内容写入时刻、序列锁内；`commit-pending` **不分配**序号（提交滞后合法，§5.4）；
- **事件随写例程同批提交**：`git commit -- <触及的记录路径…> events.jsonl`（D3 步骤7/高优1）；用户外部 `git add -A` 卷入事件日志无害；
- **回填判据 = 状态覆盖比较**：最后事件"事件后状态"三元组 ≠ 锁内重查的当前三元组 → 回填；`last == cur`（含 `absent == absent`）→ 无动作（稳定态，同一次删除只回填一次）；
- **回填并发纪律**：共享门闩 + 该记录规范路径锁（**锁文件独立于记录文件存在**——absent/待创建同样可锁）→ 锁内重查 records/ 与 attic/ 复算 cur → 序列锁内复核最后事件 → `last != cur` 才追加；**禁止锁外旧快照直接追加**；
- **链连续性**：非 backfill 事件按记录组成版本链，`old→new` 断裂 → verify 显式报告 `history_incomplete`，**不伪造**中间事件；
- **撕裂尾行**：末行 JSON 不可解析（崩溃半行**或**手改坏行，无论是否带换行）→ 读取时忽略并告警；**追加例程先把文件截断到最后一个完整合法行界（`good_bytes`），再写新行**——坏行被整体移除，对应记录由回填覆盖。中部坏行 = 结构损坏，响亮错误（R6.1），不做自动修复。

### 4.3 枚举域（B1/红线24）

回填与漂移的枚举域 = **当前记录集合（records/ ∪ attic/ 枚举）∪ 事件日志历史集合**（events.jsonl 出现过的全部 `(id, path)`）。仅枚举现存文件会漏外部删除/重命名。

---

## 5. 并发协议与写屏障（D3/D17/红线20）

### 5.1 锁序（全库统一，违序 = 实现缺陷）

```
门闩 barrier.lock（共享=普通写贯穿全程；独占=维护/merge/apply）
  → 记录锁 records/<相对路径>.lock（可多把；缺失记录同规范路径锁）
    → 序列锁 events.lock（叶子；唯一序号分配点）
```

读路径不获取门闩（零写盘、不受维护阻塞）；**维护屏障必须持有而非探测**（单次 check-then-act 是竞态，红线20）。

### 5.2 锁文件与陈旧锁判定（红线22）

- 记录锁文件 = `<记录相对路径>.lock`（含 records/ 与 attic/ 侧同一映射路径）；内容 JSON：`{"pid":<int>,"host":"<hostname>","ts":"<RFC3339>"}`；
- 陈旧判定：**PID 死亡即抢、存活永不抢、超时只许等待方放弃**（等待超时返回结构化 `lock_timeout`，含持有者信息）；
- 门闩/序列锁文件内容无规范意义（字节范围锁按句柄持有）。

### 5.3 写路径（单记录，D3 九步）

1. 共享门闩（贯穿全程）→ 2. 记录锁 → 3. 全摘要令牌 CAS（版本**与**摘要；不一致 → 结构化冲突，含当前令牌，不落盘）→ 4. 类型声明校验（strict 拒收 / lenient 告警）→ 5. temp + 原子改名落盘（信封 version = 基线+1）→ 6. 序列锁内追加事件 → 7. `git commit -- <该记录路径> events.jsonl`（可重试资源）→ 8. 释放记录锁；写路径顺带登记台账 trail → 9. 释放共享门闩。

### 5.4 git 提交语义（D17）

- `index.lock` 冲突可重试（指数退避，默认 ≤ 3 次）；耗尽 → **commit-pending**：事件已分配序号并落盘，git 提交延后——登记 `.pending-commits/<seq>.json`（含路径清单），后续对账（`mem reconcile`/写例程）重试；
- 提交滞后合法；**提交失败不回滚真相层**（文件与事件已原子生效，git 只是历史）；
- 线性 main；机制不建分支（红线7）。

### 5.5 检查点发布收敛纪律（r4 B3/B2）

索引物化与检查点发布必须经由**统一收敛原语**，各前端（sidecar tick/握手、CLI reconcile、MCP reconcile）不得各自实现顺序：

1. **维护**：物化索引（rebuild + FTS），索引 `meta.epoch` 递增（物化代际）；
2. **新鲜查询**：对变更日志订阅做一次**咨询时刻的查询**（不是轮询缓存值）——为 0 → 收敛；>0 → 回到 1（吸收维护期间的二次写入，有界 4 轮）；订阅失效/覆盖缺口 → **不发布**；
3. **发布**：收敛后检查点才可发布，且必须携带 `index_epoch`（发布时的索引代际）；
4. **读路径核对**：检查点 `index_epoch` == 索引当前 epoch 才可走快路径；索引缺失/schema 不完整/代际不匹配 → 直读（读路径**只读**打开索引，不建库不建表——红线18）；
5. 无订阅证据（如脱离 sidecar 的 reconcile）→ 只维护、**不发布检查点**——无证据不得签发证明（红线21）。

---

## 6. 库级配置 mem.json

```json
{
  "format_version": 1,
  "created": "2026-11-05T00:00:00Z",
  "budgets": {
    "brief_items": 30,
    "brief_bytes": 16384,
    "list_limit": 50,
    "list_limit_max": 200,
    "trace_depth": 4,
    "trace_nodes": 200,
    "diff_since_limit": 200,
    "current_drift_limit": 100
  },
  "doubt_rules": [],
  "session_idle_secs": 86400,
  "subscription": { "supported": false, "backend": null },
  "init": { "template_dir": null }
}
```

- `budgets`：开放问题25 的 v1 默认值；`list_limit_max` 是任何调用可请求的上限；
- `session_idle_secs`：台账空闲阈值（开放问题37 默认值）；
- `subscription`：能力声明（D5/开放问题39）：`backend ∈ null|"usn"|"rdc"|"fsevents"`（rdc = ReadDirectoryChangesW，非提权可用；fsevents 预留 macOS，v1 未接）；`supported=false` → 永久直读形态（正确性不依赖订阅）；
- **`init.template_dir`**（开放问题40 定名）：`mem init` 的外部播种资产目录（exe 旁 `builtin-types/` 优先，其次本键指定路径，最后内嵌字节兜底）——git `init.templateDir` 的对应物；
- 未知键保留；`format_version` 升级是显式迁移例程（R8.7）。

---

## 7. 会话台账 sessions/（D13/红线25）

- 文件：`sessions/<session_id>.json`，机制所有，per-file 锁；session_id = `<本地日期>_<8位随机hex>`，如 `session_20261104_b7`；
- 字段：`principal`、`view`（view 记录 ID 或 null）、`task_ref`（string|null）、`conventions_version`、`watermark`（事件序号水位）、`trail`（id 数组，写入通道）、`read_trail`（flush 提交的读通道增量）、`handoff_from`（string|null）、`status`（`active|flushed|closed`）、`created`/`last_write`（RFC 3339）；
- **创建触发器没有一个是读命令**：MCP initialize / `mem session start` 显式建立；写路径兜底（无台账时建）；**brief 纯读，永不创建或修改台账**（红线25）；
- **幂等键 = principal + view + task_ref**：同键的活跃台账 → start 返回既有台账（幂等）；
- **接手选择作用域 = view + task_ref**（优先序：显式 id > 同 view 且 task_ref 相等 > 同 view 且 task_ref 为空 > **task_ref 不同永不接手**）；可接续 = `flushed/closed`，或 `active` 且 `last_write` 超空闲阈值；活跃会话不可自动接手（peers 摘要呈现）；接手 = 新台账 + 落盘 `handoff_from` + 继承水位；
- trail 双通道：写通道随写路径登记；读通道由客户端累计、`session flush --trail …` 显式提交；只读存储 flush 失败并标注。

---

## 8. 漂移与增量（D12/B3/红线26）

`mem diff --since N` 输出两部分，**语义分开呈现**：

- **A. `changes_since_N`**：`seq > N` 的事件按记录去重（附变更史）；受分页预算 `diff_since_limit`；
- **B. `current_drift`**：内存状态比较（枚举域 = §4.3 并集）——当前三元组 ≠ 最后事件三元组者；`absent` 已登记 → 不报告（稳定态）；**不受游标过滤**、独立命名、独立预算 `current_drift_limit`，不混入 A；每项附当前令牌（absent 项附最后已知 path）与 mtime（信息性）；标注 `backfill 待执行`；
- 与 git 完全无关；只读存储 B 项标注"只读无法登记，将持续出现直至可写回填"。

---

## 9. 版本与演化

- 本规范版本号：`format_version = 1`。向后兼容的扩展（新增可选字段/新 op）升 minor 版本记录于本文件修订表；
- 类型声明格式规范见 [type-declaration.md](type-declaration.md)；约定事实源见 [conventions-source.md](conventions-source.md)。

### 修订表

| 版本 | 日期 | 变更 |
|---|---|---|
| v1.0 | 2026-11-05 | 初版：D18/D19 落规范；收口开放问题40（`init.template_dir`）、问题25（默认预算）、问题37（默认空闲阈值） |
| v1.0.2 | 2026-11-05 | §5 检查点发布收敛纪律（实施评审 r4 B3/B2）：维护→新鲜查询→收敛才发布；index_epoch 代际核对；读路径只读打开索引不建库 |
| v1.0.1 | 2026-11-05 | §4.2 撕裂尾行措辞修正（实施评审 B1）："补 
" 表述与"视为截断"自相矛盾，统一为"截断到最后完整合法行界"；删除"先补 
"表述 |
