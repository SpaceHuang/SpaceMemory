//! 正文域判定与分页单源（G1/G2，R11.2/R11.4；binary-content-design §2/§3.4）。
//!
//! **G1 判定单源**："正文区是否合法 UTF-8"只在本模块实现一次（输入=正文区全体
//! 字节），四个消费方（检索参与门槛、正文分页通道选择、文本预览降级、索引物化）
//! 只调用不复写——本项目评审史反复复现"两套实现必然漂移"。
//!
//! **G2 字节不经过文本域**：一切取回通道以原始字节为源。正文区合法 UTF-8 时，
//! 文本通道在数学上字节保真（合法 UTF-8 字节 ↔ JSON 字符串往返恒等）；不合法时
//! 文本通道不可能保真，必须走字节通道（CLI 落文件 / MCP base64 分页）。
//!
//! 文本分页切点对齐码点边界（既有行为修订：此前按任意字节切再有损解码，跨页
//! 拼装会丢页边界处的字节）——页实际字节数可能少于预算至多 3 字节；续读偏移
//! 仍按字节计（R11.2 迁移节）。

/// **G1 单源判定**：正文区是否合法 UTF-8。
pub fn is_valid_utf8(body: &[u8]) -> bool {
    std::str::from_utf8(body).is_ok()
}

/// G1 的取值形态：合法 → Some（零拷贝借用）；不合法 → None（正文域不参与文本
/// 处理——检索不匹配、素材列写空串、预览降级、分页走字节通道）。
pub fn text_or_none(body: &[u8]) -> Option<&str> {
    std::str::from_utf8(body).ok()
}

/// 文本分页（G2：切点对齐码点边界；仅合法 UTF-8 正文可用）。
///
/// 返回 None = 正文非合法 UTF-8（调用方必须走 [`body_page_bytes`] 字节通道——
/// 本函数绝不有损解码）。offset 为字节偏移（resume.body_offset 原样回传）；
/// 页实际字节数可能少于预算至多 3 字节（码点边界回退）。
///
/// 病态配置守则：byte_budget 小于首个码点宽度时，页**至少含一个码点**（页可能
/// 超预算至多 2 字节）——空页 + 原偏移续读会让调用方死循环，比预算微超更劣；
/// 预算下限 2 的常规配置不触发（r12 B7-3 下限校验单源）。
pub fn body_page_text(
    body: &[u8],
    offset: usize,
    byte_budget: usize,
) -> Option<(String, bool, Option<usize>)> {
    if !is_valid_utf8(body) {
        return None;
    }
    let text = std::str::from_utf8(body).expect("is_valid_utf8 已判定");
    let off = offset.min(text.len());
    // 起点必须落在码点边界（合法 UTF-8 中非边界起点 = 调用方构造的偏移）：
    // 前进到下一码点边界（不回退——回退会重发上一页尾部字节）。
    let off = {
        let mut o = off;
        while o < text.len() && !text.is_char_boundary(o) {
            o += 1;
        }
        o
    };
    let rest_len = text.len() - off;
    let mut take = rest_len.min(byte_budget);
    while take > 0 && !text.is_char_boundary(off + take) {
        take -= 1;
    }
    if take == 0 && rest_len > 0 {
        // 病态配置（预算 < 首码点宽度）：至少一个码点，绝不死循环。
        take = 1;
        while !text.is_char_boundary(off + take) {
            take += 1;
        }
    }
    let truncated = rest_len > take;
    let next = if truncated { Some(off + take) } else { None };
    Some((text[off..off + take].to_string(), truncated, next))
}

/// 字节分页（G2 字节通道：任意字节按原始字节分页；页 = byte_budget 个原始字节）。
/// 返回 (本页字节, 是否截断, 下一偏移)；调用方选择保真交付形态（CLI 落文件 /
/// MCP base64）。
pub fn body_page_bytes(body: &[u8], offset: usize, byte_budget: usize) -> (&[u8], bool, Option<usize>) {
    let off = offset.min(body.len());
    let rest = &body[off..];
    let take = rest.len().min(byte_budget);
    let truncated = rest.len() > take;
    let next = if truncated { Some(off + take) } else { None };
    (&rest[..take], truncated, next)
}

/// 字节数的限位显示（H1，实施评审 r3）：字节数的十进制位数不进模板上界——
/// <1MB 用原数字（≤6 位）；≥1MB 用整数工程缩写（"1MB"/"1025MB"/"18EB"，≤6 字节）。
/// 任意 usize 恒 ≤6 字节 → 描述符总长对任意正文长度成立。
fn byte_count_display(n: usize) -> String {
    if n < 1_000_000 {
        return n.to_string();
    }
    let (v, unit) = if n < 1_000_000_000 {
        (n as f64 / 1e6, "MB")
    } else if n < 1_000_000_000_000 {
        (n as f64 / 1e9, "GB")
    } else if n < 1_000_000_000_000_000 {
        (n as f64 / 1e12, "TB")
    } else if n < 1_000_000_000_000_000_000 {
        (n as f64 / 1e15, "PB")
    } else {
        (n as f64 / 1e18, "EB")
    };
    format!("{v:.0}{unit}")
}

/// 非文本正文的占位描述符核心（BD3，模板固定：字节数＋正文摘要前缀＋取回指引；
/// 总长上界 128 字节——对**任意**字节数成立，H1：模板固定部分＋限位后的字节数
/// ≤128，debug_assert 保留）。机制对正文零解释——描述符只描述存在，绝不携带有损字节。
fn descriptor_for(record_id: &str, body_len: usize, digest_prefix: &str) -> String {
    let d = format!(
        "[非文本正文 {} 字节 sha256:{digest_prefix}…，取回：mem show {record_id} --body-out <路径>]",
        byte_count_display(body_len),
    );
    debug_assert!(d.len() <= 128, "描述符模板上界 128 字节（BD3）：{}", d.len());
    d
}

/// 非文本正文的占位描述符（BD3）。
pub fn non_text_descriptor(record_id: &str, body: &[u8]) -> String {
    use crate::token::sha256_hex;
    let digest = sha256_hex(body);
    descriptor_for(record_id, body.len(), &digest[..6])
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------- G1 单源判定 ----------

    #[test]
    fn g1_predicate_ascii_utf8_and_multibyte_valid() {
        assert!(is_valid_utf8(b""));
        assert!(is_valid_utf8(b"hello"));
        assert!(is_valid_utf8("几何图".as_bytes()));
        assert!(!is_valid_utf8(&[0xFF, 0xFE]));
        assert!(!is_valid_utf8(&[b'a', 0x80]));
        // PNG 头（\x89PNG）含非法 UTF-8 序列。
        assert!(!is_valid_utf8(&[0x89, b'P', b'N', b'G']));
    }

    #[test]
    fn g1_text_or_none_matches_predicate() {
        assert_eq!(text_or_none(b"abc"), Some("abc"));
        assert_eq!(text_or_none(&[0xFF]), None);
    }

    // ---------- 文本通道：切点码点对齐（G2） ----------

    #[test]
    fn text_page_ascii_budget_exact_cut() {
        let body = b"abcdefgh";
        let (page, truncated, next) = body_page_text(body, 0, 4).unwrap();
        assert_eq!(page, "abcd");
        assert!(truncated);
        assert_eq!(next, Some(4));
        // 跨页拼装恒等（R11.2：合法正文文本通道分页拼装 == 原文）。
        let (page2, truncated2, next2) = body_page_text(body, 4, 4).unwrap();
        assert_eq!(page2, "efgh");
        assert!(!truncated2);
        assert_eq!(next2, None);
        let mut joined = page;
        joined.push_str(&page2);
        assert_eq!(joined, "abcdefgh");
    }

    #[test]
    fn text_page_multibyte_cut_aligns_to_codepoint() {
        // "几何图甲乙" 每字 3 字节；预算 4 → 切点 4 落在第二字中间 → 回退到边界 3
        //（设计：页实际字节数可能少于预算至多 3 字节——切点向后对齐）。
        let body = "几何图甲乙".as_bytes();
        let (page, truncated, next) = body_page_text(body, 0, 4).unwrap();
        assert_eq!(page, "几");
        assert!(truncated);
        assert_eq!(next, Some(3), "续读偏移仍按字节计");
        // 续读拼装恒等（R11.2：合法正文文本通道分页拼装 == 原文）。
        let (page2, truncated2, next2) = body_page_text(body, 3, 4).unwrap();
        assert_eq!(page2, "何");
        assert!(truncated2);
        assert_eq!(next2, Some(6));
        let (page3, _, _) = body_page_text(body, 6, 4).unwrap();
        assert_eq!(page3, "图");
        let mut joined = page;
        joined.push_str(&page2);
        joined.push_str(&page3);
        joined.push_str(&body_page_text(body, 9, 4).unwrap().0);
        joined.push_str(&body_page_text(body, 12, 4).unwrap().0);
        assert_eq!(joined, "几何图甲乙");
    }

    #[test]
    fn text_page_offset_mid_codepoint_advances_forward() {
        let body = "几何".as_bytes(); // 6 字节
        // offset=1（非边界）：前进到 3，绝不回退重发。
        let (page, truncated, next) = body_page_text(body, 1, 100).unwrap();
        assert_eq!(page, "何");
        assert!(!truncated);
        assert_eq!(next, None);
        let _ = body.len();
    }

    #[test]
    fn text_page_tiny_budget_never_livelocks() {
        // 病态配置：预算 2 < 首码点宽度 3 —— 页至少一个码点（非空页 + 前进偏移）。
        let body = "几何图".as_bytes();
        let (page, truncated, next) = body_page_text(body, 0, 2).unwrap();
        assert_eq!(page, "几");
        assert!(truncated);
        assert_eq!(next, Some(3), "next 必须前进（原偏移 + 空页 = 调用方死循环）");
    }

    #[test]
    fn text_page_empty_body_and_offset_beyond_end() {
        let (page, truncated, next) = body_page_text(b"", 0, 16).unwrap();
        assert_eq!(page, "");
        assert!(!truncated);
        assert_eq!(next, None);
        let (page, truncated, next) = body_page_text(b"abc", 99, 16).unwrap();
        assert_eq!(page, "");
        assert!(!truncated);
        assert_eq!(next, None);
    }

    // ---------- 文本通道对非文本正文关闭 ----------

    #[test]
    fn text_page_returns_none_for_invalid_utf8() {
        let bin: &[u8] = &[0x89, b'P', b'N', b'G', 0xFF, 0x00];
        assert!(body_page_text(bin, 0, 16).is_none(), "非文本正文不走文本通道（G2）");
    }

    // ---------- 字节通道 ----------

    #[test]
    fn byte_page_splits_raw_bytes_exactly() {
        let bin: Vec<u8> = (0u8..=255).collect();
        let (p1, t1, n1) = body_page_bytes(&bin, 0, 100);
        assert_eq!(p1.len(), 100);
        assert!(t1);
        assert_eq!(n1, Some(100));
        let (p2, t2, n2) = body_page_bytes(&bin, 100, 100);
        assert_eq!(p2.len(), 100, "页大小受预算钳制（256 字节正文共 3 页）");
        assert!(t2);
        assert_eq!(n2, Some(200));
        let (p3, t3, n3) = body_page_bytes(&bin, 200, 100);
        assert_eq!(p3.len(), 56);
        assert!(!t3);
        assert_eq!(n3, None);
        // 拼装恒等（字节通道逐字节保真）。
        let mut joined = p1.to_vec();
        joined.extend_from_slice(p2);
        joined.extend_from_slice(p3);
        assert_eq!(joined, bin);
    }

    #[test]
    fn byte_page_offset_clamped_beyond_end() {
        let (page, truncated, next) = body_page_bytes(b"abc", 99, 16);
        assert_eq!(page, b"");
        assert!(!truncated);
        assert_eq!(next, None);
    }

    // ---------- BD3 占位描述符 ----------

    #[test]
    fn descriptor_bounded_and_actionable() {
        let body: Vec<u8> = (0..=255).collect();
        let d = non_text_descriptor("01980000-0000-7000-8000-00000000dead", &body);
        assert!(d.len() <= 128, "总长上界 128 字节（BD3）：{} —— {d}", d.len());
        assert!(d.contains(&format!("{} 字节", body.len())), "含字节数：{d}");
        assert!(d.contains("sha256:"), "含正文摘要前缀：{d}");
        assert!(d.contains("--body-out"), "含取回指引：{d}");
        assert!(!d.contains('\u{FFFD}'), "绝不携带有损字节");
    }

    #[test]
    fn descriptor_empty_body_still_bounded() {
        let d = non_text_descriptor("id-x", b"");
        assert!(d.len() <= 128);
        assert!(d.contains("0 字节"));
    }

    // ---------- H1（实施评审 r3）：大正文档上界（位数 7 与 10 + usize 上界） ----------

    #[test]
    fn h1_descriptor_bounded_for_megabyte_and_gigabyte_and_usize_max() {
        // 1MB（7 位）、1GB（10 位）、u64/usize 上界（20 位）——原模板固定部分
        // 124 字节在 ≥1MB（131）越界，debug 构建读路径 panic（mem show 默认形态）。
        let id = "01980000-0000-7000-8000-00000000dead";
        for len in [1_048_576usize, 1_000_000_000, usize::MAX] {
            let d = descriptor_for(id, len, "ab12cd");
            assert!(d.len() <= 128, "len={len} 描述符 {} 字节 > 128：{d}", d.len());
            assert!(d.contains("--body-out"), "取回指引保留：{d}");
            assert!(d.contains("sha256:"), "摘要前缀保留：{d}");
        }
    }

    #[test]
    fn h1_byte_count_display_capped_at_six_bytes() {
        assert_eq!(byte_count_display(0), "0");
        assert_eq!(byte_count_display(999_999), "999999");
        assert_eq!(byte_count_display(1_000_000), "1MB");
        assert_eq!(byte_count_display(1_048_576), "1MB");
        assert_eq!(byte_count_display(1_000_000_000), "1GB");
        assert!(byte_count_display(usize::MAX).len() <= 6, "EB 档 ≤6 字节");
    }
}
