# 嵌入式下游接口契约开发方案

评审稿 · 2026-11-21 · 上游源码核对基准 `a82ab5f`

输入为[下游集成接口契约需求 v0.6](../issues/embedded-downstream-contract-requirements.md)及其 [r5 复审](../review/embedded-downstream-contract-requirements-review.md)。需求复审已关闭 B1–B7。本文提交实现细节供裁决，真实进程、历史发布物和迁移证据由实施阶段产出。

## 1. 评审范围

本期交付单记录 Rust API 契约、真实进程回归、版本化契约与共店防护。共享数据目录仍由库、CLI、MCP 等入口多进程直写。单 MCP 进程是下游可选的部署方式。现有 `commit_batch` 汇总 Git 历史，已生效记录在 Git 失败后保留，并可被其他进程逐条读取。

P1–P5 及产品版本号规则已由维护者于 2026-11-21 批准，均采用 A。整店必需特性、不可变 tag、破坏性变化另发版、单记录契约和维持直写沿用需求已记录裁决。批量工具新增、跨记录事务、扫描快照、serve 集中写以及主设计和需求基线修订均列在本轮批准清单之外。

| 决策 | 背景及依据 | 选项与可观察后果 | 可逆性、推荐与未批准时的处理 |
|---|---|---|---|
| P1 基础格式标记与首次切换 | 当前打开流程直接将 `format_version` 解析为 u32，再探测可写性。单纯增大数字仍会被该源码接受，旧二进制矩阵待实测。 | A 将同名字段改为字符串 `spacememory/2`，显式停机迁移，已知数字解析器在探测前失败。B 保留数字和外部隔离，漏启动一个旧实例就可能继续写新店。 | 启用前可更换编码；启用后回退靠完整备份，变更编码需另迁移。2026-11-21 批准 A。 |
| P2 打开与读取范围 | 缺少写特性时，读者也可能误解事件或记录。当前已有直读路径，各缺项的解析完备性需按发布物验证。 | A 新版本允许合法旧格式显式只读；防护格式缺项仅在所有缺项均有只读兼容声明时允许直读，其余在打开时拒绝。B 所有缺项拒绝打开，消费者无法读取描述为安全的旧投影。 | 开放已验证的只读范围容易，收回已承诺的读能力需版本化。2026-11-21 批准 A。 |
| P3 写许可与维护 | 当前记录写已有跨进程共享门闩，删除、合并等使用独占，部分会话、配置、索引入口另走写路径。现有锁能否覆盖目标平台全部入口，待进程测试。 | A 复用门闩，普通写共享、格式/特性变更及需全店稳定状态的维护独占；升级要等在途写结束，不同记录仍可并行。B 所有写取独占，更易审计，但一个慢写会挡住无关写。仅在开店检查会允许长活句柄在不知情下写入已升级的店。 | 改锁策略会影响并发与超时，发布前按覆盖与时延证据复核。2026-11-21 批准 A。 |
| P4 特性声明与退役 | 目录清单决定写入者是否有能力维护整店。已有记录和历史事件可能继续依赖某项特性。首版基础语义和退役核验成本仍待用样本确认。 | A 把已有机制语义纳入首版基础格式，核验后以空清单启用。新机制先登记，日常只增，退役经显式迁移。B 同样先登记新机制，同时提供日常退役能力，清除时须证明全店无依赖。详见下文。 | 2026-11-21 批准 A。后续可单独引入安全退役；错误清除标记后须停写、核对历史及目录状态。 |
| P5 发布契约载体 | 下游可以固定 Git 提交，但仍需要按该提交判断 API、行为、格式及共店范围。现有 tag 工作流可生成发布说明，远端保护和候选发布物仍待核验。 | A 在 tag 对应提交内固化机读能力、公开接口与行为、变更记录及已验证共店组合。B 沿用标签和自动生成说明，由下游查源码和测试判断兼容性。详见下文。 | 2026-11-21 批准 A。载体在首发前可调整；旧发布内容固定后，修订经新发布点交付。 |

### P4 · 特性标记何时能移除

必需特性表示写入者维护整店数据所需的机制语义。即使当前记录已清理，旧事件或恢复操作仍可能依赖它。已批准的整店粒度与先登记后使用规则适用于两个选项。

| 选项 | 优点 | 代价与失败表现 |
|---|---|---|
| A．首版日常只增加，退役走显式迁移 | 写者只需证明新增依赖在数据出现前已登记。登记后崩溃可以保留标记，无需在线推断这项特性是否还被使用。首版可以集中验证并发登记和崩溃恢复。 | 清单可能留下已无实际依赖的标记，缺少该特性的旧版本仍会拒写。确需恢复这些版本的写资格时，要安排有核验和回退步骤的迁移。 |
| B．首版支持日常退役 | 能在确认依赖已经消除时及时清除标记，让原本仅因该标记被挡住的兼容版本恢复写入。 | 每次清除都必须在独占锁下证明现存记录、历史事件和恢复／维护路径均无依赖。检查漏掉旧事件时，程序会重新放行无法维护整店的写者。完整检查还可能延长独占锁持有时间。 |

例如，某特性改变了事件含义。即使使用它的记录已被删除，只要历史事件仍参与恢复，该标记就必须保留。**已批准 A**。首版尚无完整的在线退役验证证据，额外保留标记的后果是部分版本继续被挡在写门外。A 保留了将来增设退役能力的空间。未使用正文恢复能力的店仍以空清单启用；既有 `restore_body` 事件则由迁移核验并补标。

### P5 · 下游从哪里取得可固定的发布契约

下游锁定一个 tag 或提交后，需要知道该版本承诺哪些 Rust 接口和行为、可以维护哪种目录，以及哪些版本组合有共店证据。两个选项均沿用已批准的不可变 tag 和破坏性变化另发版政策。

| 选项 | 优点 | 代价与失败表现 |
|---|---|---|
| A．每个发布提交携带契约和兼容证据 | 下游固定一个提交即可取得对应代码和契约，可以自动读取格式及特性声明，也能沿变更记录找到适配步骤。出问题时可复查当时的承诺和覆盖证据。 | 上游每次发布都要维护契约和证据，并校验机读声明与运行时一致。验证不足的共店组合无法列入支持范围；如果更新流程漏改能力表，文档与程序会给出不同判断。 |
| B．维持 tag 和自动生成说明 | 发布流程改动小，消费者仍可锁定代码，并保留常规变更摘要。 | 下游得自行从源码和测试推断哪些行为稳定、哪些格式可写。即使 tag 不变，兼容判断也缺少该版本明确发布的依据。单靠这一选项无法满足 C2 的逐版本公开契约要求。 |

例如，下游要判断升级后能否与仍在运行的旧版本共店。A 提供该版本的支持组合和格式／特性声明；B 需要下游自行找两边的实现和测试来判断。**已批准 A**。它把兼容判断交给能够验证并承诺该判断的上游发布方。产品升号规则与首个稳定契约版本的定版条件见 §7。

最可能被推翻的三处

1. **P1 配置替换即完成首次迁移。** 如果历史样本仍进入可写探测，或现有合法店必须修改记录、事件才能被新基线解释，须重做标记或迁移算法。完成条件以历史二进制和真实店副本为准。
2. **P3 复用维护屏障。** 如果有公开写入口无法使用同一把跨进程锁，或者子调用、索引维护回调在持锁期间重入并卡死，当前作用域设计须改；被阻塞的无关写及等待时长也应记录。
3. **P2 缺写能力时继续读。** 如果同一份旧读结果隐去新语义且无法明确报告，或者读取仍触发索引/会话变更，则该特性不得声明只读兼容，直到对应的证据成立。

## 2. 基线与交付面

| 范围 | 已核对和待补证据 | 计划新增面及条款 |
|---|---|---|
| Rust 行为 | `ops::add` 同路径竞争可返回 `Added { created: false, … }`；`MemError::Conflict` 含 `expected/current`；已有锁子进程测试，需穿过真正的写入口。 | 公开行为清单、调用示例、独立进程回归。C1.1–C1.7 |
| 格式/兼容 | `StoreConfig` 缓存配置，顶层未知键进 `extra`；当前 `check_writable` 仅查句柄布尔值。 | 版本标记、必需特性、发布能力声明、结构化拒绝、内部写许可、迁移入口。C2.4–C2.11 / P1–P4 |
| 维护 | CLI bind 直接写配置；MCP reconcile 另触发索引重建；serve 握手有触发持久化维护的分支。 | 受管配置更新，维护接入及无副作用读路径。C2.8–C2.9 / P3 |
| 发布 | `.github/workflows/ci.yml` 对 `v*` tag 构建打包并调用 GitHub Release；本地 tag 列表为空，远端未查询。 | 逐版本契约/变更记录、发布核验与标签保护操作。C2.1–C2.4 / P5 |
| 工具 | 工具注册表在 `crates/mem-mcp/src/tools.rs`，分派在 `protocol.rs`，serve 处理握手及新鲜度。 | 目标提交工具清单及写路径证据；现有历史汇总语义回归。C3.1–C3.5 |

## 3. 单记录公开语义与进程证据

以下覆盖已发布契约明确列出的受管入口。以 Rust 调用结果和真相层观测断言行为。CLI/MCP 对应入口验证结果映射；内部模块可见函数逐个列入稳定清单或标注其内部用途及升级影响。首批范围为 Store 打开、读/写入口、令牌、结构化结果错误、事件读取、必要维护类型；实施时以编译通过的下游示例固定准确签名、特性开关和使用前提。公开 Rust API 变更（包括枚举和可构造结构字段）须列出调用者适配。

| 编号 | 机制及验证点 |
|---|---|
| A1 · C1.1 | 在记录锁内比较版本和整文件 SHA-256。两进程读取同一令牌，再同时更新，恰一成功，输者返回胜者令牌；比较其目标字节和事件行，确认只有胜者写入。不增版本的外部编辑也用旧摘要触发冲突。CAS 失败前禁止产生该操作的记录/事件变更。 |
| A2 · C1.2 | 依类型声明的唯一键生成规范路径，在同路径锁下判定活记录并独占发布；输者返回同 ID、当前令牌和 `created: false`。两进程同键、异键各跑一轮；验证支持的唯一键编码边界，确认不同身份不会被错误合并。 |
| A3 · C1.3 | 普通删除按引用类型及规则阻止或清理；强制模式允许悬空并返回警告。用引用者和归属者样例验证公布的每个分支、受影响 ID 与真相状态。 |
| A4 · C1.4 | 序列锁保护分配和追加，从公开事件读取入口按行核对编号与成功操作的记录 ID、路径及摘要。用干净店的并发非无操作写断言事件连续、严格递增、无重复无漏记，并验证不同记录可具有相同本地版本。历史断链由现有校验报告。 |
| A5 · C1.5 | A 经公开 API 成功后通知父进程，父进程再命令已打开独立 Store 的 B 读取；无 Git 或提交挂起两种情况下读取当前字节、版本和令牌。另在逐文件扫描的两次读取间让 B 修改两条记录，验证各文件可解析且记录混合观察。 |
| A6 · C1.6–C1.7 | 在已完成一笔写后，分别杀死持屏障/记录锁、发布记录后尚未追加事件、事件完整追加但尚未返回的写者；追加中的尾行与新建文件发布也必须有可控故障覆盖。不同存活进程在配置时限内读取、继续写入并显式对账，核对既有成功操作和序列。允许在途写未生效，或已生效且可读/能经事件或漂移检测发现。若出现半文件、已成功事件丢失、重复回填或无界等待，先补复现再改平台原子发布、尾行恢复或锁等待。 |
| A7 · C3 | 用无 Git、成功汇总、提交汇报错三种情况验证已生效记录保留、pending 可对账；在两笔写中间读取或令第二笔失败，验证第一笔仍可见。随发布登记实际工具列表和 serve 职责。 |

测试父进程以 stdin/stdout 或店外标记发握手、就绪、继续、退出指令。子进程在同一临时目录用真实库入口执行，回传结构化响应。按里程碑同步，避免靠 sleep 猜交错；到期判失败、杀死并回收子进程，保留步骤诊断。测试暂停点仅在测试构建可用。优先复用 `tests/locks.rs` 的子进程模式。记录/摘要核验复用现有解析器和事件校验入口，保留相关真实字节以定位半写。

### 已建真实进程覆盖

`crates/mem/tests/embedded_contract_process.rs` 已用独立 `mem` 子进程覆盖 A1 同令牌 CAS 竞态、A2 同唯一键幂等创建、A3 被引用记录的默认拒删与强制 tombstone、A4 不同记录并发事件序列、A5 已打开 Store 句柄可见另一进程写入及扫描中观察合法混合状态、A6 在记录发布后/事件追加后终止写进程并验证回填恢复/不重复、A7 无 Git 与成功/失败 Git 汇总及 pending 恢复、CLI 与 MCP 进程双向共享数据，以及 CLI 旧格式迁移/未知格式探测前拒绝。扫描交错使用测试专用同步钩子：先读完首条目标记录，再让子进程更新两条记录，验证返回结果均可解析且体现逐记录观察。强杀窗口由子测试进程通过就绪文件同步到达后再终止，不依赖 sleep 猜测交错。仍不代替事件尾行部分追加/强杀窗口和逐平台配置时限矩阵。锁原语独立子进程证据在 `crates/mem-core/tests/locks.rs`。

## 4. 防护协议候选设计

### 4.1 磁盘与公开能力

P1/P4 批准的配置以单个原子替换的 `mem.json` 承载，示意仅列版本相关字段

```json
{
  "format_version": "spacememory/2",
  "required_features": []
}
```

- 数字 `1` 是历史基础格式。字符串 `spacememory/2` 是首个防护格式的确切标识。缺少或非法的字段报告损坏；合法但未支持的基础标识在正常打开、探测前拒绝。防护格式要求显式特性清单；写出排序去重的字符串集合，读取时拒绝无效标识和重复项。标识以 `<机制域>.<能力>/v<正整数>` 表达，按完整字符串匹配，未来语义变更分配新标识。
- 每个发布提交携带能力文档和机器能力表，显式列支持读取和写入的基础格式集合、`supported_features` 以及各基础格式上可读但不可写的特性。运行时用同一机读输入或由该输入生成的常量执行判定；测试校验发布声明与实际运行结果。机制标识可编入代码，调用者的类型和载荷仍由数据声明。
- 基础格式覆盖原有信封、事件操作、恢复、机制配置和索引重建约定；新机制只有在店内实际使用时才进入 `required_features`。`event.restore_body/v1` 已成为首个真实特性：正文恢复在独占门闩内登记后再写记录/事件；未使用该能力的店保持空清单。迁移旧格式时扫描已验证的事件历史，若已存在 `restore_body` 则补登记。能力表列当前实际解析的 Schema 方言、引用扩展、未知字段处理范围及索引重建规则。自定义类型、载荷、正文样本单独做迁移前后保真。
- 配置更新通过原始 JSON 对象定点修改，保留顶层和嵌套未知键值。记录改写保留未知信封键与未修改载荷，遵守整文件 CAS 及正文字节保持。事件历史原则上原字节保留；若维护/重写路径无法无损保留未知可选内容，该入口结构化报告兼容性缺项并拒绝，包含位置和字段。已知需要写语义的扩展须事前登记完整特性 ID。

### 4.2 打开、写许可与错误

| 条件（自上而下） | 行为 |
|---|---|
| 配置不可解析、形态非法，或基础格式未知 | 正常打开失败，跳过探针。 |
| 基础格式仅支持读（首版包括经验证的数字 1） | 仅显式只读打开；普通打开报告迁移要求，写操作拒绝。 |
| 支持基础写格式，缺写特性且任一缺项未声明读兼容 | 打开失败，报告完整缺项。 |
| 支持基础写格式，缺写特性且全部缺项声明读兼容 | 允许只读打开，直读，跳过探针；写请求报告全部缺项。 |
| 格式与特性均支持 | 显式只读无探针；Auto 复用现有临时区探测，卷不可写时只读。 |

公开读取使用当前支持的读规则。受管基础格式迁移要求关闭全部现存句柄，禁用在线基础格式迁移。可在线增加的特性须保持该基础格式的读取规则；不能为旧读者维持基础解释的变化走离线基础格式升级。防护格式的公开读取在读取前后各校验一次磁盘格式和特性，采用只增集合保证读取过程中出现的新缺项会在返回前被发现。新状态不满足只读声明时拒绝返回本次结果；读兼容但需要降级时走真相层直读并重新核验。集合校验只约束兼容性，跨记录读取依旧允许混合观察。读取无持久副作用，允许的只读打开及读取不得创建锁文件、打开可写 SQLite、启动维护进程或经握手间接触发持久化。不能证明索引新鲜且兼容时走真相层直读。

本机实现以 `Store::read_compatible_missing_features()` 重读磁盘配置：返回本版本不支持但声明允许只读的缺项；若有缺项未声明只读兼容，则返回完整排序的结构化缺项错误。记录、事件、台账及统一查询入口在读取前后复核；只读兼容缺项只能直读真相层，不得使用派生索引。若索引证明构造期间兼容状态变化，则不返回索引句柄。

写入口的共同流程如下。

1. 以物理目录归一化后的店标识获取同一既有跨进程维护门闩；共享供普通单记录写，独占供格式/特性/config 更新、删除/合并等现有独占操作和索引/checkpoint 维护。保留门闩、记录锁、序列锁顺序。路径别名与同进程多句柄共用该边界。拒绝前允许触及获取门闩所需的既有运行时文件。
2. 持门闩重读磁盘配置、验证格式、全店特性、打开模式与相关维护状态。句柄的 `writable()`、缓存配置及此前握手都不能授权写盘。将本次读取的配置用于受管路径中影响写正确性的规则。
3. 用不可外部构造、作用域绑定 Store 的许可向内部写原语传递证明。嵌套路径传入同一许可，包括 `*_locked`、事件追加、会话轨迹、Git pending 和组合维护。禁止二次抢门闩或共享升级为独占。既有公开写入口通过门闩包装，已公开的低层可写签名若无法安全兼容，列为破坏性 API 变化并在批准的发布点记录。任意调用者自行用 OS API 写目录、修改 Git 工作区或改协议元数据在受管边界之外，由部署方协调。
4. 保持许可直到实际受管写、事件登记及本次维护副作用完成。外部提交汇或维护回调须有明确的无重入规则；若将工作移至后台，后台每次执行重新获取许可。写者非正常退出由平台释放门闩，再依现有死 PID 规则、事件与当前状态对账接管。保留有界等待和结构化超时，核对当前阻塞式屏障与配置时限是否需补齐。

错误通过 Rust 结构化结果和 CLI JSON/MCP error data 透出。候选错误类别为基础格式不支持（发现值及支持集合）、需要迁移（源/目标）、特性缺项（排序后的完整标识）、未知内容无法保留（路径/字段）、只读、迁移状态非法及锁超时。实现登记稳定判别码与字段，发布前冻结名称并以两前端测试保护；现有 CAS 冲突保持原义。拒写验收比较真相层、事件及派生持久状态。

### 4.3 特性登记和维护覆盖

机制操作启动前即可判断是否可能引入新特性。若可能，直接进独占路径，在锁内重读并检查现有集合和本版本能力，依据当前集合求并集。以同文件系统原子替换提交清单后再写依赖数据和登记事件，期间持有同一独占许可。登记失败时保持业务数据原状；登记成功后崩溃，保留可能多出的特性标记。重试按集合幂等。首版无通用用户删标记接口；退役须另行在独占迁移中证明现存记录、历史日志和恢复/维护机制均已消除依赖。缺少任一写特性时，不得通过维护或登记规避全店拒写。

| 写路径 | 接入方案与核对点 |
|---|---|
| `mem-core/ops.rs`、`event.rs`、`gitattrs.rs` | 记录操作、事件追加/回填、`.gitattributes`、独立或嵌套的 `commit_batch`、pending 重试/清理。回填在许可内拿规范记录锁后重算状态，然后持序列锁追加；只读 drift 独立。 |
| `session.rs`、`agenda.rs` | 显式 start/flush/close、轨迹登记、提案物化与 apply；内部复用父许可。纯读 brief、计划读取与扫描返回内存结果。 |
| CLI bind/init、Store 配置与目录原语 | bind 移到库级受管配置更新，使用独占许可和保真原子替换。init 为单独的新店入口，防并发重复初始化，最后发布可打开配置。资产播种、临时目录或输出文件需按店内/店外及持久化性质分别核对；店内写入均纳入适用门禁。核对低层公共目录原语、类型/约定安装相关路径，避免保留绕过口。 |
| `mem-index`、`freshness.rs`、`sidecar.rs` | 从可写打开（包括建库）到重建、checkpoint/派生文件发布均需许可。独立维护 tick 保留；握手只返回内存/只读观测，未收敛时按不新鲜应答。禁用读触发的持久化启动/维护。测试直接调用及前端路径。 |
| CLI 与 `mem-mcp/src/protocol.rs` | reconcile 是显式写，回填、pending、索引和检查点统一在一趟受管边界内使用内部许可。其他工具逐个按工具清单对应库入口，验证无前端先写后检查。 |

#### 当前入口盘点（本机源码核验）

| 入口 | 门禁与许可 | 仍需补的证据 |
|---|---|---|
| `ops::{add, update, link, delete_record, restore_record, merge, fix_record, restore_body}` | 各公开写入口持共享或独占门闩，锁内重读磁盘兼容状态；低层 `*_locked` 由受管父路径调用。 | 所有拒绝原因与记录/事件/pending 全净零矩阵；每个失败窗口真实进程覆盖。 |
| `EventLog::append_*`、`reconcile_backfill`、`commit_batch` | 低层事件追加已收窄为 crate 私有；回填和提交汇持共享许可并复核配置，事件序号继续在叶子序列锁中分配。 | 仍需 A4/A6 失败窗口真实进程证据；测试专用事件注入符号仅在 debug 集成测试构建暴露。 |
| `session::{start, flush, close}`、`agenda::{write_proposals, save_plan, apply}` | 公开入口复核兼容性；组合 apply 在独占边界内使用锁内变体。写后 trail 登记 `session::register_trail` 已收窄为 crate 私有。 | session `persist` 与 `save_plan_locked` 是模块内部 helper；现有句柄在另一进程改写必需特性后拒绝 session 写并净零，仍需其他平台并发矩阵。 |
| `Store::{register_required_feature(s), set_config_value, migrate_format_v1}` | 特性/config 独占更新；单数登记委托给批量入口，成功后同步缓存配置；迁移专用锁加独占屏障，并以目标配置原子替换为唯一完成点。 | 空集合无操作、非法/未支持特性拒绝、批量不支持清单排序完整且 `mem.json` 净零已有测试；正文恢复正向登记、旧事件迁移补标、登记/数据/事件三阶段强杀与同特性并发进程也已验证。不同特性的 F/G 并发并集需第二个真实特性；正式历史发行包和真实部署备份仍待 T09。 |
| `Store::read_compatible_missing_features`、`ops::{get, scan_all, list_pending_commits}`、`EventLog::{read_log, enumerate_md, enumerate_candidates, current_state_for_id, current_path_for_id}`、`agenda::{load_plan, list_plans}`、台账/view/find/drift 读取 | 公开真相层读取前后重读磁盘兼容配置；只读兼容缺项可直读，不兼容缺项拒绝。缺失台账目录只返回空值，不创建目录。 | 本机用旧 Store 句柄验证记录、事件状态/枚举、agenda 计划、pending、台账与 find 拒绝；用可控扫描交错验证兼容性在扫描返回前复核，索引证明失效并降级。真实扩展特性样本仍缺。 |
| `Index::{rebuild, rebuild_with_entries, set_body_texts}` | 公开物化入口复核兼容性；sidecar 使用不可伪造的 `ExclusiveBarrierGuard` 建立维护许可，避免持锁重入。 | 覆盖 MCP/CLI 与维护 tick 的失败净零；只读/句柄打开的跨进程负例。 |
| `Store::create`、init 播种、`.gitattributes`、类型安装 | 初始化通过不可伪造、绑定目标路径的 `InitPermit` 覆盖建店与 `.gitattributes` 播种；CLI 自举用 `.init-in-progress` 阻止普通打开与格式迁移，重试时把半成品原样移至同级隔离目录后重新初始化，成功后才移除标记。`gitattrs::ensure_for_write` 收窄为 crate 私有。店索引打开先取维护许可，已持许可的路径显式传入同一许可。 | 已有进程测试验证标记阻止普通打开、完整半成品字节隔离保留后重试成功、迁移遇标记净零、跨目标伪用许可拒绝及缺特性时索引不落盘。仍需类型安装与所有低层入口的独立进程交错验收。 |

此表区分“调用路径当前有兼容性门禁”与“低层 API 已无法绕过”。未列为稳定下游入口的低层可写符号，在发出首个稳定契约前应收窄可见性或新增不可构造许可参数；不能用文档声明抵消可直接调用的写入口。

## 5. 首次迁移、失败恢复与回退

P1 已批准采用**显式停机迁移**。实施时接通库级迁移操作及现有管理前端的相应入口。迁移前必须验证本节的现场前提；它具有窄范围迁移权限，不经普通旧格式写入口。操作参数至少固定源目录、目标基础格式和备份身份，部署记录列命令、构建提交、目录、历史进程与服务清单、停止和恢复时间。迁移程序不能仅凭目录锁证明历史进程已停止。

1. 部署方停止历史及新协议写者、读者和 sidecar，关闭所有现存 Store 句柄，停用自动拉起并隔离旧二进制/服务，确认迁移期间独占访问。保存店外完整可恢复备份（含配置、记录、事件、会话、类型与约定、Git 及 pending）；运行时锁和端点由恢复后的进程按原规则重建。先在临时副本完成恢复演练。
2. 迁移程序取得同目录独占门闩，以专用解析器核验数字 1 的配置与源格式。按基础语义预检事件/状态、未知字段保真与机制扩展；识别损坏或未处理的恢复欠账时报告并停止，由部署方先用适用旧版本显式修复后重新停机和备份。历史备份或文件尾损坏不由迁移静默修改。
3. 首次迁移以配置标记为唯一真相切换，保留其余配置键并写入目标标记与经核验的清单。记录、日志、用户 schema/payload/body 原样保留。复用同卷临时文件及原子替换，配置替换前源店仍是旧格式；替换后新标记即为完成，残余临时文件只在显式维护中清理。派生索引在重新开放前由新版本独立重建，或先以无法证明新鲜的状态直读；旧证明不得用于新格式快路径。
4. 杀进程后，先保持隔离，读盘判定：完整源标记可核对备份后重试；完整目标标记进入迁后核验，重复命令只确认它且不得重置已有特性；缺失、损坏或非预期标记保持停机，恢复备份再诊断。若原子替换无法在候选平台证明上述结果，阻止该平台启用，并在设计复审时提交可识别的迁移中状态方案。
5. 用实际历史发布二进制尝试公开打开；以配置反序列化错误以及探测前无临时/持久写为通过条件。记录精确提交或二进制哈希、入口、平台与输出。未纳入证据的历史构建继续由部署方隔离。启用新写者前运行新版本只读校验和副本写入冒烟。
6. 回退时重新停机，保留失败或已迁目录作为取证副本，从迁前完整备份恢复至独立可验证目录，再切换服务路径。迁后写入需要人工导出并按兼容路径重放；不得仅把字符串标记改回数字后重启旧写者。记录恢复验证和迁后写入处置。未来需要改多个真相文件的格式迁移须独立设计提交与恢复协议。

## 6. 格式、并发与版本验收

| 编号 | 验证 |
|---|---|
| F1 · C2.5–C2.7、C2.11 | 用已支持、已识别但不支持、未知、多个缺项，以及损坏配置和未知基础格式测试打开/读/写矩阵；新鲜许可使用磁盘配置。测试未知可选键跨配置与记录修改，含嵌套键和二进制正文样本。 |
| F2 · C2.8、C2.11 | A 持共享许可并在校验后暂停。B 以独占尝试登记并写依赖数据；先证实 B 无法完成，再释放 A，见 A 先完成、B 再完成。随后复用 A 原 Store 发起下一笔写，必须报告缺项。另令 A 在读取首轮校验后暂停，让 B 激活 A 不可解释的特性，验收 A 返回前拒绝输出；兼容特性则验证只读降级。对普通写和每类维护许可/入口执行拒写。两个不同真实特性的 F/G 并发并集验收仅在构建支持至少两项真实特性时适用；少于两项时以同一真实特性的跨进程并发登记幂等、无丢更新及依赖写等待许可交错作为当前版本验收。加入第二项真实特性后，必须补做 F/G 并集和两类依赖数据/事件可解释性验收，不得注册仅供测试的虚构特性。 |
| F3 · C2.8 | 在独占边界中分别于清单替换前、替换后尚未写依赖数据、数据发布后事件返回前杀进程；核对清单和依赖数据、恢复可观察性及重试幂等。 |
| F4 · C2.9 | 用缺写特性但允许读取的版本，在有索引、session、sidecar 条件下运行读取和握手，逐树比较真相、事件、索引及其边文件、台账和 checkpoint。以写操作观测补足最终字节比较无法发现的临时写，单独记录原有 Auto 探针例外。逐个尝试 flush、重建和 reconcile，验证拒绝和无持久状态变化。 |
| F5 · C2.6、C2.10–C2.11 | 首发前使用可复现历史程序样本与隔离合成店执行 §5 演练；协议版本和不同能力版本依次访问同一店，对比迁前后业务文件字节与机制解释。测试成功、失败、重试和从店外完整备份恢复，以及历史路径在探测前拒绝。真实部署店仅在部署数据可用、获授权后执行发布后运维复验；不能验证的旧程序记录隔离。 |
| V1 · C2.1–C2.4、C3.1 | 从候选提交固定 Git revision 编译最小下游、运行 A/F 证据并核对工具实际枚举。tag 发布后解析到提交及契约内容摘要，下一版本逐项复查旧映射。打包与播种检查依[打包指南](../rules/packaging.md)。 |

本机使用真实进程和独立构建的能力档验证交错；正式声明的跨发布组合还需实际版本二进制证据。macOS、Windows 的文件锁、进程终止、原子替换及探测边界分别登记。开发机不能执行的 Windows、历史架构及实际发布物测试列外部验收，保留平台、责任人、输入、预期和产物路径；相应范围的发布声明须等待证据。SpaceTeam 的固定 revision、POC 与升级测试由下游运行。

## 7. 发布交付与兼容判定

候选发布点在同一提交提供 Rust 稳定接口清单和编译示例、C1 行为/边界与测试索引、机读格式/特性声明、已验证的共店组合表、变更与迁移记录。每项公开变化列影响的 API/行为/格式条目、消费者适配和兼容类型。不同发布版本只有声明且验证过的组合才在支持范围；进程对数据目录实施基础格式和特性检查，共店部署方负责挑选组合。协议不需要在目录里登记每个运行进程的产品版本。

发布前由维护者核验远端 tag、既有对外承诺与实际发布产物，据此确定首个稳定契约版本的具体号码。同时配置 tag 禁止更新与删除，并检查工作流从目标提交构建及附带资产。证据存储发布名、tag 解引用后的提交、契约摘要、构建平台和能力档。破坏性改变使用新版本号和新 tag，兼容修订也签发独立发布点。发布执行由维护者授权，首次防护协议版本以通过的 C2 实际发布提交标识，历史旧程序的自动拒绝范围由 F5 样本登记。开发/首发环境若没有真实部署店，首发前使用隔离合成店、历史源码样本和故障注入验证迁移与备份恢复；真实部署店复验在实际部署数据存在且获授权后进行，不是开发阶段的首发门槛。

### 产品版本号规则

2026-11-21 批准采用 [SemVer 2.0.0](https://semver.org/spec/v2.0.0.html)。稳定产品版本使用 `MAJOR.MINOR.PATCH`，升号判断覆盖已发布的公开 API、行为、数据格式支持与共店承诺。每次发布使用新版本和新 tag。若同批包含多类变化，按影响最高的一类升号。

| 变化 | 升号 | 示例（以 `1.4.2` 为示意） |
|---|---|---|
| 兼容修复 | PATCH | 修复事件漏记，使实现恢复已发布的行为，发 `1.4.3`。 |
| 兼容新增或公开接口标记弃用 | MINOR，补丁位归零 | 新增独立查询 API，现有调用与数据目录支持继续成立，发 `1.5.0`。 |
| 破坏已发布的承诺 | MAJOR，次版本与补丁位归零 | 修改公开 Rust 签名、撤销 CAS 冲突响应中的当前令牌，或要求既有可写店先迁移才能继续写，发 `2.0.0`。 |

新增可选机制特性可走次版本，前提是升级后未启用该特性的既有店仍可按原契约运行，已声明的共店组合依然成立。启用特性后，缺少该能力的旧写者会拒写，发布记录须说明启用和恢复条件。若升级本身就登记特性，并导致原先受支持的旧写者无法继续写，则升主版本。

产品版本标识发布，`spacememory/2` 标识店的基础格式。具体共店组合以发布契约、基础格式和目录必需特性为准。2026-11-22 本机 `git tag --list` 为空，`git ls-remote --tags origin` 无输出；GitHub API 返回仓库规则集仅有作用于 `main` 的 active branch ruleset、未列出 tag 目标，经典 tag protection 查询返回 HTTP 404（未取得保护规则）。维护者于 2026-11-22 选定首个稳定契约候选版本 `1.0.0`；此决定用于本分支候选元数据，不代表已经审定或发布。

## 8. 执行计划

前序任务产出的证据随产物提交。方案验收条款集中在 §3、§5–§7。

- [x] T01 记录基线与写入口矩阵。产物为 §3 接口映射与现有公共入口盘点；证据为 CLI/MCP/sidecar 分派对照及实际工具枚举。下游示例和正式契约文件并入 T08。
- [x] T02 完成本机 C1 多进程回归。覆盖 A1 CAS、A2 唯一键、A3 默认/强制删除、A4 事件序列、A5 已打开句柄及扫描混合观察、A6 记录发布/完整事件追加/事件尾行半写后的进程强杀与恢复、A7 Git 失败/pending 收敛、CLI/MCP 双向共店和并发 init；迁移 CLI 拒绝/保真也由独立进程运行。强杀后由独立 CLI 在 10 秒内 reconcile、读取并继续写入。本机证据见 `crates/mem/tests/embedded_contract_process.rs` 和 `crates/mem-core/tests/locks.rs`；Windows/其他目标平台时限矩阵列 T09 外部验收。
- [x] T03 实现基础格式与能力拒绝。完成字符串格式解析、未知格式探测前拒绝、只读旧格式、磁盘必需特性重新校验、旧 Store 句柄的记录/事件日志、状态与枚举/pending/agenda 计划/台账/find 读取前后兼容检查及机读能力 JSON 导出。旧数字格式显式只读打开现以整店路径和文件字节快照验证零持久化。只读兼容缺项直读，其他缺项拒绝。证据 `foundation.rs`、`embedded_contract_process.rs`、`session_view.rs`、`maintenance.rs`、`find_kernel.rs` 与全工作区通过结果。
- [x] T04 接通记录、事件与特性登记许可。记录操作和配置更新复核磁盘特性，登记在独占门闩内幂等合并；单数登记委托批量入口，成功后同步句柄缓存；低层事件追加已收窄为 crate 私有。空登记无操作、非法/未支持特性拒绝、批量缺项完整排序且配置字节净零由 `foundation.rs` 验证。首个真实特性 `event.restore_body/v1` 由正文恢复入口在同一独占门闩内先登记、再依赖写；旧格式迁移根据已校验事件自动补标。正向登记、迁移保真及真实独立进程在登记后/记录发布后/事件追加后强杀三阶段均有测试；另验证恢复写者在独立进程中等待共享写者完整发布记录及事件后再登记特性和执行依赖写。崩溃后重开与对账通过。2026-11-22 维护者选择“当前 1.0.0 只按已有真实特性验收；不同特性并集在不足两项时不适用”的方案：本版本已有同一真实特性的跨进程并发登记幂等和依赖写等待交错测试，因此不同特性 F/G 测试不阻止 T04；未来加入第二项真实机制特性时，该并集及两类依赖数据/事件可解释性测试自动成为必需门槛。不得为关闭测试而造虚构能力。A6 另覆盖通用记录发布、事件尾行半写后的强杀恢复。
- [x] T05 接通会话、agenda、配置和提交待办。公开 session/agenda/config 写入口复核磁盘能力；trail 低层写入收窄为 crate 私有；CLI/MCP reconcile 在单一独占许可中完成回填、pending 重试、索引物化和 checkpoint。CLI 初始化标记、半成品隔离恢复、配置变化后既有 Store 句柄拒写/拒读、缺失 `sessions/` 时只读扫描不建目录均有测试。验收：`session_view.rs`、`maintenance.rs`、CLI/MCP reconcile 测试、`embedded_contract_process.rs`。
- [x] T06 接通公开索引物化与 sidecar 许可。店级索引创建先验证特性并持独占维护许可；已持许可的 CLI/MCP/sidecar 路径将同一许可传到索引打开、重建、正文素材和 checkpoint。缺特性拒写时 CLI/MCP 对 index、events、pending、checkpoint 的快照均保持字节一致；长活句柄磁盘特性变化后 `open_proved` 拒绝旧索引快路径；迁移后旧格式检查点失效。验收：`index_paths.rs`、`cli.rs`、`mcp.rs` 与全工作区检查。
- [x] T07 本机迁移与回退验收。已完成停机迁移 API、CLI 入口、深度预检、未知键保留、失败关闭、幂等、完整备份到独立副本的递归逐文件字节比较，以及迁移格式切换前/后的独立进程强杀、重试和已迁移识别。迁移前建立真实 SQLite 索引与旧格式检查点，迁移后 `open_proved` 明确因格式不匹配直读、不给索引句柄，旧派生文件字节保持不变；证据 `mem-index/tests/index_paths.rs::migrated_format_rejects_legacy_index_checkpoint_as_proof`。其他进程证据 `migration_process_death_before_and_after_atomic_switch_is_recoverable`；迁移和完整树回退 fixture `foundation.rs`。新增本机历史源码样本：从干净提交 `8026d0d9f2fdb0252bff5405a982e9927299a418` 重建 `mem 0.1.1`，并精确复现 macOS release 二进制 SHA-256 `43226de5d577bb084bee05dee7dff924df35c17eb0168415a0f9ce7e2d429437`；可复跑副本位于 `dist/embedded-contract-candidate/historical-mem-0.1.1-8026d0d9`。该源码构建旧程序不是正式发行包。由该旧二进制创建数字格式 1 店；当前 CLI 迁移成功。迁移前 61 文件备份清单逐项匹配，迁后除 `mem.json` 外原有 60 文件均按字节保持，仅新增空 `migration.lock`；从独立备份恢复后旧二进制 `list` 成功。该样本同时验证旧二进制尝试打开 `spacememory/2` 时因字符串类型错误早期拒绝，打开前后全店文件清单与 SHA-256 完全相同。2026-11-22 重建与拒绝复验命令记录见末尾追加项。正式历史发行包、真实部署数据及 Windows/其他平台原子替换演练属于 T09 外部验收，不计入本机 T07 完成范围。
- [x] T08 发布候选契约。本机产物包括 Rust API 签名候选清单、带构造器/扩展类型约束的上下文与错误 API、覆盖契约表中 mem-core 与 mem-index 稳定入口及下游非穷尽错误匹配方式的编译期消费者探针、只读消费者示例、能力 JSON、MCP 工具名清单和[候选变更记录](embedded-downstream-contract-changelog.md)；探针还以穷尽构造 `AddArgs`、穷尽匹配 `OpenMode` 锁定 1.0 输入形状，字段/变体变更按 MAJOR 处理，并编译锁定支持/只读兼容特性查询 API。manifest 的格式、特性、版本字段与运行时能力逐项一致性由 `foundation.rs::capability_manifest_matches_runtime_format_and_feature_tables` 验证，并检查 API/MCP/变更记录指针、未验证共店声明及候选发布状态。本机静态复核记录见 [实现复核](../review/embedded-downstream-contract-implementation-review.md)，范围内未发现阻断项。维护者于 2026-11-22 接受 `1.0.0` 契约候选；这不构成发布授权。Cargo 运行时版本、能力 manifest 与文档已同步；候选未发布、共店组合为空。发布物、tag 保护和外部验收证据属 T09。`cargo fmt --all -- --check` 因大量既有格式差异失败，未全仓格式化。
- [ ] T09 分阶段外部验收与发布。
  - **首发前硬门槛**：完成适用于本次发布声明的平台/架构验证；提供本机候选包之外的固定发布提交构建与签名/来源证明；核验可取得的正式历史发行二进制对新格式的拒绝行为；以本机隔离合成店、历史源码样本、故障注入及完整备份恢复验证迁移成功/失败/重试路径；核验覆盖发布 tag 的保护/不可变规则；明确批准发布的提交、版本、资产、能力声明、回退方案。真实部署数据不是本开发环境的前置输入，不能因无法提供生产店而阻止首发。缺少其他适用证据或授权，不得把候选标作正式发布。2026-11-22 的远端快照显示暂无 release/tag，已有规则仅覆盖 `main` 分支，尚无 tag 保护证据。
  - **正式发布后运维验收**：真实部署店迁移、备份与回退演练在实际部署数据可用并获授权后进行；记录停机隔离、迁前后文件摘要、迁移结果及恢复验证。开发阶段已完成的本机迁移、故障注入和合成样例证据不替代真实环境演练；该演练**不阻止首个 `1.0.0` 发布**。
  - **正式发布后下游验收**：SpaceTeam POC 与升级/回退测试须以正式发布的 `1.0.0` 资产为输入，固定下游仓库 revision，记录调用入口、升级路径、结果与数据快照摘要。该验收用于证明下游真实集成和指导后续兼容性声明，**不阻止首个 `1.0.0` 发布**；在完成前不声明 SpaceTeam 已通过，也不把该组合加入 `validated_coexistence`。
  - **后续平台发布门槛**：Windows 不是本机迭代要求；若本次或后续发布声明支持 Windows/其他平台，需在相应 runner 上验证锁、强杀、原子替换、历史探测及消费者示例编译。平台范围未验证前，不对该平台作支持声明。

### T04 / T09 未关闭项证据清单

下表把剩余工作拆成可交付的证据包。`待提供` 表示当前仓库和本机环境没有可复核输入；提交材料后按对应验收项复跑并把产物路径、提交/版本、命令、结果和哈希补入计划。维护者于 2026-11-22 确认目前没有第二项真实机制特性或外部验收材料。维护者接受 `1.0.0` 候选不等于下表各项通过，也不授权发布。

| 项 | 负责方与输入 | 关闭证据 | 当前状态 |
|---|---|---|---|
| T04 · 两个不同真实特性并发登记 | 机制实现者在未来新增第二项真实特性时，记录其独立产品语义、实际依赖的数据/写入口和来源决策；不能只为测试注册标识 | 当受支持真实特性不少于两项时，两个进程基于同一初始清单并发登记 F/G 并执行各自依赖写；重开后清单恰为排序后的并集、两类数据/事件均可解释，重复运行幂等。少于两项时该子项标记为 N/A；同特性并发幂等与依赖写等待交错是当前版本验收。第二特性加入时该证据转为必需，并通过对应 F2/F3 与全工作区测试 | **1.0.0 范围内 N/A**：维护者于 2026-11-22 选择方案 2；当前仅 `event.restore_body/v1` 一项真实特性。未来新增第二项时自动重开此门槛 |
| T09 · 候选 macOS 分发包 | 当前候选源码与本机 Apple Silicon macOS 发布工具链 | `scripts/package-release-macos.sh` 生成 `mem` + 同级 `builtin-types/` tarball；包内二进制版本核验，执行 init/list 冒烟；另对已初始化临时店在 packaged `list` 前后比较全树文件数与路径/内容 SHA-256；资产目录与仓库源逐项一致，并记录归档/二进制 SHA-256、Git HEAD、工具链和 release 编译输入摘要 | **本机候选包已通过**：`dist/embedded-contract-candidate/mem-v1.0.0-aarch64-apple-darwin.tar.gz` SHA-256 `dfd7b24d4a28a6bce58e6567664396c3e0adfcf1969bdf86d7a729f9bcc6f6c6`；包内 `mem` SHA-256 `774ce202b971823f3f175bcc8ac6bb66d2e03215b803e34a85d69eda5eb4a7b1`。构建输入清单 `dist/embedded-contract-candidate/candidate-build-inputs.sha256` 摘要 `af7742d95bf197ff0a1a52182c60cb2e24c415f05206380bcb8d0acce4c1f884`，来源记录 `candidate-build-provenance.txt` 摘要 `11cb78d93e3939e8da77b469d364be544b7f4f05e8e1af51b888a9d29d8bc2a3`。这仍是未签名本地候选，不代替从固定发布提交构建或正式 Release |
| T09 · 候选 Windows 分发包 | 当前候选源码与本机 x86_64-pc-windows-msvc 工具链；`scripts/package-release.sh` 与 CI `release-windows` job 为同一脚本 | 生成 `mem.exe` + 同级 `builtin-types/` zip；包内版本核验、exe 旁播种冒烟（`asset_source=external`）、init/list 可用；资产目录与仓库源逐项一致；packaged `list` 前后全树聚合 SHA-256 比对；记录归档/二进制 SHA-256、Git HEAD、工具链与编译输入清单 | **本机候选包已通过（2026-11-24）**：`dist/mem-v1.0.0-x86_64-pc-windows-msvc.zip` SHA-256 `1a8469f890c86aaa56972a19de899224d765fd1d42131e382a263763121ab42e`；包内 `mem.exe` SHA-256 `c70a7117bdc31a6e25aee00897c917f8c5d19adb23c1900b0bfe301cb252901b`；读路径零写盘复核通过；来源指纹 `dist/embedded-contract-candidate/candidate-build-inputs-windows.sha256`（78 项输入，摘要 `af23cef5bbd16160cf8178c869b251ead3821e977d14afcfc4248838d0bc7b91`）与 `candidate-build-provenance-windows.txt`（摘要 `1fff0de1d8e95dfb4cad880852b43f70c72a2c9fb44c78491474e2090a3e52ed`）。未签名本机候选，不代替 CI 固定提交构建；MSI/NSIS 安装器未实现（打包指南 §2 未收口项） |
| T09 · 正式历史发行二进制 | 发布维护者提供真实发行资产及其 Release/tag、平台/架构、SHA-256；版本提交源码构建只作为补充样本 | 对每个声明覆盖的历史二进制记录版本、哈希、平台、打开新格式的入口与输出；证明在协议拒绝之前没有临时或持久写，并保存前后整店文件清单/哈希。§5 第 5 步与 F5 结果可复跑 | **维护者确认目前无外部验收材料**；本机只有从历史提交构建的 `0.1.1` 补充样本（macOS 与 Windows 本机各一）；GitHub Releases 页面当前无正式 release |
| T09 · 真实部署店迁移/回退（发布后运维验收） | 实际部署环境具备后，由店数据责任人提供经授权的脱敏副本或隔离克隆、旧程序/服务清单、可恢复的店外完整备份及恢复目标 | 在隔离副本演练：停机隔离、迁前树清单与哈希、正式版本迁移、迁后业务文件保真和能力核验、迁后写入处置说明；再从独立备份恢复到新目录并验证回退路径。记录命令、输入/输出摘要、失败/重试结果。开发阶段以合成店、历史源码样本与故障注入完成可复跑门槛；真实副本演练不作为首发条件 | **首发前不适用/不阻断**：本开发环境没有真实部署店；发布后实际环境具备并获授权时执行。本机迁移、备份恢复与进程故障测试已完成 |
| T09 · 不可变发布 tag | 仓库管理员配置规则；草案与 JSON 见[发布 tag 保护规则草案](release-tag-ruleset-proposal.md) | 保存规则 ID、目标模式、限制项和活动状态的远端读回结果；在一次性测试仓库用相同规则验证首次创建成功、后续更新和删除被拒；记录发布提交、tag 指向、契约摘要与资产清单。没有活动规则或无法证明匹配范围时不声明 tag 受保护 | **规则已应用并读回核验（2026-11-24）**：ruleset ID `27981120`（"Protect release version tags"），target=tag、active，匹配 `refs/tags/v[0-9]*`，限制 update+deletion，`bypass_actors` 为空（创建响应含 `current_user_can_bypass: "never"`）；与既有 main 分支规则（`27179551`）并存。一次性测试仓库的首次创建成功/更新删除被拒行为验证仍待执行（需创建临时仓库，待授权）；正式 tag 创建后再读回规则与 tag 指向归档 |
| T09 · SpaceTeam POC 与升级（发布后） | 正式 `1.0.0` 发布资产已可用后，下游维护者提供可访问仓库、固定 revision、最小调用路径和测试环境 | 固定下游 revision 针对正式发布资产执行编译、读写/API 与适用的 MCP 工具枚举；从下游当前受支持版本执行升级/迁移及失败回退，并归档命令、结果、实际产品版本、店快照摘要。若其实际不使用 Rust/MCP 某项入口，应在验收记录说明适用范围。此验收可关闭发布后的集成证据，但不作为首发前置条件 | **首发前不适用/不阻断**：正式版本发布后再由下游执行；完成前不声明 SpaceTeam 验收通过或已验证共店 |
| T09 · F4 只读兼容特性矩阵 | 仅在存在一项有明确理由可安全只读、但本构建不支持写的真实特性时，维护者才将其列入 `read_compatible`；当前候选 manifest 的该数组为空 | 使用明确声明可读兼容的构建/特性，在带索引、session 和 sidecar 的店上执行读请求与握手，比较全部真相层和派生文件；对 flush、重建、reconcile 等维护写逐项验证拒绝及无持久化副作用，并以写观测补足临时文件检查 | **当前无符合条件的测试对象**；manifest `features.read_compatible=[]`，不得用伪特性或临时改能力表声称通过；若未来特性经裁决加入，再执行 F4 |
| T09 · Windows/其他发布平台 | 发布维护者和对应平台 CI runner | 对实际承诺的平台/架构运行锁/强杀/原子替换/历史探测和消费者示例编译；记录 runner、目标、构建版本、命令与结果。维护者确认 Windows 不是本机迭代要求，只免除本机开发门槛，不免除发布平台声明所需证据 | **平台测试证据已齐（2026-11-24）**：Windows 开发机（x86_64-pc-windows-msvc，rustc/cargo 1.99.1）套件全绿含 28 项真实进程测试、历史源码样本拒写探测与消费者示例编译；GitHub windows-latest runner 同矩阵全绿（run 45332485516，提交 `348e93d`，`cargo check --workspace --all-targets` + `cargo test --workspace` + 消费者示例 dev/release，5m45s；前两轮暴露并修复 sidecar 管道 OVERLAPPED 栈逃逸与无界排干等待，见文末日志）。正式 Windows 发行包随首发 tag 由 CI `release-windows` 产出；安装器（MSI/NSIS）仍未实现（打包指南 §2 未收口项） |
| T09 · 首发授权与发布归档 | 维护者核对外部证据并明确授权具体提交与版本 | 授权记录应指明准确提交、`1.0.0` tag、发布资产、能力 manifest、已验证共店组合及回退方案；发布后将远端 tag/Release/资产哈希和 capability 摘要归档。候选接受不能替代此授权 | **未授权、未发布**；`validated_coexistence` 仍为空 |

尾行强杀回归新增 `reconcile_repairs_event_tail_left_by_killed_process`（有记录漂移时修复并回填）与 `reconcile_discards_partial_event_without_duplicate_when_store_is_stable`（无漂移时仅截断，不重复追加）。A6 强杀测试用 `run_bounded` 约束独立 CLI 的恢复、读取和续写，每个命令上限 10 秒。T07 新增迁移格式原子切换前后进程强杀、恢复重试/已迁移识别，并对完整备份副本递归比较全部文件字节。本轮新增 `find_rechecks_incompatible_disk_features_after_store_open`，验证旧 Store 句柄在磁盘特性改变后拒绝统一检索；统一特性登记入口并加空操作/非法与未支持特性拒绝净零、批量完整排序缺项测试；能力 JSON 文件、运行时格式/特性/版本与 API、MCP 源码指针一致性测试。最新 `cargo test --workspace`、`cargo check --workspace --all-targets`、mem-core/mem-index 消费者示例 check、release 生产目标检查（`cargo check --release --workspace --lib --bins --examples`）和 `git diff --check` 通过。当前增量再次运行 `cargo test -p mem-core --test foundation feature_registration` 与 `cargo check --workspace --all-targets` 通过。`cargo check --release --workspace --all-targets` 会因多个集成测试调用仅在 debug 构建提供的 fixture API（如 `save_checkpoint_test`、`append_test_event`、进程暂停钩子）失败；release 库/二进制/示例生产目标检查单独通过，未通过屏蔽行为测试来强行清除该限制。新增下游进程测试文件及本轮 find 回归已 rustfmt。`cargo fmt --all -- --check` 先前报告本分支和仓库中大量既有格式差异；未对全仓自动格式化。真实扩展特性登记/依赖数据中断矩阵、真实历史数据迁移/回退、正式历史二进制、发布标签保护及下游 SpaceTeam POC 未执行；Windows 运行环境按维护者决定不作为本机迭代门槛。T04/T08/T09 仍未关闭，方案实现尚未达到全部完成定义。

2026-11-22 追加直接读取兼容回归：`direct_agenda_and_pending_reads_recheck_compatibility` 与 `direct_event_state_reads_recheck_compatibility` 先以旧 Store 句柄触发失败，再为 `agenda::{load_plan,list_plans}`、`ops::list_pending_commits`、`event::{enumerate_md,enumerate_candidates,current_state_for_id,current_path_for_id}` 加前后磁盘检查；`current_state` 委托受检入口。`direct_event_enumeration_rechecks_compatibility` 先失败，再验证绕过日志读取的路径枚举拒绝；`record_scan_rechecks_compatibility_after_scanning` 通过可控交错证明扫描后置复核拒绝返回。`cargo test --workspace` 全量通过，`cargo check --workspace --all-targets` 与 `cargo check --release --workspace --lib --bins --examples` 通过，候选消费者示例签名检查 `cargo check -p mem-core --example embedded_contract`、release 示例检查和 `git diff --check` 通过。新建示例单文件 rustfmt 通过；全仓及部分既有格式未齐文件的 rustfmt 检查仍因原有格式差异失败，不全仓重排。Windows GNU/MSVC target 的 rustup 组件下载在八分钟内停滞于 14 MB 部分文件，已中止；Windows 编译/运行仍由 T09 外部验收。历史源码构建的 macOS release 样本已独立拒绝新格式并在真实旧格式样本迁移/备份恢复中通过；不等同正式发布二进制矩阵或部署历史数据。变更记录新增 manifest 指针和一致性断言，测试先失败后通过。T04/T08/T09 的缺项与外部责任保持未关闭。

2026-11-22 补齐嵌入契约消费者编译探针：mem-core 示例覆盖 API 表列出的 Store 初始化/迁移/特性登记/维护许可、WriteContext builders、记录 CRUD/link 和 EventLog 签名；mem-index 独立示例 `embedded_index_contract` 覆盖索引可写、持维护许可、只读打开签名。两个示例 dev 与 release 编译、单文件 rustfmt 和 `git diff --check` 通过。重跑 `cargo test --workspace` 全量通过时发现二者同名触发 Cargo 输出文件冲突 warning；已将 mem-index 示例改为唯一目标名，之后全工作区测试通过且未再报告冲突，release 库/二进制/示例检查及差异和格式检查通过。已将 T07 标为本机验收完成；正式发行二进制/真实部署店/受保护 tag 和 SpaceTeam POC 仍属 T09。维护者已选首个稳定契约候选版本 `1.0.0`；T04 仍依赖实际扩展特性，T08 仍待候选审定。

2026-11-22 维护者确认首个稳定契约候选版本可定为 `1.0.0`，并说明 Windows 运行环境不是本机迭代要求。工作区 Cargo 版本、运行时能力 manifest、API 文档、变更记录已同步到 `1.0.0` 候选（不创建 tag、不发布），运行时 manifest 一致性测试先发现旧文本断言再修正后通过。测试夹具进一步改为正式 `save_checkpoint` 维护许可入口；session watermark 测试改由正式 principal 记录写入产生事件。`cargo check --workspace --all-targets`、`cargo test --workspace`、`cargo check --release --workspace --lib --bins --examples` 通过；Cargo release all-targets 检查仍因多处集成测试依赖 debug-only fixture API 失败，未为通过检查而删减 release 测试覆盖。API 候选依然未审定/发布；T04 真实扩展特性、T09 历史发行物/真实迁移数据、tag 保护、SpaceTeam POC 仍未完成。GitHub API 核验到暂无 release 和 tag，仅有 `main` branch ruleset，没有 tag 目标；经典 tag protection 查询返回 404。

2026-11-22 维护者补充确认：首个稳定契约候选版本可直接定为 `1.0.0`；Windows 运行环境不作为本机迭代要求，保留为后续跨平台发布证据。复核 T04 的真实特性清单与调用路径后，确认当前只有 `event.restore_body/v1` 一项受支持且会被写入的扩展机制特性；既有二进制正文承载不是本轮新增特性，不能用来伪造第二项特性。故 T04 的不同特性 F/G 并发并集验收继续待第二个有实际机制语义的特性，不扩大设计范围造测试能力。此前发出的真实历史发行物/数据、tag 保护规则和 SpaceTeam POC/升级证据问题尚未获答；这些继续列为 T09 外部输入，不阻断本机 Windows 以外的开发迭代，但在提供前不能关闭外部验收。

2026-11-22 T08 补充 MCP 契约对照测试：先由运行时工具表与文档表精确比较测试发现候选文档漏列 `mem_trace`，补齐后定向测试和 `cargo test --workspace` 全量通过。候选 MCP 工具清单现由 `documented_candidate_mcp_tool_names_match_runtime_table` 防止漂移。只读消费者示例在临时店运行成功：由 `mem 1.0.0` 初始化后执行 `cargo run -p mem-core --example embedded_contract -- <store>`，输出 `format="spacememory/2"` 与店路径，退出码 0；运行前后全树文件 SHA-256 聚合均为 `a36579decb38812cd1be09f68a8ee3ae9b386dbfeb62e376a71936d30e778d8d`。消费者示例还以 dev/release 编译检查 `MemError::Conflict` 双方令牌读取和 `MemError`/`MigrationStatus` 通配分支；release 生产目标检查和 `git diff --check` 通过。
2026-11-22 T08 补充 1.0 输入形状冻结证据：消费者探针实际构造 `AddArgs` 的四个候选字段、穷尽匹配 `OpenMode::{Auto, ForceReadOnly}`，并锁定 `ops::restore_body` 签名；字段/变体增删或类型变更按 MAJOR 处理，已写入候选变更记录。`cargo check -p mem-core --example embedded_contract`、对应 release 示例检查、两份消费者示例单文件 rustfmt 与 `git diff --check` 通过。

2026-11-22 T04 增补真实机制特性 `event.restore_body/v1`：先添加正文恢复正向登记和旧格式迁移扫描历史事件补标回归，确认前者因特性清单为空失败、后者迁移后未登记失败，再实现同一独占门闩内的登记与依赖写，以及迁移特性提取；两项回归转绿。扩展真实进程测试，在 `feature_registered`、`record_published`、`event_appended` 三个持有独占屏障的窗口杀进程，分别验证仅标记、记录待回填、记录加事件三种保守状态；独立进程恢复并 reconcile 成功。另验证两个独立进程并发登记同一个受支持特性时幂等合并。定向 `r11_restore_body`（13 项）、能力 manifest 同步和两项多进程特性测试通过；消费者示例开发/release 编译、单文件格式检查及 `git diff --check` 通过。`required_features` 迁移只对既存 `restore_body` 事件补标；未使用正文恢复的旧店仍以空清单迁移。T04 仅余两个不同真实特性并发 F/G 并集测试，需第二个真实机制特性，不以测试假特性关闭。
2026-11-22 当前实现回归：`cargo test --workspace` 全量通过（含 24 项下游进程测试、13 项正文恢复测试）；`cargo check --workspace --all-targets`、`cargo check --release --workspace --lib --bins --examples`、mem-core 消费者示例 dev/release check、触及 Rust 文件 rustfmt check 与 `git diff --check` 通过。能力测试另固定声明 `event.restore_body/v1`，防止运行时表和 JSON 同步漂移为空。
2026-11-22 复核优化：旧店迁移从已校验的单次事件日志快照提取 `restore_body` 特性，不再为迁移再解析第二遍事件文件；迁移补标回归、能力清单测试、全目标检查与 `git diff --check` 通过。
2026-11-22 T08 消费者探针补齐 `Store::supported_features()` 与 `Store::read_compatible_features()` 函数签名赋值，API 清单同步列出；mem-core 示例开发/release 编译、单文件 rustfmt 和 `git diff --check` 通过。
2026-11-22 完成候选实现本机五轴静态复核并记录于 `docs/review/embedded-downstream-contract-implementation-review.md`：正确性、可读性/架构、安全与性能范围未发现阻断项；明确列出第二真实特性矩阵和 T09 外部证据不在该结论内。T08 仍待维护者审定，不将本机复核当作发布批准。

2026-11-22 补强 V1 持续验证：`.github/workflows/ci.yml` 在 Windows/macOS `test` job 中显式编译 `mem-core` 与 `mem-index` 下游消费者示例的 debug/release 配置，确保非 workspace target 的 API 签名探针随 CI 持续执行。四条对应 `cargo check` 均在本机通过，Ruby YAML 解析及 `git diff --check` 通过。此项补齐候选验证自动化，不代表 T08 维护者审定或 T09 发布验收完成。

2026-11-22 全量复核：重新运行 `cargo test --workspace`（全套单元、集成及文档测试，包括 24 项 `embedded_contract_process`、13 项 `r11_restore_body` 和 MCP 候选工具清单一致性），全部通过；`cargo check --workspace --all-targets` 通过。mem-core/mem-index 消费者示例的 debug/release 检查、CI YAML 解析和 `git diff --check` 也通过。GitHub 已安装仓库搜索 `SpaceTeam` 返回空列表；仓库文件仅有下游背景陈述，未发现下游可读 POC/升级证据。因此该检索不证明 SpaceTeam 不存在，只证明当前已连接仓库集合未提供可复跑目标；T09 仍需维护者提供实际仓库/revision 或验收产物。T08 的候选审定决定已单独请求，等待维护者回复。

2026-11-22 远端状态复核：`gh api repos/SpaceHuang/SpaceMemory/releases` 返回 0 条，`/tags` 返回空数组，`/rulesets` 仅返回 active `main` branch ruleset（没有 tag target），与先前快照一致；`branches/main/protection` 本次请求以 EOF 失败，不能据此证明分支或 tag 保护状态。未修改远端设置。T09 的 tag 不可变保护仍无证据，正式历史发行物/店数据及 SpaceTeam 验收仍待提供。

2026-11-22 T08 文档核对发现 API 契约签名区未逐字列出契约表承诺的初始化租约和索引维护入口；已补 `Store::{acquire_init_lock, create_under_init_lock, acquire_index_maintenance}` 及 `Index::{open_store, open_store_under_maintenance, open_store_readonly}` 签名。mem-core/mem-index 消费者示例 debug/release 编译探针和 `git diff --check` 全部通过，表格入口与显式签名现可交叉审阅。维护者候选审定仍待回复。

2026-11-22 T08 API 扩展性审计发现变更记录的通用承诺与源码不一致：`RestoredBody` 结果未标记 `non_exhaustive`，`RestoreSource` 也允许外部穷尽匹配。先将消费者示例改为直接构造结果/穷尽匹配，基线编译通过；加上两项 `non_exhaustive` 后该消费者探针按预期红灯，分别报 E0639（不可构造非穷尽结构）和 E0004（来源枚举缺通配分支）。随后保留该属性，消费者改为读取结果字段并以 `..` 解构、来源匹配加通配分支；API 文档和候选变更记录补充扩展规则。debug/release 消费者 check、能力 manifest 同步测试、正文恢复 13 项测试、触及文件 rustfmt check 与 `git diff --check` 均通过。该改动修复 1.0 候选扩展性缺口；候选最终审定仍待维护者。

2026-11-22 扩展性修复后重新执行 `cargo test --workspace`（含全部 24 项独立进程、正文恢复 13 项、各 crate 集成及文档测试）、`cargo check --workspace --all-targets`、`cargo check --release --workspace --lib --bins --examples`，全部通过；mem-core 消费者 debug/release check、能力 manifest 一致性和 MCP 工具表测试、`rustfmt --check`（触及两个 Rust 文件）及 `git diff --check` 均通过。

2026-11-22 T08 API 表格一致性复核又补出 `Store::create`：候选表承诺其为初始化租约的单步封装，但显式签名区和编译探针之前未列它。已补充 `Store::create(root: &Path) -> MemResult<()>` 签名与函数指针探针；`cargo check -p mem-core --example embedded_contract`、单文件 rustfmt check、`git diff --check` 通过。

2026-11-22 维护者接受当前分支的 `1.0.0` 契约候选，关闭 T08 候选审定。候选继续标记未发布，`validated_coexistence` 保持空数组；该接受不构成发布授权或 T09 验收。实现复核报告和候选变更记录已同步状态，能力 manifest 与运行时同步测试将重新核验。

2026-11-22 新增 `feature_dependent_write_waits_for_inflight_shared_writer_to_finish` 跨进程交错回归：A 在 `add` 已原子发布记录、尚未追加创建事件时持共享许可暂停；B 通过 `restore_body` 尝试登记 `event.restore_body/v1` 并写依赖记录/事件。B 进入调用后保持阻塞，暂停期间配置仍无该特性且事件文件字节未变；释放 A 后两进程均在 10 秒内成功，最终事件顺序为 Create→RestoreBody，正文与必需特性一致。定向验收通过。该项补齐 F2 的不同写许可交错证据；不同 F/G 特性并发并集仍需第二个真实机制特性。
2026-11-22 新增交错回归后重跑 `cargo test --workspace` 全量通过，其中 `embedded_contract_process` 27 项；`cargo check --workspace --all-targets`、`cargo check --release --workspace --lib --bins --examples`、`rustfmt --edition 2024 --check crates/mem/tests/embedded_contract_process.rs` 与 `git diff --check` 均通过。

2026-11-22 继续完成度审计：核对 T04/T09 的验收源条款与当前工作树后，没有发现可在本机诚实替代第二真实特性的能力。历史提交 `976a027d2e28013f1dcfed2b32c62266c553a9a6` 的 `Cargo.toml` 版本为 `0.1.1`，但它只是版本提交；当前 GitHub Releases 页面明确显示无正式 release，因此不将源码构建的历史样本升级为正式发行二进制证据。新增“T04 / T09 未关闭项证据清单”，逐项记录真实第二特性、正式历史二进制、真实部署迁移/回退、tag 保护、SpaceTeam POC/升级、Windows/其他发布平台及首发授权的责任方输入和关闭证据。维护者已接受 1.0.0 候选仍不等于发布授权；工作区没有执行远端配置、tag 或发布变更。文档 `git diff --check` 通过。

2026-11-22 T09 本机发布准备：在 Apple Silicon macOS 上执行 `DIST_DIR=dist/embedded-contract-candidate bash scripts/package-release-macos.sh`，产出 `dist/embedded-contract-candidate/mem-v1.0.0-aarch64-apple-darwin/` 与同名 `.tar.gz`。脚本的 release 构建及外置资产 init/list 冒烟通过，初始化结果为 `asset_source=external`、`ok=true`；包内 `mem --version` 为 `mem 1.0.0`，`diff -qr assets/builtin-types <package>/builtin-types` 无差异。tarball SHA-256 `dfd7b24d4a28a6bce58e6567664396c3e0adfcf1969bdf86d7a729f9bcc6f6c6`，二进制 SHA-256 `774ce202b971823f3f175bcc8ac6bb66d2e03215b803e34a85d69eda5eb4a7b1`。仅生成本机未签名候选包，未创建 tag/Release、未推送；不能替代固定提交的正式发布资产或其他目标平台证据。`git diff --check` 通过。

2026-11-22 T09 packaged read-path 复核：使用候选 tarball 中的 release `mem` 在自动清理的临时目录创建店，运行 packaged `list --limit 10` 前后比较全树文件数与路径/内容 SHA-256 聚合；前后均为 64 个文件，聚合值均为 `b0baf2504cbaa5d7a1324629d41781f329939255147a912a978a2f539b2805a6`，证明本次真实发布二进制读取冒烟未改变店内文件。该结果只覆盖本机候选包和 `list` 路径，不替代 §6/F4 全只读矩阵。`git diff --check` 通过。

2026-11-22 F4 复核：契约候选能力 JSON 的 `features.read_compatible` 当前为空数组；因此不存在可真实运行“本构建缺写能力、但明示可只读”的特性样本。将 F4 的依赖条件写入 T09 证据清单，要求未来只有在实际特性被明确声明只读兼容后才跑多组件零持久化矩阵，不伪造兼容声明。用 JSON parser 核验该字段确为空，`git diff --check` 通过。

2026-11-22 兼容拒写边界定向复验：`cargo test -p mem-core --test foundation missing_required_features_allow_only_explicit_readonly_and_block_existing_handle_writes`、`cargo test -p mem --test cli cli_reconcile_refuses_missing_feature_without_persisting_index_or_events`、`cargo test -p mem-index --test index_paths writable_store_index_open_refuses_missing_feature_before_creating_database` 各 1 项通过。分别证明多项缺失特性被拒、CLI reconcile 不持久化索引/事件、店级索引拒绝前不创建数据库。它们验证拒写面，不代表不存在的 read-compatible 特性矩阵已验收。

2026-11-22 补强旧格式只读打开零持久化断言：`legacy_numeric_format_opens_only_explicitly_readonly` 先保存整店相对路径与文件字节快照，再以 `ForceReadOnly` 打开数字格式 1 店，打开后整树快照完全一致；定向 `cargo test -p mem-core --test foundation legacy_numeric_format_opens_only_explicitly_readonly` 通过。此用例覆盖旧基础格式只读路径，不代表当前 `read_compatible=[]` 的缺写特性兼容读取矩阵。

2026-11-22 只读断言变更后刷新候选包来源指纹：将清单范围收紧为 `cargo build --release --bin mem` 的编译输入（workspace/crate manifest、crates `src` 与 build.rs、Cargo.lock、rust-toolchain.toml、打包脚本、builtin-types 与 Cargo 配置），排除不会进入该二进制的测试/示例源码。当前 78 项输入 SHA-256 聚合 `af7742d95bf197ff0a1a52182c60cb2e24c415f05206380bcb8d0acce4c1f884`；构建记录摘要 `11cb78d93e3939e8da77b469d364be544b7f4f05e8e1af51b888a9d29d8bc2a3`。逐文件输入哈希和已有 tarball/二进制哈希重新核对通过；唯一源代码变更为集成测试断言，因此不需要重建 release 二进制。

2026-11-22 历史样本复现与 F5 补验：从 Git 对象 `8026d0d9f2fdb0252bff5405a982e9927299a418` 使用 `git archive` 导出干净源码并运行 `cargo build --release --bin mem`，精确得到 `mem 0.1.1` 二进制 SHA-256 `43226de5d577bb084bee05dee7dff924df35c17eb0168415a0f9ce7e2d429437`；留存副本 `dist/embedded-contract-candidate/historical-mem-0.1.1-8026d0d9`。用该程序对候选 `spacememory/2` 店执行 `list --limit 5`，退出码 1，错误为配置中的字符串版本不能反序列化为历史 u32（`invalid type: string "spacememory/2", expected u32`）；打开前后均 64 文件且路径/内容 SHA-256 映射相同，未创建临时或持久文件。该复验恢复了可执行旧样本，但仍是历史源码构建，不等同正式发行资产矩阵。

2026-11-22 T07 迁移回退定向复验：`cargo test -p mem-core --test foundation format_migration_is_atomic_idempotent_and_preserves_unknown_config` 与 `cargo test -p mem --test embedded_contract_process migration_process_death_before_and_after_atomic_switch_is_recoverable` 均通过；分别覆盖完整副本逐文件字节恢复/旧格式只读读取，以及原子切换前后强杀后的迁移重试和幂等识别。

2026-11-22 只读快照与历史迁移证据加入后，全量重跑 `cargo test --workspace`（包括 27 项 `embedded_contract_process`）通过；`cargo check --workspace --all-targets`、`cargo check --release --workspace --lib --bins --examples`、`rustfmt --edition 2024 --check crates/mem-core/tests/foundation.rs` 和 `git diff --check` 均通过。

2026-11-22 维护者回复先前的剩余验收材料请求：“目前没有第二特性或外部验收材料”。据此更新 T04/T09 未关闭项表，明确当前缺失事实；这不撤销任务、不代表候选获发布授权，也不关闭任何验收门槛。

2026-11-22 维护者选择 T04 方案 2：保持首个 `1.0.0` 候选的真实产品特性范围，不为了跨特性测试引入虚构特性；不同特性 F/G 并发并集在当前仅有一项真实特性时标为 N/A。T04 以已有的同一真实特性跨进程并发登记幂等测试、依赖写等待共享写者完成的许可交错测试，以及其余登记/恢复/迁移测试验收完成。未来加入第二项真实特性时，不需重新裁决范围；自动要求补做 F/G 并发并集、依赖数据/事件可解释性和幂等验收。理由是验收应覆盖真实能力语义，避免为通过测试扩张产品面；护栏是第二项特性合入前必须补齐相应真实双进程测试，并重新审查能力 manifest、迁移补标和兼容性表。T09 外部验收仍未关闭。

2026-11-22 维护者裁决 T09 的 SpaceTeam POC/升级验收不作为首个 `1.0.0` 发布前置条件：下游产品必须针对可用的正式版本做真实升级，要求其在发布前验收未发布候选会阻止下游按正常产品版本验收，且并非本仓库可独立完成。首发前仍要求本仓库适用的平台/历史兼容证据、发布 tag 保护及明确发布授权；SpaceTeam 证据移到正式发布后，届时固定下游 revision 并验收正式资产，完成前不声明下游通过或把组合加入 `validated_coexistence`。Windows/其他平台仅在作出相应支持声明时要求平台验收。理由是把集成验收放在可供下游验证的产品发布之后；护栏是区分“已发布”与“已通过下游验收”，并保留后续兼容声明门槛。

2026-11-22 维护者裁决真实部署店迁移/回退演练属于发布后运维验收：本任务的工作环境是开发仓库，并无自然存在的生产店数据；把经授权真实/脱敏店作为首发硬门槛，等于要求开发者先制造或索取并不存在的生产数据。首发前改以已完成的本机隔离合成店迁移/回退、完整备份逐文件验证、迁移切换前后进程强杀恢复、历史源码样本和故障注入作为可复跑证据；真实环境投入运行且店数据责任人提供授权副本后，再按正式版本执行真实店演练。理由是开发阶段使用可控可复现数据验收机制，生产副本只在真实环境产生后做运行验证；护栏是不得把合成证据描述成真实店验收，发布后演练仍须记录迁前/后摘要、迁移、恢复结果和责任授权。

2026-11-22 候选包来源追溯补强：检查构建输入时发现仓库 `rust-toolchain.toml` 与 macOS 打包脚本也须纳入来源指纹；更新清单后覆盖 112 个输入文件（workspace crates、Cargo manifest/lock、工具链声明、打包脚本及 builtin-types）。生成 `dist/embedded-contract-candidate/candidate-build-inputs.sha256`（摘要 `45bbf2db908c4e81eca4146ac73c868c69b5eb35451be4d45ccac72679e54058`）和 `candidate-build-provenance.txt`（摘要 `5ba7e3c5e362259d7022588ad35fa2b4dfcad031d5c3cdc35bbb4d4551d01922`），包含 Git HEAD `a82ab5ffb37e8eb3adc93fc7720901ae8db59e12`、Apple Silicon target、Cargo/rustc 版本及与既有产物一致的二进制/归档哈希。逐文件重算输入摘要和产物哈希通过。该清单让未提交工作树中的本机包可核对输入，但不替代固定提交或签名构建证明。

2026-11-23 契约实现评审阻断修复（提交 `b7bce80`、`4fb43e0`、`7d687de`、`7ab8733`、`0afc989`、`8afd72c`；本轮评审往返未单独归档，以修复提交与随附测试为记录）：事件 `current_state_for_id` 改为委托读取前后复核的受检入口，`*_unchecked` 变体收窄为 crate 私有；`Index::save_checkpoint` 收敛到正式维护许可入口。索引侧：可写索引关闭不得在快照锁外发布检查点或改库（`writable_index_close_never_checkpoints_or_changes_database_outside_snapshot_lock`）；重建失败在写锁内以热日志回滚且可重试，`open_proved` 不触碰热回滚日志、维护可恢复（子进程遗留热日志加两项恢复测试）。事件侧：尾行撕裂修复走 `repair_torn_tail_with`，截断遇 IO 错误保留完整前缀且尾行不动；迁移预检拒绝缺失历史记录、重复序号与损坏目标配置。配置侧：以 RawValue 定点改写保留任意精度未知数字，合法 JSON 对象在配置打开与记录写入中原样保留、不改写类型；握手只报告 pending 变更、不运行维护、不消费变更。[API 契约](embedded-downstream-contract-api.md) 补充 `sidecar::reconcile_under_maintenance` 返回语义行。

2026-11-23/24 Windows RDC 新鲜度线与本文的交叉：主设计开放问题39 记录维护者实施裁决（2026-11-24，安全直读）——`ReadDirectoryChangesW` 异步交付目录变化，独立探测事件不能证明更早变化已交付，故 `backend: "rdc"` 只触发后台维护，握手 `undigested: null`，查询直读，RDC 不向检查点发布器提供新鲜度证明（提交 `623fa48`、`23c1a49`、`c1b7940`、`f945b4d`；护栏见 `crates/mem-core/src/platform/README.md`）。其中 `2633725` 补跨版本拒绝：旧 sidecar 的 RDC 零计数应答不得授权新 CLI/MCP 的 indexed 快路径，两前端共用的 `consult_with_hello` 拒绝 `backend=rdc`，即使只升级一端也保持直读；`4514e74` 同步 Windows 维护竞态断言。该裁决把本文 §4.2「不能证明索引新鲜且兼容时走真相层直读」在 Windows RDC 后端落为机制行为，并保住 C2.4 的旧 sidecar 共店组合语义。

2026-11-24 Windows 本机（x86_64-pc-windows-msvc，rustc/cargo 1.99.1）首次产出 T09 Windows 开发机证据，基线 `4514e74` 加一处测试断言修正：`cargo test --workspace` 全绿（21 个测试目标，含 28 项 `embedded_contract_process` 真实进程测试——A1–A7 的锁、强杀、原子替换迁移、CAS 竞争、唯一键幂等、事件序列与 Git pending 在 Windows 本机复现）；mem-core/mem-index 消费者示例 dev/release 编译、`cargo check --workspace --all-targets`、触及文件 rustfmt 与 `git diff --check` 通过。首轮套件暴露 `fastpath::r11_windows_async_rdc_keeps_queries_direct_and_correct` 时序脆弱断言：其取记录 ID 前 8 位大写前缀并断言恰好命中一条，而 UUIDv7 前 8 位仅覆盖约 65.5 秒时间窗，同店数秒内先后创建的记录必然共享前缀；直读按既定 fold 前缀语义全量命中是正确行为（单源 `search.rs`），修正为取完整毫秒时间戳段 12 位后断言确定性成立，产品代码零改动。验证期间工作树另含一轮全仓 rustfmt 格式化差异（46 文件，语义无关，非本任务产出），上述绿结果覆盖该状态。

2026-11-24 历史源码样本 Windows 重建与拒写探测（T09 历史程序项的本机证据）：从 `8026d0d9` 以 `git archive` 干净导出至 `dist/embedded-contract-candidate/historical-mem-0.1.1-8026d0d9-win/`，`cargo build --release --bin mem` 得 `mem 0.1.1`，SHA-256 `0147280d8e8af2eeb59c3f507c814f28249108d77a40c63613d25bf8e2432135`；以当前 `mem 1.0.0`（debug 构建）初始化 `spacememory/2` 店并写入一条记录后，旧程序 `list --limit 5` 退出码 1，错误为配置反序列化早期失败（`invalid type: string "spacememory/2", expected u32`），打开前后全店 61 文件路径与内容聚合 SHA-256 完全一致（零临时、零持久写入）。仓库 `dist/` 下另有 2026-11-10 的 `mem-v0.1.1-x86_64-pc-windows-msvc` 本机打包（无提交号与哈希记录），不作为历史样本证据。该样本仍为历史源码构建，Windows 正式发行包、GitHub runner 矩阵与安装器证据仍未关闭；`validated_coexistence` 保持空。

2026-11-24 T09 本机 Windows 候选包：执行 `bash scripts/package-release.sh`（与 CI `release-windows` job 同一脚本），产出 `dist/mem-v1.0.0-x86_64-pc-windows-msvc/` 与同名 `.zip`。release 构建及 exe 旁播种冒烟通过（`asset_source=external`、init/list 可用），包内 `mem --version` 为 `mem 1.0.0`，`diff -qr assets/builtin-types` 与包内 `builtin-types` 无差异；zip SHA-256 `1a8469f890c86aaa56972a19de899224d765fd1d42131e382a263763121ab42e`，包内 `mem.exe` SHA-256 `c70a7117bdc31a6e25aee00897c917f8c5d19adb23c1900b0bfe301cb252901b`。packaged 读路径复核：包内 exe 初始化临时店（63 文件）后执行 `list --limit 10`，前后全树聚合 SHA-256 均为 `63087c0432107ed6906f2899cc4f2679469cd2b56b145fe501e807801f76dccc`，零写盘。来源指纹 `dist/embedded-contract-candidate/candidate-build-inputs-windows.sha256` 覆盖 78 项编译输入（摘要 `af23cef5bbd16160cf8178c869b251ead3821e977d14afcfc4248838d0bc7b91`）；`candidate-build-provenance-windows.txt` 记录 Git HEAD `679e776f8c719af7cccd49769ba784c53c77584c`、工具链与产物哈希（摘要 `1fff0de1d8e95dfb4cad880852b43f70c72a2c9fb44c78491474e2090a3e52ed`）。这是未签名本机候选，不代替 CI 从固定发布提交构建的正式资产；MSI/NSIS 安装器仍未实现（打包指南 §2 未收口项）。

2026-11-24 Windows runner 证据与 sidecar 管道修复（三轮 CI 收敛）：首轮 dispatch（run 45307257493）macOS runner test job 全绿，Windows runner 在 `fastpath::fastpath_end_to_end_via_production_binary` 失败——sidecar 命名管道握手 2 秒预算在冷启动 runner 上不足；提交 `e63ea71` 放宽预算（握手轮询 2s→10s、indexed 快路径重试 10→20 次）并加进程提前退出诊断。二轮（run 45308333189）暴露真因：serve 子进程以 `ExitStatus(3221225477)`（0xC0000005，访问违例）崩溃——`PipeServer::create` 以栈局部 OVERLAPPED 承载挂起的异步 `ConnectNamedPipe`，create 返回即释放栈帧，而内核在客户端真正连上的任意未来时刻仍写该结构完成状态，落在已释放栈上（环境相关布局：开发机静默、runner 崩溃）；`try_serve_one` 读/写超时路径另有不收割挂起 I/O 即关句柄的同类问题。提交 `773c78e` 修复：连接 OVERLAPPED 改为 Box 字段、完成即收割、放弃挂起读/写前 `CancelIoEx`+收割（`abandon_io`，与 `cancel_and_drain_rdc` 同纪律）、Drop 收割挂起连接；配三条回归测试（多客户端重建循环 / 沉默客户端读超时重建 / 挂起连接下 Drop），本机 lib 54 项三轮全绿、全工作区通过。三轮（run 45310735000）Windows `cargo test --workspace` 静默卡 2 小时后取消——无界阻塞排干（`GetOverlappedResult(bWait=1)`）在异常等待源下可永久挂起；提交 `7b37f2a` 把排干改为有界（完成事件至多等 5 秒，超时由句柄关闭兜底取消），`348e93d` 给 CI test job 加 30 分钟上限把挂死变成可见失败。四轮（run 45332485516）双平台全绿：windows-latest 5m45s、macos-latest 1m25s，`cargo check --workspace --all-targets`、`cargo test --workspace`（含 28 项 `embedded_contract_process`）、mem-core/mem-index 消费者示例 dev/release 编译全部通过。至此 Windows 平台测试证据为开发机 + GitHub runner 两层；正式发行包随首发 tag 由 `release-windows` 产出。runner 记录：workflow_dispatch、分支 `codex/safer-agent-memory-setup`、各 run 号与 job 时长如上；`validated_coexistence` 保持空，该证据不构成发布授权。

2026-11-24 T09 tag 保护规则应用：经维护者批准，以仓库 Administration 权限调用 `POST /repos/SpaceHuang/SpaceMemory/rulesets` 应用[发布 tag 保护规则草案](release-tag-ruleset-proposal.md)中的 JSON。创建响应与 `GET /rulesets/27981120` 独立读回逐项一致：ID `27981120`、名称 "Protect release version tags"、target=tag、enforcement=active、include `refs/tags/v[0-9]*`／exclude 空、限制项 update+deletion、`bypass_actors: []`；创建响应另含 `current_user_can_bypass: "never"`——管理员亦无日常绕过通道。远端 rulesets 现共两条：`27981120`（tag）与既有 `27179551`（main branch），互不影响。草案验收步骤 4（一次性测试仓库中验证首次创建成功、后续更新/删除被拒）未执行——需创建临时仓库，留待维护者授权；正式版本 tag 的创建不受此规则限制，创建后须再次读回规则与 tag 指向归档（草案步骤 6）。执行账号 SpaceHuang，2026-11-24。

2026-11-24 外部代码评审（[safer-agent-memory-setup 代码评审](../review/safer-agent-memory-setup-code-review.md)，HEAD `7913e49`）报出两项阻断，均已修复（处置详情见该评审修复记录）：B1——`7b37f2a` 的有界排干在等待超时后忽略 `ERROR_IO_INCOMPLETE` 并释放栈上 OVERLAPPED/缓冲，重新打开用后释放窗口；处置为管道 I/O 所有权重构（提交 `971b6e2`）：全部 OVERLAPPED 与读写缓冲由句柄持有，放弃仅置未确认标记并在下次复用前有界收割，仍 INCOMPLETE 时整槽泄漏给进程生命周期（唯一既不用后释放也不无限阻塞的终点），Drop 同纪律，配写侧真实放弃与注入式泄漏回归。B2——索引查询与 CLI brief 快路径只在构造期复核兼容性，长活句柄在新特性登记后仍可能输出 indexed 结果；处置为返回边界复核（提交 `f0a04b2`）：新增 `read_compatible_missing_features_at` 无句柄单源形态，`Index` 按 store_root 在返回行之前重读磁盘配置，两类缺项一律拒绝并经调用方既有兜底转真相层直读；配只读句柄存续期登记后三类查询拒绝回归与 brief 端到端拒绝回归。修复后 `cargo check --workspace --all-targets` 与 `cargo test --workspace` 全量通过（Windows 开发机，含 3 项新回归）；CI 双平台复验随本次推送执行。

CI 复验取证：首跑（run 45413814333）Windows job 挂 27 分钟后被 30 分钟上限终止——job 日志显示 lib 测试二进制 54/56 项已正常打印完成，未打印的两项（写放弃、注入泄漏回归）代码路径中全部等待均有 ≤5 秒超时上限，逻辑上不可能静默挂 27 分钟；该「运行中全局静默直至超时」的签名与 run 3 一致，且两者之间同一代码状态曾全绿（run 5），判读为 GitHub windows-latest runner agent 偶发僵死（输出管道停止消费 → 测试进程 stdout 写阻塞 → 二进制整体冻结），非产品代码问题。重试 run 45420241205 双平台全绿（windows-latest 4m56s / macos-latest 2m51s），证实环境偶发；`timeout-minutes: 30` 上限把此类僵死的代价圈定在可见失败内，不阻塞后续迭代。
