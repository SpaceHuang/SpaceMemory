//! r14 发布准备回归：packaging.md §2 落地清单第 5 项——两条播种路径验收。
//! ① 外部资产缺失 → init 走内嵌兜底成功（混合分发裁决，开放问题40）；
//! ② 外部在场 → 优先走外部（污染法验证优先级：外部字节原样搬运，含污染标记）。
//! 查找顺序本身（模板覆盖 > exe 旁 > share 布局 > 内嵌）由 mem-assets 单测覆盖。

use assert_cmd::Command;
use serde_json::{json, Value};

fn mem() -> Command {
    Command::cargo_bin("mem").expect("mem 二进制")
}

fn j(out: &std::process::Output) -> Value {
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("输出不是 JSON：{e}\n{}", String::from_utf8_lossy(&out.stdout)))
}

/// 复制仓库真实资产到目标目录（原样整目录，含 README）。
fn copy_assets(dst: &std::path::Path) {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/builtin-types");
    fn walk(src: &std::path::Path, dst: &std::path::Path) {
        for e in std::fs::read_dir(src).unwrap().flatten() {
            let p = e.path();
            let d = dst.join(e.file_name());
            if p.is_dir() {
                std::fs::create_dir_all(&d).unwrap();
                walk(&p, &d);
            } else {
                std::fs::copy(&p, &d).unwrap();
            }
        }
    }
    std::fs::create_dir_all(dst).unwrap();
    walk(&src, dst);
}

#[test]
fn r14_init_embedded_fallback_when_no_external_assets() {
    // 路径①：exe 旁无 builtin-types（构建树默认形态）→ 内嵌兜底成功。
    let exe_dir = std::env::current_exe().unwrap().parent().unwrap().to_path_buf();
    let exe_side = exe_dir.join("builtin-types");
    assert!(!exe_side.exists(), "测试环境被污染：exe 旁存在 {exe_side:?}（发布物冒烟应使用 dist/ 布局，不得写入构建树）");

    let tmp = tempfile::tempdir().unwrap();
    let s = tmp.path().join("store").to_str().unwrap().to_string();
    let o = mem().args(["--store", &s, "init", &s, "--principal", "codex_main"]).output().unwrap();
    let v = j(&o);
    assert_eq!(v["ok"], json!(true), "{v}");
    assert_eq!(v["asset_source"], json!("embedded"), "无外部资产必须走内嵌兜底：{v}");
    assert_eq!(v["template_dir"], Value::Null, "{v}");
    // 内嵌登记处 = 7 类型×2 件套 + conventions + bootstrap（mem-assets 单测同源断言）。
    assert_eq!(v["seeded_files"], json!(16), "{v}");
    // 类型两件套可用：list 正常返回（类型加载成功）。
    let o = mem().args(["--store", &s, "list", "--limit", "5"]).output().unwrap();
    assert!(o.status.success(), "内嵌播种的店必须可用");
}

#[test]
fn r14_external_assets_take_priority_with_pollution_probe() {
    // 路径②：外部在场 → 优先走外部（污染法：外部目录加入内嵌没有的标记文件，
    // init 后数据目录出现该文件 = 字节确实来自外部而非内嵌）。
    let tmp = tempfile::tempdir().unwrap();
    let tpl = tmp.path().join("tpl");
    copy_assets(&tpl);
    // 污染标记：内嵌字节不含此文件。
    std::fs::write(tpl.join("types/memory/zzz-pollution-probe.md"), b"external-probe\n").unwrap();

    let dir = tmp.path().join("store");
    let s = dir.to_str().unwrap().to_string();
    let o = mem()
        .args(["--store", &s, "init", &s, "--principal", "codex_main", "--template-dir",
            tpl.to_str().unwrap()])
        .output()
        .unwrap();
    let v = j(&o);
    assert_eq!(v["ok"], json!(true), "{v}");
    assert_eq!(v["asset_source"], json!("external"), "模板覆盖在场必须优先于内嵌：{v}");
    assert!(
        v["template_dir"].as_str().unwrap_or("").ends_with("tpl"),
        "来源路径应为模板目录：{v}"
    );
    // 污染标记文件原样落店 = 外部字节优先（红线1：只搬运、不解释）。
    assert!(
        dir.join("types/memory/zzz-pollution-probe.md").is_file(),
        "外部目录的标记文件必须被原样搬运（外部优先验证，packaging.md §2 清单第 5②）"
    );
    // 类型加载不受多余文件影响（find/list 仍可用）。
    let o = mem().args(["--store", &s, "list", "--limit", "5"]).output().unwrap();
    assert!(o.status.success());
}
