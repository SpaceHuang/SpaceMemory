# macOS FSEvents 实施方案评审

- 日期：2026-11-12。
- 对象：`docs/develop/macos-fsevents-implementation-plan.md`（当前未提交版本）。
- 方法：核对方案、文档写作规则、总体设计的新鲜性约束及当前工作区的 sidecar/freshness/macOS 实现；查阅 Apple API 与 Watchman 官方项目文档。
- 范围：判断方案是否足以安全进入实施；不批准发布、合并、改变新鲜性规则或关闭开放问题39。本轮不修改方案、实现及既有评审记录。

## 结论

**Request changes：2 项 P1 阻断，暂不批准以本文机制启用 macOS 索引快路径。**

FSEvents 可以作为触发维护的变化来源，但“可收到通知”与“查询时能够证明此前变化已全部送达”是两个不同能力。本文用同步刷新补齐后者，存在直接相关的反例证据。stat 对同尺寸、保 mtime 改写无辨别能力，无法补偿该缺口。

| 编号 | 级别 | 位置 | 问题 |
|---|---|---|---|
| B1 | P1 / 阻断 | §3.1、§3.2、§3.5；原文第55、65、91行 | 同步刷新不足以作为严格新鲜性证明，可能放行旧索引 |
| B2 | P1 / 阻断 | §3.1、§3.3、§7；原文第47–55、74、136行 | 根目录变化降级缺少必要的监听配置或等价身份检查 |

## B1：不能以 FlushSync 返回且计数为零证明此前修改已经全部到达

**问题与证据。** 本文把“同步刷新之后没有待处理事件”作为返回 `Some(0)` 并开放索引的充分证据。Apple 的 [FSEventStreamFlushSync 文档](https://developer.apple.com/documentation/coreservices/1445629-fseventstreamflushsync)确实描述了同步交付语义，因此作者的依据并非凭空假设。但 Watchman 官方项目专门记录了与此用法直接相关的限制：高负载下，较早文件修改的 FSEvents 通知可能晚于 cookie 通知及 FlushSync 返回；项目因此指出不能依赖 FSEvents 做查询同步。证据见 [Watchman Query Synchronization 源文档的 macOS FSEvents 限制](https://github.com/facebook/watchman/blob/main/website/docs/cookies.md#limitation-macos-fsevents)。这是上游项目的一手观察及其报告的 Apple 建议，不是本轮在当前机器上复现的结果，也不能将其表述为 Apple 已公开发布的规范变更。

**可能返回旧答案的路径。** 已发布检查点对应内容 A；外部程序在查询开始前把文件改成等长内容 B 并恢复 mtime；通知尚未送到回调；握手调用 FlushSync 后仍读到零；stat 与检查点相同，当前 `consult_with_hello` 允许 indexed；索引仍含 A。这里不需要 UserDropped/KernelDropped，也不需要通知永久丢失；仅通知延迟就足以让第一次查询错误。因此“缺口标志全部失效”及增加四轮收敛都不能覆盖这个分支。

**修订方向。** 在方案评审层正面处理 Apple API 描述与上游实测限制之间的冲突。可以保留 FSEvents 作为维护提示，但在缺乏独立证明机制时继续直读；若仍要开放快路径，需给出覆盖查询前已完成修改的独立机制及边界依据。不要把固定等待、重复刷新、cookie 或压测未失败当作绝对证明；Watchman 已明确记录 cookie 加 FlushSync 仍有反例。Endpoint Security 只是上游提到的候选方向，其可行性、权限和分发约束尚未经本轮评估，不构成推荐直接采用。

**关闭条件。** 修订方案必须明确区分“触发维护”和“允许使用索引”的证据；补可控延迟通知测试：等长保 mtime 改写已完成，刷新返回时暂不交付通知，首次查询仍必须返回 B。若保留快路径，测试之外还必须提供其同步保证的来源与适用边界；若无可靠保证，保持 macOS 直读并如实调整交付目标。不得未经用户裁决放宽“第一次读取不返回旧值”的要求。

下钻：总体设计 §4.1、红线21；`crates/mem-core/src/freshness.rs` 的 `consult_with_hello` 与 `publish_converged_checkpoint`。

## B2：根目录移动/替换的失效承诺没有对应的监听建立契约

**问题与证据。** 方案规定监视 records/attic，遇到根目录变化便永久降级，但没有规定创建流必须启用 `kFSEventStreamCreateFlagWatchRoot`，也没有提供持有目录身份并复核的替代机制。Apple 的 [WatchRoot 文档](https://developer.apple.com/documentation/coreservices/kfseventstreamcreateflagwatchroot)说明该标志用于请求被监听路径及其祖先路径变化的通知；RootChanged 事件携带原始监听路径、对应标志及零事件 ID。只在解析器里处理 RootChanged，不足以保证实际运行会收到它。

**影响。** records 被移走、原路径重建后，不能仅由旧流仍在运行推定新树持续受覆盖。新树内容可与旧检查点具有相同路径、大小和 mtime；若缺少根身份失效证据，stat 仍可能相等，后续查询可能信任不再覆盖当前真相目录的订阅。当前普通文件变化验收和模拟 RootChanged 标志的单元测试均不能证明创建参数正确。

**修订方向与关闭条件。** 在流创建契约中明确启用 WatchRoot、检查 RootChanged 并不可逆失效，或给出等价的根目录及祖先变化检测机制。同时定义根删除、替换、祖先/store 移动以及卸载导致覆盖不确定时的处理。真实 API 集成测试至少覆盖 records、attic 各自移动后原路径重建，以及 store 祖先改名；可构造相同 stat 的替换树以排除 stat 恰好兜底。缺少覆盖证明时必须直读，不能只靠手工注入标志关闭本项。

下钻：本文 §3.1/§3.3 的覆盖承诺；Apple WatchRoot 创建标志与 RootChanged 事件语义。

## 需一并补齐的接线与实施条目

这些条目不另计核心方案阻断，但应在修订时落到具体任务与测试，避免直接套用现有代码导致协议承诺失真。

1. **消化与回注契约。** §3.4 只列了 backend/fresh_undigested，当前订阅抽象还有 `reinject`。应明确每次 fresh 返回是否原子取走脏计数、回调与清零如何避免丢标记、握手维护失败/四轮不收敛后如何恢复待处理信号。若 fresh 永不清零则无法收敛；若取走后丢失欠账，后续握手可能报告零。补维护失败后下一次握手仍不允许快路径的测试。
2. **失效后的同次握手。** 当前 `try_accept_and_reply` 在 fresh 前取得 backend；本次查询返回 None 时，Hello 仍可能携带旧后端名，下一次 tick 才丢弃订阅。fresh 在维护内部失效也有相同延后处理。该结果目前仍会直读，但不符合本文“失效即 Hello.backend=none”的对外承诺。若保持此承诺，需要安排同次失效传播及配置更新测试，不能只增加枚举变体。
3. **实施顺序与回退演练。** §8 给出了源码回退方式，但未安排启用前回退演练，也未让启用任务引用演练证据。按文档写作规则 §3 补入任务依赖，并验证禁用后端/旧版本读取现有 store 后确实直读。

## 验证范围与未验证项

本轮仅新增此报告；未执行 cargo check/test，未编写或运行真实 FSEvents 探针，也未宣称观察到当前机器上的通知延迟。结论来自代码路径核对、Apple 官方 API 文档与 Watchman 官方项目的一手限制记录。工作区已有实现和测试修改保持原样。

本轮最可能被推翻的三处及所需证据：

1. **FlushSync 的适用边界。** 若有权威资料证明受支持的新 macOS 版本已经消除上游反例，且方案明确限定该版本及环境，可重新评估 B1；单次或多次压测通过不足以推翻。
2. **根身份检测的替代实现。** 若拟用绑定默认强制 WatchRoot，或已有独立且完整的根身份验证，可据此关闭 B2，但必须写入方案契约并有真实接线证据。
3. **接线的修改范围。** 本轮依据当前未提交工作区；如果后续 sidecar 已改为同次传播失效、持久保留未消化欠账，应按新代码与故障注入结果重审上述接线条目，不沿用当前判断。
