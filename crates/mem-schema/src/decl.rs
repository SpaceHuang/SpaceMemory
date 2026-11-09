//! 类型声明（两件套）的运行时加载与解析（D19）。
//!
//! 类型在运行时加载，不编译进二进制（R8.2/红线1）。自举试金石：principal 与 view
//! 也必须经类型声明引入（红线13），播种资产见仓库 assets/builtin-types/。

use std::collections::BTreeMap;
use std::path::Path;

use crate::error::LoadError;
use crate::validate;
use crate::Violation;

const DRAFT_2020_12: &str = "https://json-schema.org/draft/2020-12/schema";
/// 派生 rel 缺省名（x-dag 检查的边；可被根级 `x-derived-rel` 覆盖）。
pub const DEFAULT_DERIVED_REL: &str = "derived_from";

/// 校验分档（R8.6）。缺省 lenient（保守默认，strict 必须显式声明）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grade {
    Strict,
    Lenient,
}

/// x-ref 声明：`{"target_type": "<t>", "existence": bool}`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefSpec {
    pub target_type: String,
    pub existence: bool,
}

/// x-filter 字段的声明形态（R10 决策③：取值形态由属性本体的 schema 定义承载；
/// 形态从 `type` 关键字读取，**不从 enum 推导**——评审 L2）。
///
/// v1 支持形态固定四类（string / number / boolean / array）；元素为标量的数组
/// 运行时按元素实际 JSON 形态裁定等值。支持形态之外的声明（对象、多类型、
/// 无 type 等）加载不失败（前向容忍），但会被 verify 告警
/// `filter_shape_unsupported`，运行时对该字段过滤 → 兼容类型集为空 →
/// `invalid_filter_value`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterShape {
    /// `"type": "string"`——折叠等值。
    Text,
    /// `"type": "number"`——JSON 数值文法解析后数值等值。
    Number,
    /// `"type": "boolean"`——仅认字面 `true`/`false`。
    Bool,
    /// `"type": "array"`——任一元素按其实际形态等值即命中。
    Array,
    /// v1 支持形态之外的声明（verify 告警；运行时不可解释任何文本）。
    Unsupported,
}

impl FilterShape {
    /// verify 用：是否 v1 支持形态。
    pub fn is_supported(&self) -> bool {
        !matches!(self, FilterShape::Unsupported)
    }
}

/// 从属性 schema 读取声明形态（单源：形态 = 属性本体定义，不重复声明）。
fn filter_shape_of(prop: &serde_json::Value) -> FilterShape {
    let kinds: Vec<&str> = match prop.get("type") {
        Some(serde_json::Value::String(s)) => vec![s.as_str()],
        // type 数组：剔除 null（可空字段），恰剩一个标量/array 才可判定。
        Some(serde_json::Value::Array(a)) => a
            .iter()
            .filter_map(|v| v.as_str())
            .filter(|s| *s != "null")
            .collect(),
        _ => vec![],
    };
    match kinds.as_slice() {
        ["string"] => FilterShape::Text,
        ["number"] => FilterShape::Number,
        ["boolean"] => FilterShape::Bool,
        ["array"] => FilterShape::Array,
        _ => FilterShape::Unsupported,
    }
}

/// 一种类型声明的运行时形态。
#[derive(Debug)]
pub struct TypeDecl {
    type_name: String,
    grade: Grade,
    unique_key: Vec<String>,
    dag: bool,
    derived_rel: String,
    topics: Vec<String>,
    refs: BTreeMap<String, RefSpec>,
    /// x-filter 声明（payload 属性级，R10 决策③）：字段 → 声明形态。
    filters: BTreeMap<String, FilterShape>,
    raw: serde_json::Value,
    validator: jsonschema::Validator,
}

impl TypeDecl {
    /// 从 schema.json 字节加载（响亮失败：R6.1）。
    pub fn load(type_name: &str, bytes: &[u8]) -> Result<Self, LoadError> {
        let raw: serde_json::Value = serde_json::from_slice(bytes)?;
        let obj = raw.as_object().ok_or(LoadError::NotObject)?;

        match obj.get("$schema").and_then(|v| v.as_str()) {
            Some(s) if s == DRAFT_2020_12 => {}
            Some(other) => return Err(LoadError::WrongDraft(other.to_string())),
            None => return Err(LoadError::WrongDraft("(missing)".into())),
        }

        let grade = match obj.get("x-grade") {
            None | Some(serde_json::Value::Null) => Grade::Lenient,
            Some(serde_json::Value::String(s)) if s == "strict" => Grade::Strict,
            Some(serde_json::Value::String(s)) if s == "lenient" => Grade::Lenient,
            Some(other) => return Err(LoadError::BadGrade(other.to_string())),
        };

        let unique_key = match obj.get("x-unique-key") {
            None | Some(serde_json::Value::Null) => Vec::new(),
            Some(serde_json::Value::Array(a)) => {
                let mut key = Vec::new();
                for item in a {
                    let f = item
                        .as_str()
                        .ok_or_else(|| LoadError::BadUniqueKey)?
                        .to_string();
                    key.push(f);
                }
                key
            }
            Some(_) => return Err(LoadError::BadUniqueKey),
        };

        let dag = matches!(obj.get("x-dag"), Some(serde_json::Value::Bool(true)));
        let derived_rel = obj
            .get("x-derived-rel")
            .and_then(|v| v.as_str())
            .unwrap_or(DEFAULT_DERIVED_REL)
            .to_string();

        let topics = match obj.get("x-topics") {
            None | Some(serde_json::Value::Null) => Vec::new(),
            Some(serde_json::Value::Array(a)) => a
                .iter()
                .map(|v| v.as_str().unwrap_or_default().to_string())
                .collect(),
            Some(_) => Vec::new(),
        };

        let properties = obj.get("properties").and_then(|v| v.as_object());
        // 键字段必须在 properties 有定义（声明自身的坏写入也要可识别，R6.1）。
        for f in unique_key.iter().chain(topics.iter()) {
            if !properties.map(|p| p.contains_key(f)).unwrap_or(false) {
                return Err(LoadError::UnknownKeyedField(f.clone()));
            }
        }

        // x-ref 解析（payload 属性级）。
        let mut refs = BTreeMap::new();
        if let Some(props) = properties {
            for (field, spec) in props {
                if let Some(rs) = spec.get("x-ref") {
                    let target = rs
                        .get("target_type")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| LoadError::BadRefSpec(field.clone()))?
                        .to_string();
                    let existence = rs
                        .get("existence")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true);
                    refs.insert(field.clone(), RefSpec { target_type: target, existence });
                }
            }
        }
        for f in refs.keys() {
            if !properties.map(|p| p.contains_key(f)).unwrap_or(false) {
                return Err(LoadError::UnknownRefField(f.clone()));
            }
        }

        // x-filter 解析（payload 属性级，R10 决策③）。点名缺失与 x-ref 同族：
        // 加载即响亮失败（R6.1）；形态不支持的声明不失败（verify 告警 + 运行时
        // 兼容集为空），保持对支持形态之外声明的前向容忍。
        let mut filters = BTreeMap::new();
        if let Some(props) = properties {
            for (field, spec) in props {
                if spec.get("x-filter") == Some(&serde_json::Value::Bool(true)) {
                    filters.insert(field.clone(), filter_shape_of(spec));
                }
            }
        }
        for f in filters.keys() {
            if !properties.map(|p| p.contains_key(f)).unwrap_or(false) {
                return Err(LoadError::UnknownFilterField(f.clone()));
            }
        }

        let validator = jsonschema::options()
            .build(&raw)
            .map_err(|e| LoadError::SchemaCompile(e.to_string()))?;

        Ok(Self {
            type_name: type_name.to_string(),
            grade,
            unique_key,
            dag,
            derived_rel,
            topics,
            refs,
            filters,
            raw,
            validator,
        })
    }

    pub fn type_name(&self) -> &str {
        &self.type_name
    }
    pub fn grade(&self) -> Grade {
        self.grade
    }
    pub fn unique_key(&self) -> &[String] {
        &self.unique_key
    }
    pub fn dag(&self) -> bool {
        self.dag
    }
    pub fn derived_rel(&self) -> &str {
        &self.derived_rel
    }
    pub fn topics(&self) -> &[String] {
        &self.topics
    }
    pub fn ref_specs(&self) -> &BTreeMap<String, RefSpec> {
        &self.refs
    }

    /// x-filter 声明（R10 决策③）：字段 → 声明形态（含 Unsupported——供 verify
    /// 告警 filter_shape_unsupported 与运行时兼容判定）。
    pub fn filter_fields(&self) -> &BTreeMap<String, FilterShape> {
        &self.filters
    }
    pub fn raw(&self) -> &serde_json::Value {
        &self.raw
    }

    /// draft 2020-12 结构校验（不含 x-ref 存在性——那是跨记录检查，见
    /// [`Self::check_ref_existence`]）。
    pub fn validate_payload(&self, payload: &serde_json::Value) -> Vec<Violation> {
        self.validator
            .iter_errors(payload)
            .map(|e| {
                // required 违例的 instance_path 指向容器；把缺失字段名并进路径，
                // 保证"报告哪一条"落到字段级（R6.1）。
                let path = match &e.kind {
                    jsonschema::error::ValidationErrorKind::Required { property } => {
                        let prop = property.as_str().unwrap_or_default();
                        let base = e.instance_path.to_string();
                        if base.is_empty() {
                            format!("/{prop}")
                        } else {
                            format!("{base}/{prop}")
                        }
                    }
                    _ => e.instance_path.to_string(),
                };
                Violation { path, message: e.to_string() }
            })
            .collect()
    }

    /// 分档裁决（R8.6）：strict 无违例才放行；lenient 违例 → 降级告警。
    pub fn policy(&self, violations: &[Violation]) -> validate::Policy {
        validate::policy_for(self.grade, violations)
    }

    /// x-ref 存在性检查（R8.5）：`exists(target_type, id)` 由调用方提供
    /// （mem-core 用记录表实现；测试用闭包）。null / 非引用字段跳过。
    pub fn check_ref_existence(
        &self,
        payload: &serde_json::Value,
        exists: impl Fn(&str, &str) -> bool,
    ) -> Vec<Violation> {
        validate::check_ref_existence(&self.refs, payload, exists)
    }
}

/// 全店类型声明的运行时集合：`types/` 目录的加载结果。
///
/// R6.2 可隔离：单个坏声明不拖垮整体——降级为 `broken` 条目并显式报告。
#[derive(Debug, Default)]
pub struct TypeSet {
    decls: BTreeMap<String, TypeDecl>,
    broken: Vec<(String, String)>, // (类型名, 错误)
}

impl TypeSet {
    pub fn load_dir(types_dir: &Path) -> std::io::Result<Self> {
        let mut set = Self::default();
        for entry in std::fs::read_dir(types_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            let schema_path = entry.path().join("schema.json");
            match std::fs::read(&schema_path)
                .map_err(LoadError::Io)
                .and_then(|b| TypeDecl::load(&name, &b))
            {
                Ok(d) => {
                    set.decls.insert(name, d);
                }
                Err(e) => set.broken.push((name, e.to_string())),
            }
        }
        Ok(set)
    }

    pub fn get(&self, type_name: &str) -> Option<&TypeDecl> {
        self.decls.get(type_name)
    }

    pub fn decls(&self) -> impl Iterator<Item = &TypeDecl> {
        self.decls.values()
    }

    /// 坏声明清单（R6.1/R6.2：可识别 + 可隔离，绝不静默）。
    pub fn broken(&self) -> &[(String, String)] {
        &self.broken
    }
}
