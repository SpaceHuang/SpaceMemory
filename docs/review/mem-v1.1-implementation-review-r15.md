# 当前项目代码评审——第十五轮（macOS 与跨记录边界）

- 日期：2026-11-11
- 基线：`main`，HEAD `2cc5ac2`（版本 0.1.1）；开始评审时工作区干净。
- 环境：macOS / Darwin arm64，`rustc 1.97.1 (8bab26f4f 2026-07-14)`。
- 方法：按 `code-review-and-quality` 的正确性、可读性、架构、安全、性能五轴，先检查测试与需求，再检查实现；独立交叉审查核心写路径与检索路径。
- 范围：当前工作区实现，不限于最近提交。重点检查锁、删除恢复、事件回填、路径解析、索引检索及 CLI/MCP 装配；本报告不声称逐行穷尽全库或完成 Windows 实机复验。

## 结论与待批准清单

**Request changes：确认 8 项 P1 阻断，另有 1 项 P2 必改问题。当前不能据此批准发布。**

原始 HEAD 在 macOS 上无法编译；绕过编译问题后的隔离探针还复现了删除副本丢失、锁提前失效、日志丢事件、事件身份混淆、目录越界写入、索引漏召回与非 force 删除产生悬空引用。它们涉及正常受管操作、崩溃恢复或接口边界，不是代码风格偏好。

| 编号 | 优先级 | 用户可见后果 | 证据 |
|---|---|---|---|
| B15-1 | P1 | macOS 目标无法构建，测试不能执行 | 原始 HEAD 的 check/test 均退出 101 |
| B15-2 | P1 / Critical | 同键记录删除、重建、再删除，旧删除副本被覆盖 | 真实 CLI + 无 Git 核心探针 |
| B15-3 | P1 / Critical | guard 仍存活时，外部维护或写入者能取得冲突锁 | macOS 真实子进程争锁 |
| B15-4 | P1 / Critical | 日志尾部缺换行后再追加，会把已接受的事件变成坏行并丢失 | 真实 EventLog 探针 |
| B15-5 | P1 | 对账把新记录的内容摘要登记到旧记录 ID 下 | 真实 CLI reconcile + 核心探针 |
| B15-6 | P1 | `id` 路径可越出店目录读取、修改另一文件 | 真实 CLI show/update |
| B15-7 | P1 | 同一查询直读命中，索引模式返回空结果 | 真实双执行器对账探针 |
| B15-8 | P1 | 非 force 删除与新增引用并发，成功后留下悬空引用且无警告 | 真实核心并发探针 |
| Q15-1 | P2 / Required | 只读打开索引仍创建 WAL/SHM 缓存文件 | 真实 mem-index 探针 |

**本轮请求确认**：以上问题按文内关闭条件处理后重新评审；本轮没有代替实施者修改代码。

**本轮明确不批**：设计语义变更、历史评审结论改写、发布或合并操作，以及把临时副本验证等同于原始 HEAD 全量通过。

## 验证口径——原始 HEAD 与隔离探针分开记账

| 验证 | 结果 |
|---|---|
| 原始 HEAD：`cargo check --workspace --all-targets` | **失败**，5 个编译错误；另有 unused variable 警告 |
| 原始 HEAD：`cargo test --workspace --no-fail-fast` | **失败**，同样被编译错误阻断，未进入工作区测试执行 |
| 原始 HEAD：`cargo test -p mem-schema -p mem-assets` | **通过：20 项测试，0 失败**；doc-tests 无测试 |
| 隔离副本：核心锁、删除、回填、并发删除、日志尾行探针 | 复现 B15-2/3/4/5/8 |
| 隔离副本：实际 `mem` CLI | 复现 B15-2/5/6 |
| 隔离副本：NUL 三域 direct/indexed 一致性断言 | **失败：direct=3，indexed=0**，即 B15-7 的复现证据 |
| 隔离副本：只读打开前后目录不变断言 | **失败：新增 -wal/-shm**，即 Q15-1 的复现证据 |

为在构建阻断之外继续检查业务逻辑，探针在系统临时目录复制源码，只处理 B15-1 的编译前置：补 macOS `LOCK_OFFSET = 1 << 20`，并让非 Windows 的三个空枚举 match 可编译。一份副本用非 Windows 不可达分支，另一份用对空枚举值匹配；二者都不会建立 macOS 订阅。被测锁生命周期、记录操作、事件日志、SQL 和路径解析没有修改。

因此，下文运行证据是“原业务实现 + 明示的编译前置绕过”，不是原始 HEAD 的生产构建通过证明。索引探针另外模拟了合法的新鲜性 Hello，明确断言 `open_proved` 返回索引句柄；它验证真实物化、粗筛、精筛链路，不声称在 macOS 建立了 Windows RDC 订阅。

## B15-1 [P1] 修复 macOS 的编译分支

**位置**：`crates/mem-core/src/platform/macos.rs:55,71`；`crates/mem-core/src/sidecar.rs:213-254`。

原始 HEAD 两次命令均给出：

```text
E0425 macos.rs:55,71: cannot find value LOCK_OFFSET in module super
E0004 sidecar.rs:220: non-exhaustive patterns for &Subscription
E0004 sidecar.rs:228,246: non-exhaustive patterns for &mut Subscription
error: could not compile mem-core due to 5 previous errors
```

偏移常量只定义在 Windows 模块，macOS 却引用父模块中的同名值。`Subscription` 的唯一变体也只在 Windows 编译，其他平台仍对其引用作没有分支的 match，不能通过穷尽检查。

这直接阻断目标平台上的安装、运行与测试；CI 的 `macos-latest` 测试矩阵也不能完成。

**关闭条件**：在正确的平台边界提供锁偏移并修复无订阅分支，保留无法证明新鲜性时直读的行为；原始源码在 macOS 上 check 零警告、完整 test 通过。临时探针的绕过不算关闭。

## B15-2 [P1 / Critical] 删除时不得覆盖既有不同身份的 tombstone

**位置**：`crates/mem-core/src/ops.rs:648-652`；merge 同类操作在 `:1005-1011`。

删除将记录移入 `attic/`，目的是保留可恢复副本。当前目标路径仅由原记录路径推导，macOS 的 `rename` 会替换已存在的目标文件。

**最小复现**：

1. 创建 `topic`，载荷为 `{"slug":"reused","title":"A"}`，得到 ID A。
2. 删除 A，产生 `attic/topic/reused.md`。
3. 用同一 slug 创建标题为 B 的新记录，得到不同 ID B；操作正常成功。
4. 删除 B。

实际第二次删除成功且 `warnings=[]`；attic 中只剩 B。随后 `restore A` 返回“记录不存在”。无 Git 的核心探针也确认 A 的正文字节已被 B 替换，无法依赖存储本体找回。CLI 探针未执行 reconcile 时同样具备该覆盖条件；它不依赖 B15-5。

**修复建议**：删除和 merge 共用不可覆盖的 tombstone 发布原语；目标若已属于不同 ID，在任何破坏性操作之前返回明确冲突并保留双方字节。若希望同键多代副本共存，需要另行裁决路径与恢复契约。

**关闭条件**：上述序列中 A 不丢失；delete、merge 都有目标冲突回归；无 `.git` 的存储同样成立。不仅补一次 `exists()` 检查，还要保证并发发布不会覆盖。

下钻：需求 R2.3、R11.1；设计 D17、红线10。

## B15-3 [P1 / Critical] macOS 锁必须持续保护所有存活 guard

**位置**：`crates/mem-core/src/lock.rs:89-105,185-187,292-294`；`crates/mem-core/src/platform/macos.rs:107-110`。

macOS 这里使用进程级 fcntl 锁，但每个 guard 都单独开关同一锁文件。进程内注册表能约束本进程的线程，不能弥补跨进程锁被提前释放。

**真实子进程探针一**：同一进程持共享门闩 g1、g2，另一个进程尝试独占。

```text
two shared alive: exclusive acquired = false
one shared STILL alive: exclusive acquired = true
```

只释放 g1 后，g2 仍活跃，外部维护进程却已能进入独占区。

**探针二**：持记录锁，再在同进程等待同一记录锁直到超时。超时诊断经 `read_owner` 另开并关闭该锁文件，导致原有 fcntl 锁释放：

```text
record initially alive: record acquired = false
same process timeout: true
original record guard STILL alive: record acquired = true
```

这使尚未完成的读改写失去互斥，维护也可能穿透写屏障。既有线程争锁测试主要覆盖进程内注册表，没有覆盖这两个真实子进程窗口。

**修复建议**：在平台层统一锁资源所有权：采用能匹配句柄生命周期的原语，或维护每个锁文件的唯一底层 fd 与共享引用计数；诊断读取不能另外关闭会释放活跃锁的 fd。仅调整 Rust Drop 顺序不能解决进程级锁语义。

**关闭条件**：两个探针在原持有者退出前始终获取失败；补真实跨进程回归，覆盖共享持有者部分释放与同进程等待超时。

下钻：设计 D3/D17、红线19/20/22。

## B15-4 [P1 / Critical] 在追加之前修复日志尾部的行边界

**位置**：`crates/mem-core/src/event.rs:196-198,203-221,313-324`；回填追加也应统一使用同一修复规则。

事件日志按行保存 JSON 事件。崩溃可能发生在 JSON 已写完、最后换行尚未写出的时刻。读取当前接受这一完整 JSON，但追加没有补回行分隔符：

```text
valid event missing final newline before append: count=1
after append second event: count=0 torn_tail=true
```

复现只需创建一个事件，删除最后的 LF，再追加第二个事件。追加返回成功，磁盘实际变成两个紧邻的 JSON 对象；下一次读取把整行当成撕裂尾行。后续修复会截断这两条事件，丢掉已经接受的历史并使序号可能被重新使用。

同一尾部恢复入口还有 UTF-8 截断问题：在完整日志后追加 `{"by":"` 加一个中文字符的前两个 UTF-8 字节，`read_log` 直接返回“非 UTF-8”，尚未进入尾行识别。合法前缀无法通过现有回填追加恢复。

**修复建议**：按原始字节识别最后完整行与尾部状态，把“完整 JSON 但缺 LF”和“损坏尾部”都纳入序列锁内的追加准备；UTF-8 错误应按所在行区分尾部撕裂与中部损坏。普通追加与回填追加共用该准备原语，读取本身保持零写盘。

**关闭条件**：补三种回归：完整事件无 LF 后追加、中文 UTF-8 中途截断后追加、同样损坏位于日志中部。前两种保留合法历史并能恢复，中部损坏仍响亮拒绝；事件序号不回退、不复用。

下钻：规范 `envelope-format.md` §4.2；设计 D12、红线24。

## B15-5 [P1] 当前状态查询必须核对记录身份

**位置**：`crates/mem-core/src/event.rs:117-128,486-487,518-527`。

回填逐 ID 比较当前状态与最后日志状态，但 `current_state` 只收路径，优先读取 records 中该路径的文件，不核对其 `envelope.id`。

复用 B15-2 的前 3 步，此时 A 的删除副本仍在 attic，B 正常存活于相同的 records 路径；运行 reconcile。实际追加：

```text
id         = A
op         = backfill
path       = records/topic/reused.md
new_digest = B 当前完整文件的摘要
warnings   = []
```

A 没有恢复或修改，却被登记成位于 records 且拥有 B 的内容。这会污染 ID 与路径映射、增量结果和历史恢复依据。该问题发生在第二次删除之前，即使禁止覆盖 tombstone，仍须单独修复。

**修复建议**：将候选 ID 传入状态解析，在 records 与 attic 分别核对身份；状态、摘要与实际路径从同一次确认取得。不要只因路径存在便认定候选记录仍在那里。

**关闭条件**：A 在 attic、B 占同名 records 路径时，两者状态各自正确；连续 reconcile 不为稳定的 A/B 追加事件；diff 对同一状态解析也一致。

下钻：设计 D12、红线24/26。

## B15-6 [P1] 拒绝 records/attic 路径中的目录越界

**位置**：`crates/mem-core/src/ops.rs:105-118`；写入落点 `:413-440`；`crates/mem-core/src/store.rs:209-210`。

`get` 只检查输入是否以 `records/` 或 `attic/` 开头，随后直接与店根拼接。路径中间的 `..` 未被拒绝，返回的相对路径又被写操作、锁路径和事件登记复用。

**真实 CLI 复现**：在临时店的兄弟位置放一份合法 memory 记录 `outside.md`，执行：

```sh
mem --store "$STORE" show records/../../outside.md
mem --store "$STORE" update records/../../outside.md \
  --expect "$SHOW_TOKEN" --payload '{"title":"modified-outside"}'
```

两条命令均退出 0；店外文件确实被改写，返回 `rel="records/../../outside.md"`。这是合法信封文件的越界读写，不代表任意普通文件都会通过记录解析；也不声称绕过 OS 用户权限。问题是受管记录入口没有守住所选择的店目录边界，MCP 复用相同核心入口。

**修复建议**：给受管记录路径建立统一校验边界，拒绝父目录成分和非法根，确认解析后的目标仍在所选 records/attic 子树；符号链接逃逸也应覆盖。所有写入口和锁路径消费经过校验的路径，避免各前端各自补丁。

**关闭条件**：CLI/MCP 的 show、update、delete、fix 等路径入口一致拒绝越界；店外文件与锁文件均不改变；合法记录路径照常可用。

下钻：需求 R2、R6；设计 D1 的本地存储边界。

## B15-7 [P1] 索引粗筛必须保留 NUL 后的文本命中

**位置**：`crates/mem-index/src/proved.rs:124-126`；物化 `crates/mem-index/src/sqlite.rs:277-305`。

NUL（U+0000）是合法 UTF-8 字节，也可存在于 JSON 字符串。正文或载荷字符串为 `before\u0000needle` 时，直读的字符串包含判断能找到 `needle`，SQLite LIKE 则在 NUL 处停止匹配。索引粗筛先丢掉记录，后续共用精筛无法补回。

**真实执行器复现**：分别在正文、载荷字符串、别名中放置该值，其他域不含查询词。探针使用有效 UUID、加载 memory 类型声明，执行真实物化、检查点保存和 `open_proved`，再对同一库执行 `find::execute`：

```text
direct hits: [aliases.md, payload.md, body.md]
indexed hits: []
assertion failed: indexed must preserve complete direct hits
left: 0
right: 3
```

即使单独只考虑正常正文与载荷字符串，漏召回也成立；不依赖接受带控制符别名的语义。订阅证明由探针模拟，SQL 和两条执行器为原业务代码。

**修复建议**：内容域粗筛使用支持完整字符串的字面子串判定，例如 `instr`，或保守纳入含 NUL 的候选；身份域继续保持前缀语义。不得通过删改正文字节“修复”查询。

**关闭条件**：增加正文、载荷及已接受别名输入的双执行器对账；NUL 前后、查询自身含 NUL、多词与/否定组合、分页续读均不改变命中集合。

下钻：需求 R10.2/3/4；设计红线8。

## B15-8 [P1] 引用建立与删除检查需要共同的并发边界

**位置**：`crates/mem-core/src/ops.rs:625-643`（反向引用检查早于记录锁）；引用写入验证没有与目标删除共享互斥边界。

当前默认删除策略是发现被引用就拒绝，只有显式 force 才允许留下依赖警告。但存在以下实际交错：

1. 测试持有目标 topic 的记录锁。
2. 另一个线程开始非 force 删除，完成“没有依赖者”的检查，随后等待记录锁。
3. 主线程正常创建 memory，其 `topic` 字段指向该目标；存在性检查与写入都成功。
4. 释放目标锁，删除继续。

结果：

```text
concurrent nonforce delete returned success with warnings=[]
new dependent retained refs=[topic -> target]
target location=attic/topic/race-target.md
```

调用双方均走受管接口，没有 force；最终新增依赖悬空。目标 CAS 只检查目标自身字节，不能发现新增的第三方引用。

**修复建议**：让引用建立/重挂与目标删除在同一并发协议下串行化检查和发布。仅把反向检查移进目标锁仍不够，因为引用方写入目前不取得该锁；应在不破坏既有锁序的前提下统一这个边界。

**关闭条件**：用可控同步点替代时延探针，覆盖 add 的 x-ref、update 的 x-ref、link 与删除交错；每种结果都必须等价于某个串行顺序——删除被阻止，或引用写入发现目标已删除而被拒绝。force 行为仍有明确警告。

下钻：需求 R3.4、R11.3；设计 D17。

## Q15-1 [P2 / Required] 只读索引连接仍会创建缓存文件

**位置**：`crates/mem-index/src/sqlite.rs:109-125`。

当前以 `SQLITE_OPEN_READ_ONLY` 打开 WAL 模式索引，并假设读路径不会写盘。该标志限制数据库写入，不阻止 SQLite 为读连接建立 WAL/SHM 文件。

真实 mem-index 探针先建立索引并关闭写连接，再调用 `Index::open_readonly`，目录变化为：

```text
before: [index.sqlite]
after:  [index.sqlite, index.sqlite-shm, index.sqlite-wal]
```

这发生在只读打开内部的 schema 查询阶段，属于派生缓存写入。现有“数据库不存在时不创建数据库”的测试不能覆盖这个副作用。

**修复建议与关闭条件**：在可并发维护索引的前提下，提供真正不产生缓存写入的读取方式，或条件不足时回退直读；不要在数据库仍可变化时盲目声明 immutable。补读入口前后文件集合与内容不变的回归，覆盖伴随文件缺失和只读目录。

下钻：设计红线18。

## 五轴结果与自曝三处

- **正确性与安全**：以上阻断都有编译器输出或运行探针证据；优先处理数据丢失、互斥失效和越界写入。
- **架构与可读性**：优先把路径校验、尾行准备、身份感知状态解析收敛成各自的单源边界；本轮不因大文件行数或个人命名偏好另设阻断。
- **性能**：本轮未做规模压测，不给延迟或吞吐合格结论；修复 LIKE 时应保持粗筛为超集，不能用性能理由保留漏召回。

1. **编译绕过限制**：业务运行证据来自明示修改过编译前置的副本。如果原始源码修复编译后重放结果不同，应以原始源码的新证据修订结论；目前不能把副本构建成功当作发布门禁通过。
2. **并发探针限制**：锁缺陷使用真实子进程；删除引用竞态用持锁与短等待制造交错，已观察到失败结果，但正式回归应加可控同步点。如果能证明受管引用写入与删除已有本轮遗漏的共同互斥，并使该交错无法发生，可推翻 B15-8；当前实现与运行结果不支持这一点。
3. **平台与覆盖限制**：macOS 没有本项目的索引订阅实现，索引命中对账模拟了订阅证明；Windows 本轮未运行。Windows 的 rename 是否覆盖不能由本机结果推定，B15-2 的平台实证限定 macOS；B15-5/6/7 的根因则位于共用实现。真实断电、Windows RDC 和大规模性能均未验证。

本轮仅新增此评审记录，未修改实现、规范或设计版本记录，未 commit、未 push。
