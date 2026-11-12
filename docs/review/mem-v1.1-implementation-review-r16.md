# 第十六轮实施复审——R15 修复验证

- 日期：2026-11-12。
- 对象：`main`，HEAD `2cc5ac2` 加当前未提交修复；20 个已跟踪文件发生修改。R15 报告保持原样。
- 环境：macOS / Darwin arm64；按 `code-review-and-quality` 技能先审新增测试，再审修复差异，并独立重放原场景与相邻边界。
- 范围：R15 的 8 项 P1 和 1 项 P2；本轮是定向复审，不是再次逐行评审整个项目。

## 结论与待批准清单

**Request changes：保留 3 项 P1 阻断，另有 1 项 P2 必改问题。**

当前源码已能在 macOS 上构建，工作区测试 **289 项通过、0 失败**。原来的删除副本覆盖、锁提前释放、日志尾行恢复、NUL 漏召回和只读索引创建伴随文件等场景均取得修复证据。但路径校验、引用创建和漂移扫描仍有未封住的分支；自动化全绿不足以关闭这三项。

| 本轮编号 | 级别 | 结论 | 对应前轮 |
|---|---|---|---|
| B16-1 | P1 | `fix` 吞掉路径拒绝；符号链接也能越出店目录读写 | B15-6 未关闭 |
| B16-2 | P1 | add 在取得共享门闩之前校验引用，删除可插入校验与发布之间 | B15-8 未关闭 |
| B16-3 | P1 | 回填已核对身份，但 current_drift 仍把 B 的状态归给已删除的 A | B15-5 部分修复 |
| Q16-1 | P2 / Required | merge 的目标冲突发现过晚，报错前已改内容、引用与事件 | B15-2 修复的相邻失败路径 |

**本轮请求确认的范围**：按以下证据继续处理三个阻断及 merge 的确定性失败副作用；达到关闭条件后再复审。

**明确不包含**：批准发布/合并、变更需求或设计、修改旧评审结论、宣称 Windows 实机验收完成。

## 上轮逐项复验

| R15 项 | 本轮状态 | 证据摘要 |
|---|---|---|
| B15-1 macOS 编译 | **关闭** | 当前源码 check 零警告，全量 test 成功；不再需要临时编译补丁 |
| B15-2 tombstone 覆盖 | **原数据丢失关闭** | 同键 A 删除、B 重建后再删除，返回 AlreadyExists，A 的 attic 与 B 的 records 都保留；merge 也保住旧 attic，但失败副作用见 Q16-1 |
| B15-3 锁生命周期 | **关闭** | 两个共享 guard 只释放一个，以及同进程记录锁等待超时后，真实子进程都仍不能取得冲突锁 |
| B15-4 日志尾行 | **关闭** | 完整 JSON 无 LF 后追加得到两条有效事件；截断 UTF-8 尾部识别、追加恢复及中部损坏拒绝均通过 |
| B15-5 身份混淆 | **部分修复** | reconcile 不再把 B 回填到 A；但同一稳定状态经 diff 仍返回 A 携 B 令牌，见 B16-3 |
| B15-6 路径越界 | **未关闭** | show/update 的直接 `..` 输入已拒绝；fix 仍越界写，目录符号链接也能绕过，见 B16-1 |
| B15-7 NUL 漏召回 | **关闭** | 三个内容域、NUL 与新增转义字符边界、查询含 NUL、多词/否定及分页共 143 组 direct/indexed 对账通过 |
| B15-8 删除引用竞态 | **未关闭** | 删除已持独占门闩，但 add 在进入共享门闩前的校验结果仍可过期，见 B16-2 |
| Q15-1 只读索引副作用 | **关闭** | 读取前/中/后的目录文件集合与字节不变；写者等待读快照释放；最新代际仅在崩溃残留 WAL 时，epoch 门禁正确转直读 |

## B16-1 [P1] 所有记录路径入口必须共享同一边界校验

**位置**：`crates/mem-core/src/ops.rs:150-159`、`:1106-1109`；读取入口 `:105-119`。

本轮新增校验能拒绝包含父目录成分的直接路径，但存在两条绕行路径。

**分支一：fix 把校验失败当成记录损坏，重新采用原始路径。**

`get` 返回 BadArgument 后，`fix_record` 的 `Err(_)` 分支只看输入是否以 records/attic 开头，随即恢复使用原输入。之后的记录锁、读取和原子写全部落在未经校验的路径。

真实 CLI 复现：在店的兄弟位置放一份合法 memory 记录 `outside.md`，用非确定性的 JSON 排版，使 fix 是否实际写入可观测；按该文件的版本和 SHA-256 计算令牌。

```sh
mem --store "$STORE" show records/../../outside.md
# 退出 1：直接读取已被修复后的校验拒绝

mem --store "$STORE" fix records/../../outside.md --expect "$TOKEN"
# 退出 0：仍返回 rel=records/../../outside.md
```

实际店外记录被重新排版，字节改变；店外还产生 `outside.md.lock`。这个复现不需要符号链接。

**分支二：合法词法路径中的目录符号链接指向店外。**

建立 `records/escape -> 店外目录`，店外目录放合法记录 `outside.md`。输入 `records/escape/outside.md` 全部由 Normal 路径成分组成，能通过新增校验，但读取和写入跟随链接。

```text
show records/escape/outside.md: exit=0，返回店外载荷
update 同一路径: exit=0
店外文件含 changed-outside: true
```

因此 B15-6 的“店内入口不能越出目录”仍未成立。这不表示能绕过 OS 权限，也不表示任意非记录文件都能通过解析；影响是合法记录文件可在所选店目录外被受管工具读写。

**修复方向**：在定位记录路径的公共边界完成校验，损坏记录的 fix 通道也必须经过同样校验，不能笼统吞掉 BadArgument；同时确认解析后的目标仍位于所选子树内，并覆盖目录/文件符号链接及不存在目标的父目录。按 ID/别名扫描得到的路径也应满足同一约束。

**关闭条件**：CLI/MCP 的路径读取与写入、fix 损坏修复、链接路径均一致拒绝越界；店外记录、店外锁文件不变；合法店内损坏记录仍可修复。补接口级回归，不能只测 `get`。

下钻：R15 B15-6；需求 R2/R6；设计 D1。

## B16-2 [P1] 引用存在性校验必须处于门闩保护范围内

**位置**：`crates/mem-core/src/ops.rs:329`（引用校验）、`:353`（随后才获取共享门闩）；删除独占门闩在 `:637`。

把删除改为独占门闩的方向正确，但 add 已经在门闩外读取引用目标是否存在。它可能完成校验、等待门闩，在目标删除后不复验便发布记录。

本轮核心探针直接链接当前工作区，未改业务实现，构造以下合法交错：

1. 主线程持独占门闩。
2. 另一线程开始创建 memory，`topic` 引用一个仍存活的目标；校验通过后阻塞于共享门闩。
3. 主线程在已有独占门闩内调用维护用 `delete_locked(target, force=false)`，删除成功。
4. 释放门闩，创建继续，也成功。

```text
add-after-delete success=true
delete_warnings=[]
dependent refs=[topic -> target]
target location=attic/topic/race-target.md
```

同样的检查先于门闩窗口也存在于普通删除与 add 的争用中。使用门闩内删除入口只是让探针能控制交错，不是绕开删除的保护前提。

新增 `r15_nonforce_delete_serializes_with_new_reference_creation` 在启动删除前已经完成引用创建，只证明“依赖先存在时删除会拒绝”，没有检查“创建先校验、删除先完成、创建后发布”的反向交错。

**修复方向**：让 add 从所有真相层存在性校验之前开始持共享门闩，或在取得门闩后复验；校验和实际发布必须共享同一个保护区间。principal 等存在性校验也不要复用过期快照。

**关闭条件**：增加可控同步点覆盖上述反向交错。要么创建先完整发布、删除因引用被拒绝，要么删除先完成、创建明确拒绝缺失目标；不能双方成功后留下悬空引用。

下钻：R15 B15-8；需求 R3.4/R11.3；设计 D17、红线20。

## B16-3 [P1] 漂移扫描应复用身份感知的状态解析

**位置**：`crates/mem-core/src/drift.rs:59-68`；`crates/mem-core/src/event.rs:117-121` 的无 ID 包装仍被该读路径调用。

回填已改为核对候选 ID；漂移扫描没有接入。它仍按路径优先读取 records 的状态，实际路径也只按文件存在性选择，导致同键新旧身份分裂。

**真实 CLI 复现**：

1. 创建 `topic(slug=reused)` 得 A，删除 A 到 attic。
2. 同 slug 创建另一条记录 B，B 存活于原 records 路径。
3. 执行 `diff --since 999999`，再执行 reconcile，重复 diff。

```text
reconcile.backfill_appended = 0
reconcile.skipped_stable = 3
diff.current_drift[0].id = A
diff.current_drift[0].last_path = attic/topic/reused.md
diff.current_drift[0].cur_path = records/topic/reused.md
diff.current_drift[0].token = B 的完整令牌
diff.current_drift[0].note = 位置变化（backfill 待执行）
```

对账前后均如此。A 实际仍在 attic，既没有恢复也没有修改；reconcile 已判断状态稳定，却不能消除 diff 中这条假漂移。新建的 `current_state_for_id` 单元测试和稳定回填测试通过，未覆盖这个读侧消费者。

**修复方向**：让回填与漂移共用按 ID 解析“实际路径 + 状态三元组”的单一函数，避免另写一份路径存在性分支；消除不带 ID 的状态查询在逐身份比较场景中的使用。

**关闭条件**：同键 A 在 attic、B 在 records 时，连续 reconcile 均无新增事件，前后 diff 的 current_drift 都为空；再分别修改 A/B 验证漂移只归给对应 ID，并携带对应摘要与位置。

下钻：R15 B15-5；设计 D12、红线24/26。

## Q16-1 [P2 / Required] merge 的既有目标冲突应在首次修改前检出

**位置**：`crates/mem-core/src/ops.rs:1018-1038`；第三方引用重挂更早发生。

`rename_noreplace` 已正确阻止覆盖旧 attic。但 merge 到最后才发现确定存在的冲突，此前已把第三方引用从 dup 改到 canonical、改写 canonical 并追加事件。调用方收到失败，内容却已经部分合并。

**真实 CLI 复现**：A 删除到 attic；同键重建 dup；另建 canonical 和一条引用 dup 的 memory；执行 merge。结果：

```text
merge: exit=1，File exists
canonical.version: 1 -> 2
dependent.payload.topic: dup -> canonical
events.jsonl: 新增 2 条
```

旧 attic 和 dup 都仍在，故原来的 tombstone 数据丢失不应重新打开；本项针对可预见冲突导致的部分写入。新增 merge 回归只断言旧 attic 不变、dup 仍存在，没有检查 canonical、依赖者和事件是否已改变。

**修复方向与关闭条件**：在独占门闩内、第一笔记录修改前检查既有 tombstone 冲突，同时保留末尾不可覆盖的原子操作作为并发/外部写入兜底；扩展失败断言，既有目标冲突时 canonical、第三方记录及事件日志均保持不变。此建议不要求为所有不可预见 IO 故障引入全库事务。

## 验证记录与边界

1. 当前工作区 `cargo check --workspace --all-targets`：通过，零警告。
2. `cargo test --workspace --no-fail-fast`：退出 0，**289 项通过 / 0 失败 / 0 ignored**。按测试输出逐组汇总；编译条件排除的 Windows 专属用例不计入 289，也不能算作本机已验证。
3. 独立核心探针直接依赖当前工作区，无上轮兼容补丁；锁、尾行、删除保护取得关闭证据，反向引用交错与身份漂移取得失败证据。
4. CLI 探针使用当前 `target/debug/mem`，在系统临时目录建店；复现 fix 越界、符号链接越界、稳定状态假漂移和 merge 失败副作用。
5. 索引独立探针调用当前库，三域各含 NUL 与 U+E000/U+E001/U+E002；11 个查询 × 13 个 offset，limit=2，共 143 组分页的 ID、truncated、resume 与直读一致；只读索引原目录快照场景也通过。另以子进程提交下一代 epoch 后直接退出，留下非空 WAL；检查点指向新代时，`open_proved` 因主库 epoch 不匹配回退 Direct，读取前后连同 WAL/SHM 在内的文件集合与内容均不变。
6. `git diff --check`：通过。本轮不修改实现和测试，不修改 R15；仅新增本复审报告，不 commit、不 push。

## 自曝三处

1. **平台边界**：本轮在 macOS 验证 flock 与排他改名；Windows 的对应原语和真实 RDC 未实测。如果 Windows CI/实机失败，应补充平台结果，不能由本机 289 项推定通过。
2. **并发证据边界**：反向交错探针用已持有的独占门闩和短等待安排 add 阻塞，已观察到双方成功后的坏状态；正式回归仍应加入可控同步点。若共享门闩获取后已有本轮遗漏的存在性复验，且能阻止该结果，可推翻 B16-2；当前源码和实测均未见这种保护。
3. **索引证明边界**：NUL 对账模拟合法 Hello 以进入真实索引执行器，本机没有实际 FSEvents/RDC 证明源；只读连接及崩溃残留 WAL 的代际回退已经验证。若能构造主库通过代际门禁而返回旧候选的场景，应重开该边界；本轮没有观察到，不从 immutable 忽略 WAL 本身推定读结果错误。
