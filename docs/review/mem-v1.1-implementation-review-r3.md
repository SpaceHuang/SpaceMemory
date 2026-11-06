# mem v1.1 实施评审——复审报告（R3，第二轮修复验证）

- 评审对象：R2 报告（`mem-v1.1-implementation-review-r2.md`）B4 / H7 / M4 / M6a,b / L7 / L8 的修复回应；裁决归档见设计文档开放问题39 增补（"v1.2 复审（r2）增补"）
- 评审日期：2026-11-05
- 评审环境：同前（Windows 11，**非提权**用户进程——RDC 兜底在本机可真实触发）；`cargo check` 零警告、`cargo test --workspace` **150 项全绿**（R2 轮 137 → +13）
- 评审方式：读码核对 + 本机 e2e（含 B4 对抗场景手工实测——R2 轮受 H7 遮蔽无法触发，本轮 RDC 启用后首次可真实验证）

## 复审结论（TL;DR）

**B4、H7 修复验证通过**——本机（非提权）实测：RDC 订阅生效（`read_mode: indexed, via_subscription: rdc`）、同尺寸且 **mtime 完全保持**的对抗性修改后**第一次读取即返回新值**（R2 报告的核心场景在真实环境成立）。L7/L8 已修。**遗留 3 项**：两个新发现（RDC 线程静默退出不置 gap → 订阅僵尸【H8】；握手时维护失败后信号被消费不重试【M12】）、1 个测试弱点（B4 自动回归未保 mtime，测的是 stat 路径而非订阅路径【M13】），以及跨轮残留（M4 update 半项、M6a/b 未归档、AGENTS.md 指针）。均非阻断。

---

## 一、R2 项验证

### B4 快路径证明用缓存值 → ✅ 修复通过（代码 + 真实对抗实测）

**代码核对**：`try_accept_and_reply` 重构为携带订阅与维护器（sidecar.rs:322-345）——应答**前**做 `fresh_undigested()` 新鲜查询；若 >0 **先执行维护再应答 Some(0)**；维护失败则如实应答 Some(n)（本次读取转直读）。RDC 的 `fresh_undigested` 为 `undigested.swap(0)`（每次直连 OS 源、即查即消化，windows.rs:594-599）；USN 路径同理逐次读 journal。轮询缓存值不再参与证明。

**本机对抗实测**（非提权、RDC 后端）：基线 `find markerold111` → `indexed via rdc` 命中；随后对记录做**等长 payload 修改**（359 字节 = 359 字节）并用 `touch -d` **将 mtime 精确还原到纳秒原值**（1793833438.003335700 前后一致）——stat 检查 (a) 被刻意绕过，唯一防线是订阅证明；立即读取：**新标记命中 1、旧标记命中 0、模式 indexed**（握手新鲜查询消化后以新索引应答）。红线21 立法场景（同尺寸保 mtime + 第一次读取）在真实环境通过。

**附注**：fastpath e2e（fastpath.rs:117-137）新增了 B4 回归段，但其等长修改**未还原 mtime**——该编辑会改变 stat，被条件 (a) 兜住转直读，**即使没有 B4 修复此测试也通过**（详见 M13）。真正的钉住依赖本报告的手工实测，建议按 M13 补强。

### H7 非提权 USN 失效 → ✅ 修复通过（方案 b：RDC 兜底）

**代码核对**：`Subscription` 抽象（USN / RDC 两后端，sidecar.rs:199-239）；挂接顺序 USN（提权）→ RDC（非提权，watch records/ + attic/ 双子树，忽略 `.lock`/`.tmp-` 机制副产物）→ none（永久直读）。RDC 关键语义核对：
- **变更通知按操作而非 stat 差异**：flags 含 FILE_NOTIFY_CHANGE_LAST_WRITE | SIZE | ATTRIBUTES——写操作本身触发通知，保 mtime 的写入同样被捕（对抗实测已证）；
- **溢出 = 覆盖缺口**：ERROR_NOTIFY_ENUM_DIR → `gap=true` → `fresh_undigested` 恒 None → 永久直读（安全侧，windows.rs:665-668）；
- **忽略过滤安全**：键成分字符集禁点，合法记录文件名不可能含 `.lock`/`.tmp-`，无误伤；
- **新文件/深层路径**：子树递归 watch 天然覆盖（FRN 快照法的 track() 缺陷不复存在）。

**本机实测**：非提权进程下 sidecar 报 `backend: rdc`，list/find 走 indexed 且 FTS 命中；fastpath e2e 在本机实际执行 indexed 分支（150 绿含此项，单跑 10.5s 通过）——R2 报告"indexed 分支在本机从未执行"的如实边界**解除**。

**裁决归档核对**：设计文档开放问题39 已增补（"非提权 Windows = 最常见形态，矩阵此前缺失" + 三选一裁决：已实施 b（RDC）；若设计评审倾向 a/c 撤下 RDC 即回退 USN-only）。注意该裁决注明"待设计收口"——RDC 作为证明来源与设计原文"watcher 非新鲜度依据"（§1.3 否决形态表）存在张力，增补已如实标注，留待设计轮正式收口。**归档合格，无新问题。**

### 其余 R2 遗留

| 项 | 验证结果 |
|---|---|
| L7 死条件 `1<<62` | ✅ 已删除 |
| L8 body_b64 schema | ✅ add/update 两处工具定义均已宣传（含"非 UTF-8 正文必须用它"说明） |
| M4 MCP update payload | ❌ **仍残留**：`mem_update` 的 payload 仍是 `unwrap_or_else(\|\| json!({}))`（protocol.rs:196）——lenient 类型省略 payload 依旧静默清空载荷。第二轮复检仍未修 |
| M6a/b 读路径落盘 | ❌ **仍未处置且无裁决记录**（.tmp 探针、brief 建 sessions/ 目录——方向安全，但按决策落档纪律应有明文归档） |
| AGENTS.md 指针 | ❌ 仍写"见 docs/review/mem-v1.1-implementation-review.md 的修复回应"——修复回应实际在**设计文档 v1.2 修订行 + 开放问题39 增补**，指针目标错误（跨轮未修的小项） |
| 设计文档版本行 | 观察项（非发现）：本轮修复记于 Q39 增补而未升版本行——版本记录由用户主导（AGENTS.md §7），不构成违规，提示补一条修订行更完整 |

## 二、新发现（均非阻断）

### H8（新）. RDC 线程两条退出路径不置 gap——订阅僵尸：证明名存实亡

**位置**：`crates/mem-core/src/platform/windows.rs:614-617`（CreateEventW 失败直接 return）、`:645-649`（ReadDirectoryChangesW 调用失败 CloseHandle 后 return）

`rdc_read_loop` 有两类静默退出：事件对象创建失败、或 ReadDirectoryChangesW 调用本身失败（目录句柄失效、网络存储断开等）。两条路径都**没有置 `gap=true`** 就退出线程。此后 `RdcWatcher` 仍存活，`fresh_undigested()` 恒返回 `Some(swap(0))=Some(0)`——**订阅宣称覆盖存在，实际已停止投递**。此形态下：普通编辑仍被 stat (a) 兜住，但对抗性同尺寸保 mtime 修改会被旧索引应答——与 B4 同类危害，只是触发条件从"任意修改落在 400ms 窗口"收窄为"目录 watch 先异常死亡"。

修复：线程的**每条非 stop 退出路径**置 `gap=true`（两行）。连带建议：`Drop` 里的 `CancelIoEx` 作用于 AtomicIsize 保存的句柄值——若线程已因失败退出并 CloseHandle，该值是悬空的，句柄值被内核回收后会取消无关对象的 I/O；线程退出前应将 AtomicIsize 置空（或 Drop 判 gap 后跳过 Cancel）。

**为什么定 H 不定 B**：触发需要 OS 级异常（watch 句柄失效），正常运行不触及；但它是协议"自报失效"义务的漏洞，修复成本两行，应随下批处理。

### M12（新）. 握手路径维护失败后，未消化信号已被消费且不重试

**位置**：`crates/mem-core/src/sidecar.rs:330-337`

握手时新鲜查询即消化（USN 游标推进 / RDC 计数 swap 清零）；若随后 `run_maintenance` 失败（sqlite 写失败、磁盘满等），应答 `Some(n)`（本次读取转直读，安全），但**信号已丢**——下一轮 tick 查询得 0，不再重试维护。若此前存在对抗性同 stat 修改，索引将保持陈旧直至下一次真实变更；期间证明恒成立（stat 一致 ∧ undigested 0）→ 陈旧索引被持续应答。对比：循环 tick 路径的维护失败经 `?` 直接终止 serve（后续全部直读，安全）——**两条路径失败语义不一致**。建议：握手路径维护失败时同样 `sub = None`（保守降级永久直读），与 tick 路径对齐。

### M13（新）. B4 自动回归测试未还原 mtime——测的是 stat 路径，不钉住修复

**位置**：`crates/mem/tests/fastpath.rs:117-137`

回归段的等长修改（title "T"→"U"）改变了 mtime → stat 检查 (a) 失配 → 直读返回新令牌——**该测试在 B4 未修复的代码上同样通过**。真正钉住 B4 需要还原 mtime（本报告手工实测的做法：`touch -d` 纳秒还原；测试内可用 Windows SetFileTime 或 `filetime` crate）。建议补强，否则 B4 回归保护实际不存在。

## 三、测试与验收核对

- 150 项全绿（+13）：RDC 单测（计数/消化/忽略机制副产物）、fastpath e2e 扩展（backend 认识 rdc、B4 回归段、空闲自退回落）；
- **本机首次真实执行 indexed 分支**（R2 的验证边界解除）：物化、快路径断言、FTS 命中、对抗场景全部实跑；
- 缺口：M13（B4 回归不钉住）、H8 的僵尸退出与 M12 的维护失败路径无测试（修复时顺带补）。

## 四、自曝三处（请评审人优先核查本报告最可能错的地方）

1. **M13 的断言**——"未还原 mtime 的等长修改必被 stat 兜住"依赖 stat_snapshot 比较含 mtime（freshness.rs:75-89）与 fastpath 测试编辑确实更新 mtime（serialize 后 fs::write，mtime 自然更新）。若 stat 比较实现有我不知道的粒度豁免，此条降级；可临时在 B4 修复前跑该测试验证（预期仍绿即证实）；
2. **H8 的触发现实性**——ReadDirectoryChangesW 中途失败的实际频率（本地 NTFS 上极低；网络存储较高）是经验判断；存在性与代码路径确定，严重度评估可辩论；
3. **B4 手工实测的时序**——"立即读"经 Git Bash 起进程约 100–300ms，可能落在 RDC 线程唤醒（µs–ms）之后而非真正的通知间隙内；若真要压测间隙，需要进程内同毫秒查询。实测结论（通过）对间隙的覆盖不完全确定，但代码语义（应答前新鲜查询）已独立成立。

## 五、总结

| 维度 | 评价 |
|---|---|
| R2 阻断 B4 | **修复验证通过**（代码 + 非提权真实对抗实测：保 mtime 等长修改首读返回新值） |
| R2 高优 H7 | **修复验证通过**（RDC 兜底，本机 indexed 生效，Q39 裁决归档合格） |
| 新发现 | H8（RDC 僵尸退出，H）、M12（维护失败信号丢失，M）、M13（B4 回归测试不钉住，M）——均非阻断 |
| 跨轮残留 | M4 update 半项（两轮未修）、M6a/b 未归档、AGENTS.md 指针（一轮未修） |
| 建议 | **本轮无阻断，可进入发布准备**；H8/M12/M13 建议发布前顺手处理（合计约十行代码 + 一个测试补强）；M4/M6ab/指针补归档 |
| 复审边界 | RDC 后端在本机全链实测；USN 提权分支仍仅代码核对（本机无法提权触发）——与 R2 相同的如实标注 |

---

### 待批准清单

**本轮批准范围**：B4、H7 修复的验收通过；L7/L8 通过；H8/M12/M13 的发现与定级；M4/M6a,b/AGENTS 指针残留的确认。
**本轮明确不批**：H8/M12/M13 的具体修复实现（建议随发布准备批处理，修复后不必再走完整复审，随打包验证即可）；开放问题39 的 RDC 裁决收口属设计评审（已在 Q39 标注"待设计收口"）。
