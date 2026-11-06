//! 信封（envelope）：机制拥有并管理的公共字段。
//!
//! id（uuid v7）、类型标识、created/modified、并发版本、principal（存在性强制，
//! 红线12）、refs（类型化引用，只填 ID）、aliases、`evidence_cutoff_seq`、来源提交 SHA。
//! 物理形态为 frontmatter 的 `envelope` 块（D18）；跨类型通用操作只依赖信封，
//! 不感知载荷（R8.4）。
//!
//! 未知 envelope 键**保留不丢弃**（原样再生成，verify 另行告警）——机制不吞字节。

use std::collections::BTreeMap;

/// 类型化引用：只填 ID 不填名（D11/红线11）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Ref {
    pub rel: String,
    pub target: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Envelope {
    pub id: String,
    pub r#type: String,
    /// RFC 3339 UTC（创建时刻）。
    pub created: String,
    /// RFC 3339 UTC（最近机制写时刻）。
    pub modified: String,
    pub version: u64,
    /// 归属 principal 记录 ID；null = 未归属（首条 principal 的自举逃逸）。
    pub principal: Option<String>,
    pub refs: Vec<Ref>,
    pub aliases: Vec<String>,
    pub evidence_cutoff_seq: Option<u64>,
    pub commit: Option<String>,
    /// 未知 envelope 键：保留（规范 envelope-format.md §2.2）。
    pub extra: BTreeMap<String, serde_json::Value>,
}

pub fn new_uuid_v7() -> String {
    uuid::Uuid::now_v7().hyphenated().to_string()
}

fn take_str(
    obj: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Option<String> {
    obj.get(key).and_then(|v| v.as_str()).map(|s| s.to_string())
}

impl Envelope {
    /// 从 frontmatter 的 envelope 块解析（响亮失败）。
    pub fn from_json(v: &serde_json::Value) -> Result<Self, String> {
        let obj = v
            .as_object()
            .ok_or_else(|| "envelope 必须是 JSON object".to_string())?;
        let id = take_str(obj, "id").ok_or("envelope.id 缺失")?;
        let r#type = take_str(obj, "type").ok_or("envelope.type 缺失")?;
        let created = take_str(obj, "created").ok_or("envelope.created 缺失")?;
        let modified = take_str(obj, "modified").ok_or("envelope.modified 缺失")?;
        let version = obj
            .get("version")
            .and_then(|v| v.as_u64())
            .ok_or("envelope.version 缺失或非法（必须为正整数）")?;
        if version == 0 {
            return Err("envelope.version 必须 ≥ 1".into());
        }
        let principal = match obj.get("principal") {
            None | Some(serde_json::Value::Null) => None,
            Some(serde_json::Value::String(s)) => Some(s.clone()),
            Some(_) => return Err("envelope.principal 必须是字符串或 null".into()),
        };
        let mut refs = Vec::new();
        match obj.get("refs") {
            None | Some(serde_json::Value::Null) => {}
            Some(serde_json::Value::Array(a)) => {
                for r in a {
                    let ro = r
                        .as_object()
                        .ok_or_else(|| "refs 元素必须是 object".to_string())?;
                    refs.push(Ref {
                        rel: ro.get("rel").and_then(|v| v.as_str()).ok_or("refs[].rel 缺失")?.to_string(),
                        target: ro
                            .get("target")
                            .and_then(|v| v.as_str())
                            .ok_or("refs[].target 缺失")?
                            .to_string(),
                    });
                }
            }
            Some(_) => return Err("envelope.refs 必须是数组".into()),
        }
        let aliases = match obj.get("aliases") {
            None | Some(serde_json::Value::Null) => Vec::new(),
            Some(serde_json::Value::Array(a)) => {
                let mut out = Vec::new();
                for v in a {
                    // M7：非字符串元素此前静默变 ""——R6.1 要求可识别，响亮拒收。
                    out.push(v.as_str().ok_or_else(|| {
                        "envelope.aliases 元素必须是字符串（R6.1，不静默）".to_string()
                    })?.to_string());
                }
                out
            }
            Some(_) => return Err("envelope.aliases 必须是数组".into()),
        };
        let evidence_cutoff_seq = match obj.get("evidence_cutoff_seq") {
            None | Some(serde_json::Value::Null) => None,
            Some(serde_json::Value::Number(n)) => {
                Some(n.as_u64().ok_or("evidence_cutoff_seq 必须为非负整数")?)
            }
            Some(_) => return Err("envelope.evidence_cutoff_seq 必须是整数或 null".into()),
        };
        let commit = take_str(obj, "commit");

        // 未知键保留。
        const KNOWN: &[&str] = &[
            "id", "type", "created", "modified", "version", "principal", "refs",
            "aliases", "evidence_cutoff_seq", "commit",
        ];
        let mut extra = BTreeMap::new();
        for (k, v) in obj {
            if !KNOWN.contains(&k.as_str()) {
                extra.insert(k.clone(), v.clone());
            }
        }

        Ok(Self {
            id,
            r#type,
            created,
            modified,
            version,
            principal,
            refs,
            aliases,
            evidence_cutoff_seq,
            commit,
            extra,
        })
    }

    /// 确定性 JSON（serde_json 默认 Map = BTreeMap → 键递归字母序；数组保持顺序）。
    pub fn to_json_value(&self) -> serde_json::Value {
        use serde_json::json;
        let mut o = serde_json::Map::new();
        o.insert("id".into(), json!(self.id));
        o.insert("type".into(), json!(self.r#type));
        o.insert("created".into(), json!(self.created));
        o.insert("modified".into(), json!(self.modified));
        o.insert("version".into(), json!(self.version));
        o.insert(
            "principal".into(),
            match &self.principal {
                Some(p) => json!(p),
                None => serde_json::Value::Null,
            },
        );
        o.insert(
            "refs".into(),
            serde_json::Value::Array(
                self.refs
                    .iter()
                    .map(|r| json!({ "rel": r.rel, "target": r.target }))
                    .collect(),
            ),
        );
        o.insert(
            "aliases".into(),
            serde_json::Value::Array(self.aliases.iter().map(|a| json!(a)).collect()),
        );
        o.insert(
            "evidence_cutoff_seq".into(),
            match self.evidence_cutoff_seq {
                Some(s) => json!(s),
                None => serde_json::Value::Null,
            },
        );
        o.insert(
            "commit".into(),
            match &self.commit {
                Some(c) => json!(c),
                None => serde_json::Value::Null,
            },
        );
        for (k, v) in &self.extra {
            o.insert(k.clone(), v.clone());
        }
        serde_json::Value::Object(o)
    }
}
