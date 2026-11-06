//! 播种资产登记测试（§8-2/8-3：两件套定稿后逐条登记 include_bytes!）。
//!
//! 契约（assets/builtin-types/README.md）：
//! - `assets()` 返回 `(相对路径, 字节)`，相对路径以数据目录根为基准；
//! - 字节与仓库源文件逐字节一致（编译期嵌入 = 仓库文件快照）；
//! - 必含 principal 与 view 两件套（红线13 自举：principal/view 必须经类型声明引入）；
//! - 必含 store 级 CONVENTIONS.md。

use std::path::PathBuf;

const EXPECTED_MIN: usize = 16; // 7 类型 × 2 + CONVENTIONS.md + bootstrap/principal.payload.json

fn repo_asset_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets/builtin-types")
}

#[test]
fn assets_are_nonempty_and_cover_all_builtin_types() {
    let list = mem_assets::assets();
    assert!(list.len() >= EXPECTED_MIN, "assets() 登记数 = {}，应 ≥ {}", list.len(), EXPECTED_MIN);
    for t in ["task", "memory", "project", "topic", "principal", "view", "edge"] {
        let schema = format!("types/{t}/schema.json");
        let conv = format!("types/{t}/conventions.md");
        assert!(list.iter().any(|(p, _)| *p == schema), "缺 {schema}");
        assert!(list.iter().any(|(p, _)| *p == conv), "缺 {conv}");
    }
    assert!(
        list.iter().any(|(p, _)| *p == "conventions/CONVENTIONS.md"),
        "缺 store 级 CONVENTIONS.md"
    );
}

#[test]
fn embedded_bytes_equal_repo_files() {
    let dir = repo_asset_dir();
    for (path, bytes) in mem_assets::assets() {
        let on_disk = std::fs::read(dir.join(path))
            .unwrap_or_else(|e| panic!("仓库缺源文件 {path}: {e}"));
        assert_eq!(*bytes, on_disk, "内嵌字节与仓库文件不一致：{path}");
    }
}

#[test]
fn relative_paths_are_portable() {
    // 全部为 / 分隔、无前导 ./、无 ..——播种例程直接拼数据目录路径。
    for (path, _) in mem_assets::assets() {
        assert!(!path.starts_with('/') && !path.starts_with("./"), "{path}");
        assert!(!path.contains('\\'), "路径必须用 /：{path}");
        assert!(!path.contains(".."), "{path}");
    }
}

#[test]
fn bootstrap_types_present_red_line_13() {
    // 红线13：principal 与 view 必须经类型声明引入——播种资产即其声明来源。
    for t in ["principal", "view"] {
        let (p, b) = mem_assets::assets()
            .iter()
            .find(|(p, _)| *p == format!("types/{t}/schema.json"))
            .unwrap_or_else(|| panic!("缺 types/{t}/schema.json"));
        assert_eq!(*p, format!("types/{t}/schema.json"));
        assert!(!b.is_empty());
    }
}
