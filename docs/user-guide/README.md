# SpaceMemory 使用指南

本指南面向使用 SpaceMemory 保存和取用 Agent 长期记忆的个人用户。它从安装、建库、日常读取与写入讲起，示例可直接复制后按你的路径和内容调整。指南描述的是当前仓库已经实现的 `mem` 功能；它不要求读者先阅读设计文档或规范。

## SpaceMemory 是什么

SpaceMemory 把长期记忆放在你自己控制的本地目录中。每条记录包含结构化字段和可选正文，`mem` 命令负责安全地创建、更新、查询与维护这些记录。数据目录是唯一真相；索引只是可重建的查询加速数据。记录和事件日志可以随目录一起备份或迁移。

内置记录类型包括：

- `principal`：Agent 或执行者身份；初始化时自动建立一条。
- `project`：项目。
- `task`：任务状态。
- `topic`：主题分类。
- `memory`：一般记忆，可带标题、主题和显著性。
- `view`：可复用的查询视图。
- `edge`：带属性的关系记录。

类型的具体字段由建库时播种的类型声明校验。自定义类型可放在记忆库的 `types/` 中；本指南的示例使用内置类型。

## 1. 安装并选择数据目录

目前可在 Windows 或 macOS 使用。GitHub Releases 提供 Windows zip 和 Apple Silicon macOS tar.gz；仓库也提供源码构建。macOS tar.gz 解压后，`mem` 可执行文件与 `builtin-types/` 目录位于同一文件夹。使用发布物时请保留整个 `builtin-types/` 目录，不要只复制可执行文件。

从源码构建需要 Rust 工具链和 Git：

```sh
cargo build --release
```

构建出的程序位于 `target/release/mem`（Windows 为 `target/release/mem.exe`）。将它放入 `PATH`，或在下面的命令中使用完整路径。

记忆库可以放在任意可写位置。建议选择一个稳定、会纳入备份的位置：

```sh
mem init "$HOME/AgentMemory" --principal main
```

Windows PowerShell 示例：

```powershell
mem init "$env:USERPROFILE\AgentMemory" --principal main
```

初始化会创建目录、播种内置类型、写入首条 `principal` 记录，并在可用时初始化该目录自己的 Git 历史。已有记忆库不能再次用 `init` 覆盖。

之后有三种指定数据目录的方式，优先级从高到低为：

1. 命令参数 `--store <目录>`；
2. 环境变量 `MEM_STORE`；
3. 默认目录：`~/.agent-memory`（Windows 使用用户主目录下的 `.agent-memory`）。

例如：

```sh
mem --store "$HOME/AgentMemory" brief
```

下文省略 `--store`。若没有设置 `MEM_STORE`，请按需给每条命令加上 `--store <目录>`。

## 2. 每次工作开始时读取记忆

先建立会话台账，再读取工作视图：

```sh
mem session start --principal main --task-ref "space-memory:整理发布说明" --handoff auto
mem brief
```

`session start` 输出 JSON，其中 `session` 是本次会话 ID，`watermark` 是事件日志水位，`handoff_from` 表示接手的旧会话。首次使用时 `--principal` 应使用初始化时的 slug（上例为 `main`）。如果暂时不需要接手，可省略 `--handoff auto`。

`brief` 是轻量冷启动摘要。默认展示最近记录或已绑定的默认视图，并受条目数和字节预算限制；它不会建立会话或修改数据。若输出含 `resume.offset`，可用该偏移继续读取：

```sh
mem brief --offset 30
```

再按需逐层查询：

```sh
mem list --type task --limit 50
mem show <记录ID>
mem trace <记录ID> --dir up --depth 3
```

`list` 可按类型筛选；`show` 展示一条记录的字段和正文；`trace` 沿关系查看关联记录。读取结果中的 `token` 是该记录当前修订令牌，更新时必须完整带回。

## 3. 查找记录

`find` 支持空格分隔的多个正向词项（必须全部匹配）以及以半角 `-` 开头的排除词项。匹配范围包括记录 ID 前缀、别名、载荷中的文本值和合法 UTF-8 正文；字段名本身不是匹配范围。

```sh
mem find "发布 安装"
mem find "发布 -旧版"
```

也可用 `--filter 字段=值` 对声明了 `x-filter` 的载荷字段做精确文本筛选。多条过滤条件按合取执行。当前内置类型尚未声明 `x-filter` 字段，因此这些字段过滤参数暂不适用于内置类型；自定义类型只有在类型声明显式开放过滤后才能使用。过滤值按文本比较；例如数值字段也要写成文本。

空查询或只有排除词的查询会报参数错误。结果可能分页，若返回 `resume.offset`，将其传给同一命令的 `--offset` 继续读取。

## 4. 创建和更新记忆

### 创建一条一般记忆

```sh
mem add memory \
  --payload '{"title":"发布包必须包含 builtin-types 目录","salience":0.8}' \
  --body "发布或分发 mem 时，将 builtin-types/ 与可执行文件一起交付。"
```

正文可以省略，或从文件读取；用 `-` 表示从标准输入读取：

```sh
mem add memory --payload '{"title":"会议结论"}' --body ./notes.md
cat ./notes.md | mem add memory --payload '{"title":"会议结论"}' --body -
```

输出中的 `id` 是后续读取和关联使用的记录 ID，`token` 是后续更新需要的修订令牌。重复创建有相同唯一键的记录类型时会按其类型规则收敛或报告冲突；不要依赖重复创建来修改既有内容。

### 创建项目和任务

```sh
mem add project --payload '{"slug":"space-memory","title":"SpaceMemory","status":"active"}'
```

项目输出会给出 `id`。可将它填入任务的 `project` 字段：

```sh
mem add task --payload '{"slug":"write-guide","title":"编写用户指南","status":"in_progress","project":"<项目ID>"}'
```

`project` 的 ID 必须替换成真实存在的项目记录 ID。任务状态可用 `todo`、`in_progress`、`blocked`、`done` 或 `cancelled`。

### 更新已有记录

先读取当前令牌：

```sh
mem show <记录ID>
```

然后把完整令牌原样传给 `--expect`，并提供更新后的完整 payload：

```sh
mem update <记录ID> \
  --expect 'v1@<64位摘要>' \
  --payload '{"title":"发布包应包含 builtin-types/ 目录","salience":0.9}'
```

省略 `--body-file` 时原正文保持不变；若要替换正文，传入 `--body-file <路径>`。`update` 要求显式提供完整 payload，因此先从 `show` 复制现有字段，再编辑需要变更的字段。

如果命令以退出码 `2` 返回冲突，说明记录在你读取后已被其他写入者更新。重新执行 `show`，基于新内容合并你的修改，再用新令牌重试。不要截短或手工拼造令牌。

## 5. 正文与二进制内容

文本正文可直接通过 `show` 查看。较长正文会按预算分页；按响应中的 `resume.body_offset` 继续：

```sh
mem show <记录ID> --body-offset <偏移>
```

图片、压缩包或非 UTF-8 正文不会经过文本解码。要按原始字节取回，请指定一个尚不存在、且位于记忆库目录之外的目标路径：

```sh
mem show <记录ID> --body-out ./exported-content.bin
```

命令会拒绝覆盖已有文件，也会拒绝写入记忆库内部。响应包含导出路径、字节数和 SHA-256 摘要，可用于核对导出结果。

## 6. 关联、差异和会话结束

建立两条记录间的引用时，先分别 `show` 取得双方当前令牌：

```sh
mem link <记录A> <记录B> --rel related_to \
  --expect-a '<记录A令牌>' --expect-b '<记录B令牌>'
```

会话开始响应提供水位。需要查看该水位之后的已登记变更时：

```sh
mem diff --since <watermark>
```

输出分为 `changes_since`（事件日志中的变更）和 `current_drift`（尚未登记的外部修改或删除）。如有漂移，先运行：

```sh
mem reconcile
```

再重新查看差异。结束会话时记录读过的条目，便于后续接手：

```sh
mem session flush --session <会话ID> --trail <记录ID> --trail <另一记录ID>
```

台账只用于会话接续和读留痕，不代替记录本身。`brief` 不会自动创建台账。

## 7. 删除、恢复和健康检查

删除需要当前令牌，并会将记录移入受管的删除区（attic）：

```sh
mem show <记录ID>
mem delete <记录ID> --expect '<当前令牌>'
```

被其他记录引用时，删除默认会阻止操作。`--force` 会越过引用保护并给出警告，请在确认影响后才使用。可恢复删除的记录：

```sh
mem restore <记录ID>
```

检查记忆库结构和引用：

```sh
mem verify
mem verify --deep
```

`verify` 会报告损坏记录、事件历史空洞、悬空引用、载荷与引用镜像不一致及计划问题；`--deep` 还会比较记录摘要与事件历史。修复需根据报告处理，不要直接编辑记录文件的结构头。

`mem agenda --scan` 可扫描并生成维护提案；`mem apply <计划ID>` 会逐项检查修订令牌后执行计划。只在理解计划内容和影响后应用。

## 8. 文件、备份和只读访问

记忆库中主要目录如下：

- `records/`：当前记录，是数据主体；
- `attic/`：受管删除记录；
- `sessions/`：会话台账；
- `events.jsonl`：变更事件日志；
- `types/`：当前库使用的类型声明；
- `conventions/CONVENTIONS.md`：该库约定；
- `index/`：可重建的派生索引；
- `mem.json`：库配置与查询预算。

备份或迁移时复制整个记忆库目录，不要只复制 `records/`。索引可重建，但其余状态共同构成完整库。`mem init` 建立的库包含 Git 历史；若该库位于 Git 工作区且你自行同步它，请确保同步的是整个目录并避免多端同时改写同一记录。

如果目录不可写，`brief`、`list`、`find`、`show`、`trace`、`diff` 等读取命令仍可使用；写入和维护命令会拒绝执行。只读状态不会自动修复权限或更改文件。

机制读写结构化记录时拥有 frontmatter（记录头），用户正文仍按原字节保留。请通过 `mem add/update` 修改载荷；直接改记录文件的头部可能造成令牌冲突或记录损坏。手工编辑正文后，运行 `mem reconcile` 让变更进入事件日志。

## 9. MCP 和 sidecar

`mem mcp` 可作为 stdio MCP server 供支持 MCP 的宿主调用；宿主需将它配置为 MCP 子进程，并将记忆库目录设为 `MEM_STORE`。未设置时使用默认目录 `~/.agent-memory`。MCP 握手本身不会猜测身份或建立会话；宿主应先调用会话建立工具并明确提供 principal。写操作同样要求完整修订令牌。

`mem serve --store <目录>` 可启动 sidecar，维护索引并协助证明索引新鲜性；它不是必需的常驻服务。sidecar 不可用时，读取仍可退回直接扫描记录文件，结果正确但可能较慢。

## 常用命令速查

| 目标 | 命令 |
|---|---|
| 初始化记忆库 | `mem init <目录> --principal main` |
| 建立/接手会话 | `mem session start --principal main --handoff auto` |
| 冷启动摘要 | `mem brief` |
| 按类型列记录 | `mem list --type task` |
| 搜索 | `mem find "正词 -排除词"` |
| 查看记录 | `mem show <ID>` |
| 创建记忆 | `mem add memory --payload '<JSON>'` |
| 更新记录 | `mem update <ID> --expect '<令牌>' --payload '<完整JSON>'` |
| 查看增量 | `mem diff --since <水位>` |
| 回填外部变更 | `mem reconcile` |
| 结束会话 | `mem session flush --session <ID> --trail <ID>` |
| 检查数据 | `mem verify --deep` |

所有命令的完整参数可通过 `mem --help` 和 `mem <命令> --help` 查看。命令输出为 JSON，错误写到标准错误；退出码 `2` 表示修订令牌冲突，其余执行错误为非零退出码。
