//! mem-schema 声明加载与校验测试（§8-2/8-3）。

use mem_schema::{Grade, LoadError, TypeDecl, TypeSet, Violation};
use serde_json::json;

const TASK_SCHEMA: &str = r#"{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "title": "task",
  "type": "object",
  "x-grade": "strict",
  "x-unique-key": ["project", "slug"],
  "x-dag": false,
  "x-topics": ["area"],
  "properties": {
    "slug":   { "type": "string", "minLength": 1 },
    "title":  { "type": "string", "minLength": 1 },
    "status": { "enum": ["todo", "in_progress", "done"] },
    "project": { "type": ["string", "null"], "pattern": "^[0-9a-f-]{36}$", "x-ref": { "target_type": "project", "existence": true } },
    "area":    { "type": ["string", "null"], "pattern": "^[0-9a-f-]{36}$", "x-ref": { "target_type": "topic", "existence": true } }
  },
  "required": ["slug", "title", "status"]
}"#;

const DERIVED_SCHEMA: &str = r#"{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "type": "object",
  "x-grade": "strict",
  "x-dag": true,
  "x-derived-rel": "supported_by",
  "properties": { "slug": { "type": "string" } },
  "required": ["slug"]
}"#;

fn good_payload() -> serde_json::Value {
    json!({
        "slug": "login-api",
        "title": "实现登录接口",
        "status": "in_progress",
        "project": "0198f7a9-0000-7cd2-9e3f-000000000001",
        "area": null
    })
}

#[test]
fn loads_task_metadata_from_x_keywords() {
    let d = TypeDecl::load("task", TASK_SCHEMA.as_bytes()).expect("load");
    assert_eq!(d.grade(), Grade::Strict);
    assert_eq!(d.unique_key(), &["project".to_string(), "slug".to_string()]);
    assert!(!d.dag());
    assert_eq!(d.topics(), &["area".to_string()]);
}

#[test]
fn grade_defaults_to_lenient() {
    let s = r#"{
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "object",
      "properties": { "title": { "type": "string" } }
    }"#;
    let d = TypeDecl::load("memory", s.as_bytes()).expect("load");
    assert_eq!(d.grade(), Grade::Lenient);
    assert!(d.unique_key().is_empty(), "无 x-unique-key → 无自然键");
}

#[test]
fn load_rejects_wrong_draft_and_non_object() {
    let bad_draft = r#"{ "$schema": "http://json-schema.org/draft-07/schema#", "type": "object" }"#;
    assert!(matches!(
        TypeDecl::load("x", bad_draft.as_bytes()),
        Err(LoadError::WrongDraft(_))
    ));
    assert!(matches!(
        TypeDecl::load("x", b"[1,2,3]"),
        Err(LoadError::NotObject)
    ));
    assert!(matches!(TypeDecl::load("x", b"not json"), Err(LoadError::Json(_))));
}

#[test]
fn load_rejects_unknown_keyed_fields() {
    let s = r#"{
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "object",
      "x-unique-key": ["nope"],
      "properties": { "a": { "type": "string" } }
    }"#;
    assert!(matches!(
        TypeDecl::load("x", s.as_bytes()),
        Err(LoadError::UnknownKeyedField(_))
    ));
}

#[test]
fn draft_validation_reports_violations_with_paths() {
    let d = TypeDecl::load("task", TASK_SCHEMA.as_bytes()).unwrap();
    let bad = json!({ "slug": "", "title": 42, "status": "unknown-state" });
    let v = d.validate_payload(&bad);
    let paths: Vec<&str> = v.iter().map(|x| x.path.as_str()).collect();
    assert!(paths.iter().any(|p| p.contains("slug")), "{v:?}");
    assert!(paths.iter().any(|p| p.contains("title")), "{v:?}");
    assert!(paths.iter().any(|p| p.contains("status")), "{v:?}");
    // 缺 required
    let v2 = d.validate_payload(&json!({}));
    assert!(v2.iter().any(|x| x.path.contains("slug")));
    assert!(v2.iter().any(|x| x.path.contains("status")));
}

#[test]
fn draft_validation_accepts_good_payload_and_tolerates_unknown_fields() {
    let d = TypeDecl::load("task", TASK_SCHEMA.as_bytes()).unwrap();
    assert!(d.validate_payload(&good_payload()).is_empty());
    // lenient 语义基准：schema 未写 additionalProperties:false → 未知字段不产生违例。
    let mut with_unknown = good_payload();
    with_unknown["surprise"] = json!({"x": 1});
    assert!(d.validate_payload(&with_unknown).is_empty());
}

#[test]
fn grade_policy_decides_reject_vs_warn() {
    let d = TypeDecl::load("task", TASK_SCHEMA.as_bytes()).unwrap();
    let bad = json!({ "slug": "x", "title": "x", "status": "nope" });
    let v = d.validate_payload(&bad);
    assert_eq!(d.policy(&v), mem_schema::Policy::Reject, "strict → 拒收");
    let d2 = TypeDecl::load("memory", r#"{
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "object",
      "properties": { "title": { "type": "string" } },
      "required": ["title"]
    }"#.as_bytes()).unwrap();
    let v2 = d2.validate_payload(&json!({"title": 7}));
    assert_eq!(d2.policy(&v2), mem_schema::Policy::Warn, "lenient → 降级告警");
}

#[test]
fn ref_specs_extracted_and_existence_checked() {
    let d = TypeDecl::load("task", TASK_SCHEMA.as_bytes()).unwrap();
    let refs = d.ref_specs();
    assert_eq!(refs.get("project").map(|r| r.target_type.as_str()), Some("project"));
    assert_eq!(refs.get("area").map(|r| r.target_type.as_str()), Some("topic"));

    // 存在性回调：null 跳过；合法 uuid 查表；查无 → 违例。
    let known = |t: &str, id: &str| t == "project" && id == "0198f7a9-0000-7cd2-9e3f-000000000001";
    let mut p = good_payload();
    assert!(d.check_ref_existence(&p, known).is_empty());
    p["project"] = json!("0198f7a9-0000-7cd2-9e3f-000000000099");
    let v = d.check_ref_existence(&p, known);
    assert_eq!(v.len(), 1, "{v:?}");
    assert!(v[0].path.contains("project"));
}

#[test]
fn dag_flag_and_derived_rel() {
    let d = TypeDecl::load("derived", DERIVED_SCHEMA.as_bytes()).unwrap();
    assert!(d.dag());
    assert_eq!(d.derived_rel(), "supported_by");
    let d2 = TypeDecl::load("task", TASK_SCHEMA.as_bytes()).unwrap();
    assert_eq!(d2.derived_rel(), "derived_from", "缺省 rel = derived_from");
}

#[test]
fn typeset_loads_dir_and_isolates_broken_decls() {
    let tmp = tempfile::tempdir().unwrap();
    let types = tmp.path().join("types");
    std::fs::create_dir_all(types.join("task")).unwrap();
    std::fs::create_dir_all(types.join("memory")).unwrap();
    std::fs::create_dir_all(types.join("broken")).unwrap();
    std::fs::write(types.join("task/schema.json"), TASK_SCHEMA).unwrap();
    std::fs::write(types.join("memory/schema.json"), r#"{
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "object", "x-grade": "lenient",
      "properties": { "title": { "type": "string" } }
    }"#).unwrap();
    std::fs::write(types.join("broken/schema.json"), b"{ not json").unwrap();

    let set = TypeSet::load_dir(&types).expect("目录可加载");
    assert!(set.get("task").is_some());
    assert!(set.get("memory").is_some());
    // R6.2 可隔离：坏声明不拖垮整体，但必须显式报告。
    assert_eq!(set.broken().len(), 1);
    assert_eq!(set.broken()[0].0, "broken");
    assert!(set.get("broken").is_none());
}

#[test]
fn evolution_old_payload_flags_degraded_but_readable() {
    // R8.7：schema 演化后旧载荷仍可读——校验产生违例（降级标注的原料），不抛异常。
    let d = TypeDecl::load("task", TASK_SCHEMA.as_bytes()).unwrap();
    let old_payload = json!({ "slug": "legacy", "title": "旧任务" }); // 缺 status（新 schema 才 required）
    let v: Vec<Violation> = d.validate_payload(&old_payload);
    assert!(!v.is_empty());
}

#[test]
fn builtin_assets_load_through_the_real_loader() {
    // 播种资产（红线13 自举来源）必须能被真实加载器全部解析。
    for (path, bytes) in mem_assets_shim::assets() {
        if path.ends_with("schema.json") {
            let name = path.split('/').nth(1).unwrap();
            TypeDecl::load(name, &bytes).unwrap_or_else(|e| panic!("{path}: {e}"));
        }
    }
}

// 红线1：测试直接消费仓库文件（构建期外），不经机制 parse 语义。
mod mem_assets_shim {
    pub fn assets() -> Vec<(String, Vec<u8>)> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/builtin-types");
        let mut out = Vec::new();
        fn walk(dir: &std::path::Path, rel: &str, out: &mut Vec<(String, Vec<u8>)>) {
            for e in std::fs::read_dir(dir).unwrap() {
                let e = e.unwrap();
                let name = e.file_name().to_string_lossy().to_string();
                let p = e.path();
                let r = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
                if p.is_dir() { walk(&p, &r, out); } else { out.push((r, std::fs::read(p).unwrap())); }
            }
        }
        walk(&root, "", &mut out);
        out.sort();
        out
    }
}
