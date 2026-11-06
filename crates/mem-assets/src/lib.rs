//! 播种资产字节载体（混合分发裁决，记录见仓库 assets/builtin-types/README.md；
//! 设计文档开放问题40）。
//!
//! 职责边界（红线1 的裁决解释：**禁"认识"、不禁字节级携带**）：
//! - 本 crate 把 `assets/builtin-types/` 下的类型两件套以**字节**打进二进制，
//!   供 `mem init` 在外部资产缺失时兜底播种；
//! - 对外契约只有一条：[`assets`] 返回 `(相对路径, 字节)` 列表；
//! - 本 crate 与仓库其他代码都**不 parse、不校验、不解释**这些字节——机制对类型
//!   内容零知识（R8.2）；两件套的真实解释者是运行时加载的 `mem-schema`，且只作用于
//!   数据目录 `types/` 里的副本，从不作用于本 crate 的字节；
//! - 试金石：改任一类型的字段定义，只应改动 assets/builtin-types/ 下的数据文件，
//!   不得要求改动任何 Rust 代码。
//!
//! 混合查找顺序（`mem init` 执行）：exe 旁 `builtin-types/` 或模板覆盖配置指定目录
//! （mem.json `init.template_dir`）→ 命中则用外部文件；未命中 → 用本 crate 内嵌字节。
//! 任何一次 init 只消费一份。

macro_rules! asset {
    ($p:literal) => {
        ($p, include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/builtin-types/", $p)) as &'static [u8])
    };
}

/// 内嵌播种资产：`(相对路径, 字节)`；相对路径以数据目录根为基准
/// （如 `types/task/schema.json`、`conventions/CONVENTIONS.md`）。
pub fn assets() -> &'static [(&'static str, &'static [u8])] {
    &[
        asset!("types/task/schema.json"),
        asset!("types/task/conventions.md"),
        asset!("types/memory/schema.json"),
        asset!("types/memory/conventions.md"),
        asset!("types/project/schema.json"),
        asset!("types/project/conventions.md"),
        asset!("types/topic/schema.json"),
        asset!("types/topic/conventions.md"),
        asset!("types/principal/schema.json"),
        asset!("types/principal/conventions.md"),
        asset!("types/view/schema.json"),
        asset!("types/view/conventions.md"),
        asset!("types/edge/schema.json"),
        asset!("types/edge/conventions.md"),
        asset!("conventions/CONVENTIONS.md"),
        asset!("bootstrap/principal.payload.json"),
    ]
}


/// 播种来源（packaging.md §2 清单：外部优先、嵌入兜底）。
#[derive(Debug, PartialEq)]
pub enum SeedOrigin {
    /// 外部目录命中（模板覆盖配置或 exe 旁/share 布局的 `builtin-types/`）。
    External(std::path::PathBuf),
    /// 内嵌字节兜底（外部资产缺失）。
    Embedded,
}

/// 播种结果：文件清单 + 实际使用的来源。
pub struct SeedSource {
    pub files: Vec<(String, Vec<u8>)>,
    pub origin: SeedOrigin,
}

/// 播种来源裁定（packaging.md §2 清单第 4/3 项）：
/// 模板覆盖目录 > exe 旁 `builtin-types/` > Homebrew share 布局
///（`<exe>/../share/mem/builtin-types`，参照 helix runtime 的 brew 布局）> 内嵌字节。
/// 候选必须**存在且为目录**才命中；任一外部目录命中 → 整目录字节搬运
///（原样复制、永不 parse——红线1）；全缺 → 内嵌兜底。
pub fn resolve_seed_files(
    template_dir: Option<&std::path::Path>,
    exe_dir: Option<&std::path::Path>,
) -> SeedSource {
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Some(d) = template_dir {
        candidates.push(d.to_path_buf());
    }
    if let Some(d) = exe_dir {
        candidates.push(d.join("builtin-types"));
        // Homebrew 安装根布局：bin/mem + share/mem/builtin-types。
        candidates.push(
            d.parent()
                .map(|p| p.join("share").join("mem").join("builtin-types"))
                .unwrap_or_default(),
        );
    }
    for dir in candidates {
        if dir.is_dir() {
            return SeedSource { files: walk_dir(&dir), origin: SeedOrigin::External(dir) };
        }
    }
    SeedSource {
        files: assets().iter().map(|(p, b)| (p.to_string(), b.to_vec())).collect(),
        origin: SeedOrigin::Embedded,
    }
}

fn walk_dir(dir: &std::path::Path) -> Vec<(String, Vec<u8>)> {
    fn walk(dir: &std::path::Path, rel: &str, out: &mut Vec<(String, Vec<u8>)>) {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                let p = e.path();
                let r = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
                if p.is_dir() {
                    walk(&p, &r, out);
                } else if let Ok(b) = std::fs::read(&p) {
                    out.push((r, b));
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, "", &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe_dir(root: &std::path::Path, marker: &str) -> std::path::PathBuf {
        // 造一个最小外部模板目录（结构 = store 根映射），带可识别污染标记。
        let d = root.join(marker);
        std::fs::create_dir_all(d.join("types/memory")).unwrap();
        std::fs::create_dir_all(d.join("conventions")).unwrap();
        std::fs::write(d.join("types/memory/schema.json"), format!("{marker}\n")).unwrap();
        std::fs::write(
            d.join("conventions/CONVENTIONS.md"),
            format!("- version: 1\n{marker}\n"),
        )
        .unwrap();
        d
    }

    #[test]
    fn resolve_prefers_template_dir_over_exe_side_and_embedded() {
        let tmp = tempfile::tempdir().unwrap();
        let tpl = probe_dir(tmp.path(), "tpl");
        let exe_side = probe_dir(tmp.path(), "exe-side");
        let exe_dir = tmp.path().join("bin");
        std::fs::create_dir_all(&exe_dir).unwrap();
        // exe 旁目录名为固定 builtin-types——把 exe-side 改造到正确位置。
        std::fs::rename(&exe_side, exe_dir.join("builtin-types")).unwrap();
        let s = resolve_seed_files(Some(&tpl), Some(&exe_dir));
        assert_eq!(s.origin, SeedOrigin::External(tpl.clone()), "模板覆盖必须最优先");
        assert!(s.files.iter().any(|(p, b)| p == "types/memory/schema.json"
            && String::from_utf8_lossy(b).contains("tpl")), "字节必须来自模板目录（原样搬运）");
    }

    #[test]
    fn resolve_uses_exe_side_when_no_template() {
        let tmp = tempfile::tempdir().unwrap();
        let exe_dir = tmp.path().join("bin");
        std::fs::create_dir_all(exe_dir.join("builtin-types/types/memory")).unwrap();
        std::fs::write(exe_dir.join("builtin-types/types/memory/schema.json"), b"exe-side\n").unwrap();
        let s = resolve_seed_files(None, Some(&exe_dir));
        assert_eq!(s.origin, SeedOrigin::External(exe_dir.join("builtin-types")));
        assert!(s.files.iter().any(|(p, b)| p == "types/memory/schema.json"
            && String::from_utf8_lossy(b).contains("exe-side")), "exe 旁外部目录优先于内嵌");
    }

    #[test]
    fn resolve_falls_back_to_share_layout_for_homebrew() {
        // Homebrew 安装根：bin/mem + share/mem/builtin-types（exe 的 ../share/…）。
        let tmp = tempfile::tempdir().unwrap();
        let exe_dir = tmp.path().join("bin");
        let share = tmp.path().join("share/mem/builtin-types");
        std::fs::create_dir_all(share.join("types/memory")).unwrap();
        std::fs::write(share.join("types/memory/schema.json"), b"brew\n").unwrap();
        let s = resolve_seed_files(None, Some(&exe_dir));
        assert_eq!(s.origin, SeedOrigin::External(share),
            "share 布局候选必须被覆盖（packaging.md §2 清单第 3 项）");
        assert!(s.files.iter().any(|(p, b)| p == "types/memory/schema.json"
            && String::from_utf8_lossy(b).contains("brew")));
    }

    #[test]
    fn resolve_embedded_when_no_external_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let exe_dir = tmp.path().join("empty-bin");
        std::fs::create_dir_all(&exe_dir).unwrap();
        let s = resolve_seed_files(Some(&tmp.path().join("missing")), Some(&exe_dir));
        assert_eq!(s.origin, SeedOrigin::Embedded, "外部全缺 → 内嵌兜底（混合分发，开放问题40）");
        assert_eq!(s.files.len(), 16, "内嵌两件套登记处逐条 include_bytes（7 类型×2 + conventions + bootstrap）");
        assert!(s.files.iter().any(|(p, _)| p == "types/task/schema.json"));
        assert!(s.files.iter().any(|(p, _)| p == "bootstrap/principal.payload.json"));
    }
}
