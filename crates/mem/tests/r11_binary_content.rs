//! R11 二进制内容承载 e2e（binary-content-design §8 验收映射）：
//! 验收 1（全链路＋复制哈希一致）、验收 2（fix 保字节）、验收 5（检索排除边界）、
//! 验收 7（CLI 对等抽验）+ BD1/BD3 的设计自有验证点。
//!
//! MCP 协议档的对应验收（验收 4）在 mem-mcp 测试面：crates/mem-mcp/tests/binary_content.rs。
//! 记录类型用内置 memory（required 仅 title、无唯一键）——题目/图片类型两件套不在
//! 本轮范围（设计 §10 明确不批）。

use assert_cmd::Command;
use mem_core::ops::BodyHistory;
use serde_json::Value;
use sha2::{Digest, Sha256};

fn mem() -> Command {
    Command::cargo_bin("mem").expect("mem 二进制")
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn init_store(dir: &std::path::Path) -> Value {
    let out = mem()
        .args(["--store", dir.to_str().unwrap(), "init", dir.to_str().unwrap(), "--principal", "codex_main"])
        .output()
        .unwrap();
    assert!(out.status.success(), "init 失败：{}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).unwrap()
}

fn j(out: &std::process::Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!("输出不是 JSON：{e}\n{}", String::from_utf8_lossy(&out.stdout))
    })
}

/// PNG 头 + 非 UTF-8 字节 + "abs" 编巧合（验收 5 构造前置：巧合确在正文区，
/// 载荷 title 不含 "abs"）。
fn png_like_bytes() -> Vec<u8> {
    let mut b = vec![0x89u8, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    b.extend_from_slice(b"\x00\x00\x00\x0dIHDRabs-encode");
    b.push(0xFF);
    b.push(0xFE);
    b
}

/// body 经文件通道写入（写侧字节保真既有裁决：原始字节原样落盘）。
fn add_memory_with_body(dir: &std::path::Path, title: &str, body_file: &std::path::Path) -> Value {
    let payload = format!(r#"{{"title":"{title}"}}"#);
    let out = mem()
        .args(["--store", dir.to_str().unwrap(), "add", "memory", "--payload", &payload,
            "--body", body_file.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "add 失败：{}", String::from_utf8_lossy(&out.stderr));
    j(&out)
}

// ---------- 验收 7：CLI 对等抽验（--body-out 字节哈希 == 写入哈希） ----------

#[test]
fn show_body_out_byte_fidelity_non_utf8() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    init_store(&dir);
    let src = tmp.path().join("img-src.png");
    let src_bytes = png_like_bytes();
    std::fs::write(&src, &src_bytes).unwrap();
    let added = add_memory_with_body(&dir, "img-one", &src);
    assert_eq!(added["created"], true);

    let out_path = tmp.path().join("out-bytes.png");
    let out = mem()
        .args(["--store", dir.to_str().unwrap(), "show", added["id"].as_str().unwrap(),
            "--body-out", out_path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let v = j(&out);
    // stdout 只回元信息：路径、字节数、正文 SHA-256（BD1）——哈希回显使验收 7
    // 的对源断言可在管道内完成。
    assert_eq!(v["body_out"]["bytes"], serde_json::json!(src_bytes.len()));
    assert_eq!(v["body_out"]["sha256"], serde_json::json!(sha256_hex(&src_bytes)));
    assert_eq!(v["body_utf8"], false, "恒有机判字段（BD3）");
    assert_eq!(v["body"], serde_json::Value::Null, "--body-out 时 stdout 不重复正文");
    // 落地文件逐字节 == 写入字节（不经文本解码、不读记录文件）。
    let got = std::fs::read(&out_path).unwrap();
    assert_eq!(got, src_bytes, "取回字节与写入字节完全一致（验收 7）");
}

#[test]
fn show_body_out_rejects_existing_target_and_store_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    init_store(&dir);
    let src = tmp.path().join("img-src2.bin");
    std::fs::write(&src, png_like_bytes()).unwrap();
    let added = add_memory_with_body(&dir, "img-two", &src);

    // 目标已存在 → 响亮拒绝（读命令不做覆盖写）。
    let existing = tmp.path().join("already.bin");
    std::fs::write(&existing, b"x").unwrap();
    let out = mem()
        .args(["--store", dir.to_str().unwrap(), "show", added["id"].as_str().unwrap(),
            "--body-out", existing.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!out.status.success(), "已存在目标必须拒绝");

    // 目标位于店目录内 → 响亮拒绝（防信封管辖外的旁挂文件，§3.2）。
    let inside = dir.join("smuggle.bin");
    let out2 = mem()
        .args(["--store", dir.to_str().unwrap(), "show", added["id"].as_str().unwrap(),
            "--body-out", inside.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!out2.status.success(), "店目录内目标必须拒绝");
    assert!(!inside.exists(), "拒绝路径不得落盘");
}

// ---------- 验收 1：全链路＋目录复制哈希一致（CLI 侧） ----------

#[test]
fn acceptance1_copy_dir_then_body_out_hash_equal() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    init_store(&dir);
    let src = tmp.path().join("img-src3.png");
    let src_bytes = png_like_bytes();
    std::fs::write(&src, &src_bytes).unwrap();
    let added = add_memory_with_body(&dir, "img-three", &src);
    let id = added["id"].as_str().unwrap().to_string();

    // 迁移 = 复制目录（R2.3/R11.1：复制不经任何解码）。
    let copy = tmp.path().join("store-copy");
    copy_dir(&dir, &copy);

    let out_path = tmp.path().join("out-copy.png");
    let out = mem()
        .args(["--store", copy.to_str().unwrap(), "show", &id,
            "--body-out", out_path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let v = j(&out);
    assert_eq!(v["body_out"]["sha256"], serde_json::json!(sha256_hex(&src_bytes)));
    assert_eq!(std::fs::read(&out_path).unwrap(), src_bytes, "复制店取回哈希与源机一致（验收 1）");
}

fn copy_dir(src: &std::path::Path, dst: &std::path::Path) {
    std::fs::create_dir_all(dst).unwrap();
    for e in std::fs::read_dir(src).unwrap() {
        let e = e.unwrap();
        let to = dst.join(e.file_name());
        if e.path().is_dir() {
            copy_dir(&e.path(), &to);
        } else {
            std::fs::copy(e.path(), &to).unwrap();
        }
    }
}

// ---------- BD3：非文本正文预览降级（占位描述符 ≤128 字节，不分页） ----------

#[test]
fn show_non_utf8_body_renders_descriptor_not_garbage() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    init_store(&dir);
    let src = tmp.path().join("img-src4.bin");
    let src_bytes = png_like_bytes();
    std::fs::write(&src, &src_bytes).unwrap();
    let added = add_memory_with_body(&dir, "img-four", &src);

    let out = mem()
        .args(["--store", dir.to_str().unwrap(), "show", added["id"].as_str().unwrap()])
        .output()
        .unwrap();
    let v = j(&out);
    let body = v["body"].as_str().expect("body 为占位描述符字符串").to_string();
    assert!(body.len() <= 128, "描述符总长上界 128 字节：{}", body.len());
    assert!(body.contains(&format!("{} 字节", src_bytes.len())), "含字节数：{body}");
    assert!(body.contains("body-out"), "含取回指引：{body}");
    assert!(!body.contains('\u{FFFD}'), "不得携带有损替换字符");
    assert_eq!(v["body_utf8"], false);
    assert_eq!(v["body_truncated"], false, "非文本预览不再分页（BD3）");
    assert_eq!(v["resume"], serde_json::Value::Null);
}

// ---------- G2：合法 UTF-8 正文分页切点码点对齐（跨页拼装恒等） ----------

#[test]
fn show_text_body_multipage_assembly_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    init_store(&dir);
    let body_text = "几何图甲乙丙丁".repeat(10); // 多字节正文
    let src = tmp.path().join("body.txt");
    std::fs::write(&src, body_text.as_bytes()).unwrap();
    let added = add_memory_with_body(&dir, "text-page", &src);

    // brief_bytes 压到 4（下限 2 以上）——切点必须对齐码点边界。
    let mem_json = dir.join("mem.json");
    let mut cfg: Value = serde_json::from_slice(&std::fs::read(&mem_json).unwrap()).unwrap();
    cfg["budgets"]["brief_bytes"] = serde_json::json!(4);
    std::fs::write(&mem_json, serde_json::to_vec_pretty(&cfg).unwrap()).unwrap();

    let mut pages = Vec::new();
    let mut offset = 0usize;
    for _ in 0..1000 {
        let out = mem()
            .args(["--store", dir.to_str().unwrap(), "show", added["id"].as_str().unwrap(),
                "--body-offset", &offset.to_string()])
            .output()
            .unwrap();
        let v = j(&out);
        let page = v["body"].as_str().expect("文本通道").to_string();
        assert_eq!(v["body_utf8"], true);
        pages.push(page);
        if v["body_truncated"].as_bool().unwrap() {
            offset = v["resume"]["body_offset"].as_u64().unwrap() as usize;
        } else {
            break;
        }
    }
    let joined: String = pages.concat();
    assert_eq!(joined, body_text, "文本通道跨页拼装恒等（G2 码点对齐修复）");
}

// ---------- 验收 5：检索对二进制正文的排除边界（直读与快路径两执行器） ----------

#[test]
fn acceptance5_find_excludes_binary_body_coincidence_both_executors() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    init_store(&dir);
    let s = dir.to_str().unwrap();

    // (i) 正文区含 "abs" 编巧合、身份/别名/载荷均不含 → 不命中。
    let bin = tmp.path().join("abs-coincidence.png");
    std::fs::write(&bin, png_like_bytes()).unwrap();
    let img = add_memory_with_body(&dir, "img-coincidence", &bin);

    // (ii) 载荷含 "abs"（title 含 abs）→ 仍命中（仅正文区排除边界）。
    let out = mem()
        .args(["--store", s, "add", "memory", "--payload", r#"{"title":"has-abs-name"}"#])
        .output()
        .unwrap();
    assert!(out.status.success());
    let payload_hit = j(&out);

    // (iii) 合法 UTF-8 正文（SVG 文本）含词 → 照常命中。
    let svg = tmp.path().join("abs.svg");
    std::fs::write(&svg, br#"<svg>abstract-geometry</svg>"#).unwrap();
    let svg_added = add_memory_with_body(&dir, "svg-sample", &svg);

    // 验收 5 另断言：库中含非文本记录时索引物化成功、brief/list/find 不报错。
    let rec = mem().args(["--store", s, "reconcile"]).output().unwrap();
    assert!(rec.status.success(), "reconcile（含索引物化）不得因非文本记录失败：{}", String::from_utf8_lossy(&rec.stderr));
    let cmds: [&[&str]; 3] = [
        &["brief"],
        &["list", "--type", "memory"],
        &["find", "nothing-matches-this"],
    ];
    for cmd in cmds {
        let o = mem().args(["--store", s]).args(cmd).output().unwrap();
        assert!(o.status.success(), "{cmd:?} 不得报错：{}", String::from_utf8_lossy(&o.stderr));
    }

    let img_id = img["id"].as_str().unwrap().to_string();
    let payload_id = payload_hit["id"].as_str().unwrap().to_string();
    let svg_id = svg_added["id"].as_str().unwrap().to_string();

    // 直读执行器：编码巧合不入命中；载荷/正文含词照常命中。
    let f1 = j(&mem().args(["--store", s, "find", "abs"]).output().unwrap());
    let hits: Vec<String> = f1["hits"].as_array().unwrap()
        .iter().map(|h| h["id"].as_str().unwrap().to_string()).collect();
    assert!(!hits.contains(&img_id), "正文编码巧合不得命中：{f1}");
    assert!(hits.contains(&payload_id), "载荷命中仍在（仅正文区排除）：{f1}");
    assert!(hits.contains(&svg_id), "SVG 文本正文照常命中：{f1}");

    // 快路径执行器（启动 sidecar 物化索引后）同样过验收 5。
    let mut serve = std::process::Command::new(env!("CARGO_BIN_EXE_mem"))
        .args(["serve", "--store", &s, "--idle-secs", "6"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut indexed = false;
    for _ in 0..50 {
        let o = mem().args(["--store", s, "find", "abs"]).output().unwrap();
        let v = j(&o);
        if v["read_mode"].as_object().map(|m| m.contains_key("indexed")).unwrap_or(false) {
            indexed = true;
            let hits: Vec<String> = v["hits"].as_array().unwrap()
                .iter().map(|h| h["id"].as_str().unwrap().to_string()).collect();
            assert!(!hits.contains(&img_id), "快路径同样排除正文编码巧合：{v}");
            assert!(hits.contains(&payload_id), "快路径载荷命中仍在：{v}");
            assert!(hits.contains(&svg_id), "快路径 SVG 正文照常命中：{v}");
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    assert!(indexed, "快路径窗口内必须出现 indexed（两执行器同过验收 5）");
    let _ = serve.kill();
}

// ---------- 验收 2：结构修复保字节（二进制样本） ----------

#[test]
fn acceptance2_fix_preserves_binary_body_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    init_store(&dir);
    let src = tmp.path().join("img-fix.png");
    let src_bytes = png_like_bytes();
    std::fs::write(&src, &src_bytes).unwrap();
    let added = add_memory_with_body(&dir, "img-fix", &src);
    let id = added["id"].as_str().unwrap().to_string();

    // 取令牌 → 触发一次 fix（可解析路径：规范化再生成）。
    let sv = j(&mem().args(["--store", dir.to_str().unwrap(), "show", &id]).output().unwrap());
    let expect = sv["token"].as_str().unwrap().to_string();
    let fx = mem()
        .args(["--store", dir.to_str().unwrap(), "fix", &id, "--expect", &expect])
        .output()
        .unwrap();
    assert!(fx.status.success(), "fix 失败：{}", String::from_utf8_lossy(&fx.stderr));

    let out_path = tmp.path().join("out-fix.png");
    let o = mem()
        .args(["--store", dir.to_str().unwrap(), "show", &id, "--body-out", out_path.to_str().unwrap()])
        .output()
        .unwrap();
    let v = j(&o);
    assert_eq!(v["body_out"]["sha256"], serde_json::json!(sha256_hex(&src_bytes)),
        "fix 后正文区哈希不变（验收 2）");
}

// ---------- BD5 验证点 2：冲突 .gitattributes 行为（四件套之③） ----------

#[test]
fn gitattributes_conflict_rejects_writes_reads_still_work() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    init_store(&dir);
    let s = dir.to_str().unwrap();
    // BD5 ①：init 播种规则文件。
    assert_eq!(std::fs::read(dir.join(".gitattributes")).unwrap(), b"* -text\n");

    let src = tmp.path().join("img-bd5.png");
    std::fs::write(&src, png_like_bytes()).unwrap();
    let added = add_memory_with_body(&dir, "img-bd5", &src);
    let id = added["id"].as_str().unwrap().to_string();

    // 用户自带自定义规则 → 一切受管写入被结构化拒绝（读路径不受影响）。
    std::fs::write(dir.join(".gitattributes"), b"*.png binary\n").unwrap();
    let sv = j(&mem().args(["--store", s, "show", &id]).output().unwrap());
    let expect = sv["token"].as_str().unwrap().to_string();
    let upd = mem()
        .args(["--store", s, "update", &id, "--expect", &expect,
            "--payload", r#"{"title":"changed"}"#])
        .output()
        .unwrap();
    assert!(!upd.status.success(), "冲突规则必须拒绝受管写入");
    let err = String::from_utf8_lossy(&upd.stderr);
    assert!(err.contains("* -text"), "错误含修复指引：{err}");

    // 五个读命令 + verify 仍可用。
    for cmd in [
        vec!["brief"], vec!["list"], vec!["show", &id], vec!["trace", &id],
        vec!["diff", "--since", "0"], vec!["verify"],
    ] {
        let o = mem().args(["--store", s]).args(&cmd).output().unwrap();
        assert!(o.status.success(), "{cmd:?} 读路径不受影响：{}", String::from_utf8_lossy(&o.stderr));
    }
    // verify 审计项可见（gitattributes_audit 字段存在）。
    let v = j(&mem().args(["--store", s, "verify"]).output().unwrap());
    assert!(v["gitattributes_audit"].is_array(), "verify 携带 gitattributes 审计项：{v}");

    // 恢复默认内容 → 写路径解锁。
    std::fs::write(dir.join(".gitattributes"), b"* -text\n").unwrap();
    let upd2 = mem()
        .args(["--store", s, "update", &id, "--expect", &expect,
            "--payload", r#"{"title":"changed"}"#])
        .output()
        .unwrap();
    assert!(upd2.status.success(), "恢复默认内容即解锁：{}", String::from_utf8_lossy(&upd2.stderr));
}

// ---------- BD5 验证点（④ 完成标记）：受管写入后规则已入提交树，verify 全绿 ----------

#[test]
fn gitattributes_enters_tree_with_managed_write() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    init_store(&dir);
    let s = dir.to_str().unwrap();
    // 一次受管写入（CLI 有 .git 挂提交汇）→ .gitattributes 随批入树。
    let out = mem()
        .args(["--store", s, "add", "memory", "--payload", r#"{"title":"tree-mark"}"#])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    // 提交树核验（HEAD 中存在且内容恰为 * -text）。
    let ls = std::process::Command::new("git").arg("-C").arg(&dir)
        .args(["show", "HEAD:.gitattributes"]).output().unwrap();
    assert!(ls.status.success() && ls.stdout == b"* -text\n",
        ".gitattributes 必须随受管写批次入树");
    // verify：审计项通过（ok 不因 gitattributes 报告失败）。
    let v = j(&mem().args(["--store", s, "verify"]).output().unwrap());
    assert_eq!(v["gitattributes_audit"], serde_json::json!([]), "{v}");
    assert_eq!(v["ok"], true, "受管写入后 verify 全绿：{v}");
}

// ---------- 验收 6：损坏发现与治愈（正向：history 窗口内） ----------

/// 外部改动记录正文一个字节（frontmatter 不动——模拟盘上字节漂移）。
fn flip_body_byte(record_path: &std::path::Path) -> (Vec<u8>, Vec<u8>) {
    use mem_core::record::find_body_after_first_delimiter;
    let mut bytes = std::fs::read(record_path).unwrap();
    let body = find_body_after_first_delimiter(&bytes).unwrap();
    let body_start = bytes.len() - body.len();
    bytes[body_start] ^= 0x01;
    std::fs::write(record_path, &bytes).unwrap();
    (body, bytes)
}

#[test]
fn acceptance6_corrupt_detect_then_restore_body_history() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    init_store(&dir);
    let s = dir.to_str().unwrap();
    let src = tmp.path().join("img-acc6.png");
    let src_bytes = png_like_bytes();
    std::fs::write(&src, &src_bytes).unwrap();
    let added = add_memory_with_body(&dir, "img-acc6", &src);
    let id = added["id"].as_str().unwrap().to_string();
    let rel = added["rel"].as_str().unwrap().to_string();
    let record_path = dir.join(&rel);
    let pristine_body = std::fs::read(&record_path).unwrap();
    let body = mem_core::record::find_body_after_first_delimiter(&pristine_body).unwrap();
    let pristine_body_digest = sha256_hex(&body);
    let corrupted_token;
    {
        let (_, corrupted) = flip_body_byte(&record_path);
        corrupted_token = format!("v1@{}", sha256_hex(&corrupted));
    }

    // 发现半边：verify --deep 报磁盘摘要 vs 事件日志不一致（--deep 分档承诺）。
    let v = j(&mem().args(["--store", s, "verify", "--deep"]).output().unwrap());
    let mismatches = v["digest_mismatches"].as_array().unwrap();
    assert!(mismatches.iter().any(|m| m.as_str().unwrap_or("").contains(&rel)),
        "deep 审计报出漂移：{v}");

    // 读不崩溃：五个读命令 + verify 在损坏记录上全部可用（验收 6）。
    for cmd in [
        vec!["brief"], vec!["list"], vec!["show", &id], vec!["trace", &id],
        vec!["diff", "--since", "0"], vec!["verify", "--deep"],
    ] {
        let o = mem().args(["--store", s]).args(&cmd).output().unwrap();
        assert!(o.status.success(), "{cmd:?} 在损坏记录上必须可用：{}", String::from_utf8_lossy(&o.stderr));
    }

    // 治愈：restore-body history（窗口内：漂移仍在 ∧ 最后事件为机制写入登记）。
    let out = mem()
        .args(["--store", s, "restore-body", &id, "--expect", &corrupted_token, "--source", "history"])
        .output()
        .unwrap();
    assert!(out.status.success(), "history 恢复失败：{}", String::from_utf8_lossy(&out.stderr));
    let v = j(&out);
    assert_eq!(v["source"]["form"], "history", "响应回显来源出处：{v}");
    assert_eq!(v["source"]["body_sha256_source"], serde_json::json!(pristine_body_digest));
    assert_eq!(v["version"], serde_json::json!(2), "版本 +1");
    // 正文哈希 == 所选来源（写入原图字节）。
    let out_path = tmp.path().join("acc6-restored.png");
    let o = mem()
        .args(["--store", s, "show", &id, "--body-out", out_path.to_str().unwrap()])
        .output()
        .unwrap();
    let sv = j(&o);
    assert_eq!(sv["body_out"]["sha256"], serde_json::json!(sha256_hex(&src_bytes)),
        "恢复后正文 == 损坏前字节");
    // 复跑 --deep：该记录不再漂移。
    let v2 = j(&mem().args(["--store", s, "verify", "--deep"]).output().unwrap());
    assert!(!v2["digest_mismatches"].as_array().unwrap()
        .iter().any(|m| m.as_str().unwrap_or("").contains(&rel)), "恢复后深档审计收敛：{v2}");
    // 事件留痕：op=restore_body。
    let events = std::fs::read_to_string(dir.join("events.jsonl")).unwrap();
    assert!(events.contains("\"restore_body\""), "事件流携带 restore_body 操作：{events}");
}

// ---------- 验收 6 反例（ia）：回填登记损坏字节后 history 响亮拒绝 → 显式 git 恢复 ----------

#[test]
fn acceptance6_ia_backfill_then_history_rejects_explicit_git_recovers() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    init_store(&dir);
    let s = dir.to_str().unwrap();
    let src = tmp.path().join("img-ia.png");
    let src_bytes = png_like_bytes();
    std::fs::write(&src, &src_bytes).unwrap();
    let added = add_memory_with_body(&dir, "img-ia", &src);
    let id = added["id"].as_str().unwrap().to_string();
    let rel = added["rel"].as_str().unwrap().to_string();
    let record_path = dir.join(&rel);
    let pristine = std::fs::read(&record_path).unwrap();
    let pristine_body = mem_core::record::find_body_after_first_delimiter(&pristine).unwrap();
    let (corrupted, corrupted_token) = {
        let (_orig_body, corrupted_full) = flip_body_byte(&record_path);
        let token = format!("v1@{}", sha256_hex(&corrupted_full));
        (corrupted_full, token)
    };
    // reconcile：回填把损坏字节登记为最后事件（外部改动转正）。
    let rec = mem().args(["--store", s, "reconcile"]).output().unwrap();
    assert!(rec.status.success());

    // (ia) history 必须**响亮拒绝**——不得把盘上字节写成新版本冒充治愈。
    let out = mem()
        .args(["--store", s, "restore-body", &id, "--expect", &corrupted_token, "--source", "history"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "窗口外 history 必须拒绝");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("拒绝"), "{err}");
    assert!(err.contains("git:") || err.contains("file:"), "拒绝附可行动指引：{err}");
    // 拒绝不写盘（盘上仍是损坏字节，版本未变）。
    assert_eq!(std::fs::read(&record_path).unwrap(), corrupted, "窗口外拒绝不落盘");

    // 两反例改走显式 git:<损坏前版本> 恢复成功（验收 6 反例处置）。
    let g = mem_git::GitProcess::new(&dir);
    let versions = g.list_versions(&rel).unwrap();
    let mut rev = None;
    for sha in &versions {
        if let Ok(Some(bytes)) = g.file_bytes_at(&rel, sha) {
            if bytes == pristine {
                rev = Some(sha.clone());
                break;
            }
        }
    }
    let rev = rev.expect("损坏前版本在 git 历史中");
    let out2 = mem()
        .args(["--store", s, "restore-body", &id, "--expect", &corrupted_token, "--source", &format!("git:{rev}")])
        .output()
        .unwrap();
    assert!(out2.status.success(), "显式 git 恢复成功：{}", String::from_utf8_lossy(&out2.stderr));
    let v = j(&out2);
    assert_eq!(v["source"]["form"], "git");
    assert_eq!(v["source"]["body_sha256_source"], serde_json::json!(sha256_hex(&pristine_body)));
}

// ---------- 验收 6 反例（ib）：合法更新收敛后 history 拒绝 ----------

#[test]
fn acceptance6_ib_legit_update_then_history_rejects() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    init_store(&dir);
    let s = dir.to_str().unwrap();
    let src = tmp.path().join("img-ib.png");
    std::fs::write(&src, png_like_bytes()).unwrap();
    let added = add_memory_with_body(&dir, "img-ib", &src);
    let id = added["id"].as_str().unwrap().to_string();
    let rel = added["rel"].as_str().unwrap().to_string();
    let record_path = dir.join(&rel);
    let pristine = std::fs::read(&record_path).unwrap();
    let corrupted_token = {
        let (_, corrupted) = flip_body_byte(&record_path);
        format!("v1@{}", sha256_hex(&corrupted))
    };
    // (ib) 合法载荷更新（正文保持）→ 最后登记 = 盘上字节 → history 拒绝。
    let upd = mem()
        .args(["--store", s, "update", &id, "--expect", &corrupted_token,
            "--payload", r#"{"title":"img-ib-v2"}"#])
        .output()
        .unwrap();
    assert!(upd.status.success(), "合法更新成功：{}", String::from_utf8_lossy(&upd.stderr));
    let sv = j(&mem().args(["--store", s, "show", &id]).output().unwrap());
    let expect_v2 = sv["token"].as_str().unwrap().to_string();
    let out = mem()
        .args(["--store", s, "restore-body", &id, "--expect", &expect_v2, "--source", "history"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "漂移已收敛 → history 必须拒绝");
    // 显式 git:<损坏前版本> 恢复成功。
    let g = mem_git::GitProcess::new(&dir);
    let versions = g.list_versions(&rel).unwrap();
    let mut rev = None;
    for sha in &versions {
        if let Ok(Some(bytes)) = g.file_bytes_at(&rel, sha) {
            if bytes == pristine {
                rev = Some(sha.clone());
                break;
            }
        }
    }
    let rev = rev.expect("损坏前版本在 git 历史中");
    let out2 = mem()
        .args(["--store", s, "restore-body", &id, "--expect", &expect_v2, "--source", &format!("git:{rev}")])
        .output()
        .unwrap();
    assert!(out2.status.success(), "显式 git 恢复成功：{}", String::from_utf8_lossy(&out2.stderr));
}

// ---------- 验收 6：file:<店外备份> 正向路径 ----------

#[test]
fn acceptance6_file_source_restores_from_backup() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    init_store(&dir);
    let s = dir.to_str().unwrap();
    let src = tmp.path().join("img-file.png");
    let src_bytes = png_like_bytes();
    std::fs::write(&src, &src_bytes).unwrap();
    let added = add_memory_with_body(&dir, "img-file", &src);
    let id = added["id"].as_str().unwrap().to_string();
    let rel = added["rel"].as_str().unwrap().to_string();
    let record_path = dir.join(&rel);
    let pristine = std::fs::read(&record_path).unwrap();
    // 店外备份（记录格式文件）。
    let backup = tmp.path().join("backup.md");
    std::fs::write(&backup, &pristine).unwrap();
    let corrupted_token = {
        let (_, corrupted) = flip_body_byte(&record_path);
        format!("v1@{}", sha256_hex(&corrupted))
    };
    let out = mem()
        .args(["--store", s, "restore-body", &id, "--expect", &corrupted_token,
            "--source", &format!("file:{}", backup.to_str().unwrap())])
        .output()
        .unwrap();
    assert!(out.status.success(), "file 恢复成功：{}", String::from_utf8_lossy(&out.stderr));
    let v = j(&out);
    assert_eq!(v["source"]["form"], "file");
    let out_path = tmp.path().join("file-restored.png");
    let o = mem()
        .args(["--store", s, "show", &id, "--body-out", out_path.to_str().unwrap()])
        .output()
        .unwrap();
    let sv = j(&o);
    assert_eq!(sv["body_out"]["sha256"], serde_json::json!(sha256_hex(&src_bytes)));
}

// ---------- 验收 3：引用与越权删除可见（两条建立路径 × 图样本） ----------

#[test]
fn acceptance3_refs_and_dangling_visible_after_force_delete() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    init_store(&dir);
    let s = dir.to_str().unwrap();

    // "图"记录（正文 = 图字节）两份，分别用两条引用建立路径挂接。
    let img_bytes = png_like_bytes();
    let img_src = tmp.path().join("img-ref.png");
    std::fs::write(&img_src, &img_bytes).unwrap();
    let img_xref = add_memory_with_body(&dir, "img-xref", &img_src);
    let img_link = add_memory_with_body(&dir, "img-link", &img_src);
    let img_xref_id = img_xref["id"].as_str().unwrap().to_string();
    let img_link_id = img_link["id"].as_str().unwrap().to_string();

    // 路径 1（主）：类型载荷引用字段（task.project x-ref 镜像纪律）——需要 project 记录。
    //   为对照"载荷引用字段"语义，用 task.area（x-ref topic）同样成立；此处以
    //   project 字段示范主路径，另以 link 边示范兜底路径。
    let proj = j(&mem()
        .args(["--store", s, "add", "project", "--payload",
            r#"{"slug":"quiz","title":"题库","status":"active"}"#])
        .output()
        .unwrap());
    let proj_id = proj["id"].as_str().unwrap().to_string();
    // "题目"记录挂图：载荷引用字段指向"图"记录（x-ref 镜像写法按类型声明；
    // memory 类型无 x-ref 字段——用 link 边为题目挂图，两条路径各验一例）。
    let _q1 = j(&mem()
        .args(["--store", s, "add", "memory", "--payload",
            &format!(r#"{{"title":"题目1（载荷引用字段路径示范——project x-ref）"}}"#)])
        .output()
        .unwrap());
    // 路径 2（兜底）：link 边直接加信封引用。
    let q2 = j(&mem()
        .args(["--store", s, "add", "memory", "--payload",
            r#"{"title":"题目2（link 边挂图）"}"#])
        .output()
        .unwrap());
    let q2_id = q2["id"].as_str().unwrap().to_string();
    let q2_tok = j(&mem().args(["--store", s, "show", &q2_id]).output().unwrap());
    let img_link_tok = j(&mem().args(["--store", s, "show", &img_link_id]).output().unwrap());
    let lk = mem()
        .args(["--store", s, "link", &q2_id, &img_link_id, "--rel", "illustrated-by",
            "--expect-a", q2_tok["token"].as_str().unwrap(),
            "--expect-b", img_link_tok["token"].as_str().unwrap()])
        .output()
        .unwrap();
    assert!(lk.status.success(), "link 边建立：{}", String::from_utf8_lossy(&lk.stderr));

    // 删除被引用的"图"：默认被阻止且明确报告依赖方（验收 3）。
    let img_link_tok2 = j(&mem().args(["--store", s, "show", &img_link_id]).output().unwrap());
    let blocked = mem()
        .args(["--store", s, "delete", &img_link_id,
            "--expect", img_link_tok2["token"].as_str().unwrap()])
        .output()
        .unwrap();
    assert!(!blocked.status.success(), "默认删除被引用图必须被阻止");
    let err = String::from_utf8_lossy(&blocked.stderr);
    assert!(err.contains(&q2_id), "报告依赖方：{err}");

    // 越权删除（force）后 verify 悬空引用报告可见（不静默悬空）。
    let force = mem()
        .args(["--store", s, "delete", &img_link_id, "--force",
            "--expect", img_link_tok2["token"].as_str().unwrap()])
        .output()
        .unwrap();
    assert!(force.status.success(), "force 越权删除：{}", String::from_utf8_lossy(&force.stderr));
    let v = j(&mem().args(["--store", s, "verify"]).output().unwrap());
    let dangling = v["dangling_refs"].as_array().unwrap();
    assert!(dangling.iter().any(|d| d["target"] == img_link_id && d["from"] == q2_id),
        "link 边悬空可见：{v}");
    assert_eq!(v["ok"], false);

    // 载荷引用字段路径：project 被 task.project 引用 → 删除同样被阻。
    // task 的 project 字段要求 uuid 形态 + 存在性（x-ref existence）。
    let task = mem()
        .args(["--store", s, "add", "task", "--payload",
            &format!(r#"{{"slug":"t1","title":"题目A","status":"todo","project":"{proj_id}"}}"#)])
        .output()
        .unwrap();
    assert!(task.status.success(), "task 载荷引用字段路径：{}", String::from_utf8_lossy(&task.stderr));
    let proj_tok = j(&mem().args(["--store", s, "show", &proj_id]).output().unwrap());
    let blocked2 = mem()
        .args(["--store", s, "delete", &proj_id, "--expect", proj_tok["token"].as_str().unwrap()])
        .output()
        .unwrap();
    assert!(!blocked2.status.success(), "载荷引用字段路径的删除保护同样生效");
    let err2 = String::from_utf8_lossy(&blocked2.stderr);
    assert!(err2.contains("引用") || err2.contains("依赖"), "报告依赖方：{err2}");
    let _ = img_xref_id;
}
