# principal 约定

## 载荷字段语义
- principal 是**归属**维度（谁写的、谁的台账），不承担"现场相关性"（D13.5）。
- 防错报不防伪装（红线12）：自报身份不被采信，机制按 principal 记录存在性强制；首条 principal 由 `mem init` 自举。

## 生命周期
- 一类执行体一条 principal（如 codex_main）；不按会话建 principal（会话是台账的事）。

## 提炼与引用
- 记录的 envelope.principal 指向此处；溯源（R9.2"principal 溯源"）即沿该引用。
