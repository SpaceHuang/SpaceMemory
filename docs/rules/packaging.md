# 打包与分发指南

> **触发时机**：首次对外分发、配置发布工具（cargo-dist 之类）时读取全文；日常开发无需加载。
> **决策依据**：混合分发模式——外部优先、嵌入兜底（裁决记录：[assets/builtin-types/README.md](../../assets/builtin-types/README.md)；设计文档开放问题40）。

## 1. 发布物形态

每个发布物 = `mem` 可执行文件 + `builtin-types/` 目录（exe 同级）：

```
mem-v0.1.0-x86_64-pc-windows-msvc/
├── mem.exe
└── builtin-types/            # 原样整目录复制自仓库 assets/builtin-types/（含其 README）
    ├── task/{schema.json, conventions.md}
    ├── memory/{schema.json, conventions.md}
    └── …
```

macOS `.app` 形态（若做）：`Contents/MacOS/mem` + `Contents/Resources/builtin-types/`（查找逻辑需覆盖 Resources 路径）。

## 2. 落地清单（逐项执行，做完勾掉）

- [ ] 发布工具配置 extra 静态资产：`assets/builtin-types/` 原样复制进**每个**发布物（zip/tar 中位于 exe 同级的 `builtin-types/`）；
- [ ] Windows 安装器（MSI/NSIS）：目录装入安装根（与 mem.exe 同级）；
- [ ] macOS tarball：同上布局；Homebrew：装入 `share/mem/builtin-types/`，查找逻辑须覆盖 exe 的 `../share/mem/builtin-types`（参照 helix runtime 的 brew 布局）；pkg/.app：Resources 路径；
- [ ] `mem init` 查找顺序实现齐全：模板覆盖配置 > exe 旁 `builtin-types/` > 内嵌字节；
- [ ] 验收两条播种路径：① 移走/改名外部目录 → init 走嵌入兜底成功；② 外部在场 → 优先走外部（临时污染外部内容可验证优先级）；
- [ ] 模板覆盖配置实现并测试（配置名随格式规范 v1 定稿，git 的对应物是 `init.templateDir`）。

## 3. 实施注意

- `include_bytes!` 相对路径从 `crates/mem-assets/src/` 出发为 `../../../assets/builtin-types/…`，workspace 构建下成立；**crates.io 源码包渠道不可用**——`cargo package` 只收 crate 目录内文件，跨目录资产进不了包。当前裁决：正式分发 = 从仓库构建的发布物；crates.io 最多作开发者渠道（`cargo install --git` 或直接用发布物）；
- 嵌入登记处：`crates/mem-assets/src/lib.rs` 的 `assets()`（两件套定稿后逐条 `include_bytes!`；只搬运、永不 parse——红线1 裁决，见 assets README）；
- 版本一致性：外部 `builtin-types/` 与 exe 同批构建、同包分发，天然同版本；模板覆盖配置引入的用户自备目录**不做版本检查**（用户资产；结构问题走 R6 校验报告路径）。

## 4. 未收口项

随格式规范 v1 定稿（设计文档开放问题40）：正式分发渠道承诺；模板覆盖配置定名；红线1"携带/认识"边界的规范条文。
