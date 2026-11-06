//! 时间工具：RFC 3339 UTC 时间戳（机制自有字段统一用）。
//!
//! 不引入 chrono：信封时间戳与事件行的形态由 envelope-format.md §2.2/§4.1 固定，
//! 本模块只产出该形态。

use std::time::{SystemTime, UNIX_EPOCH};

/// 当前时刻，RFC 3339 UTC（秒级精度，`…Z`）。
pub fn now_rfc3339() -> String {
    format_rfc3339(SystemTime::now())
}

/// SystemTime → RFC 3339 UTC（带毫秒；检查点等内部派生物用，单调性更好）。
pub fn format_rfc3339_millis(t: SystemTime) -> String {
    let dur = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = dur.as_secs() as i64;
    let millis = dur.subsec_millis();
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        y,
        m,
        d,
        sod / 3600,
        (sod % 3600) / 60,
        sod % 60,
        millis
    )
}

/// SystemTime → RFC 3339 UTC。
pub fn format_rfc3339(t: SystemTime) -> String {
    let dur = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = dur.as_secs() as i64;
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y,
        m,
        d,
        sod / 3600,
        (sod % 3600) / 60,
        sod % 60
    )
}

/// Howard Hinnant 的 civil_from_days（公历，1970-01-01 = day 0）。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// RFC 3339 UTC（`YYYY-MM-DDTHH:MM:SSZ`）→ epoch 秒；格式非法 → None。
pub fn parse_rfc3339_to_epoch(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() != 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' || b[19] != b'Z' {
        return None;
    }
    let num = |a: usize, n: usize| -> Option<i64> { s.get(a..a + n)?.parse::<i64>().ok() };
    let y = num(0, 4)?;
    let m = num(5, 2)?;
    let d = num(8, 2)?;
    let hh = num(11, 2)?;
    let mm = num(14, 2)?;
    let ss = num(17, 2)?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // days_from_civil（Howard Hinnant）。
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some((days * 86_400 + hh * 3600 + mm * 60 + ss).max(0) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn known_timestamps() {
        // 2026-11-04T09:12:00Z = 1793742720
        assert_eq!(format_rfc3339(UNIX_EPOCH + Duration::from_secs(1_793_783_520)), "2026-11-04T09:12:00Z");
        assert_eq!(format_rfc3339(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        // 闰年 2024-02-29
        assert_eq!(format_rfc3339(UNIX_EPOCH + Duration::from_secs(1_709_164_800)), "2024-02-29T00:00:00Z");
    }

    #[test]
    fn parse_roundtrip() {
        assert_eq!(parse_rfc3339_to_epoch("2026-11-04T09:12:00Z"), Some(1_793_783_520));
        assert_eq!(parse_rfc3339_to_epoch("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339_to_epoch("2026-11-04"), None);
        // 往返。
        let t = now_rfc3339();
        assert_eq!(format_rfc3339(UNIX_EPOCH + std::time::Duration::from_secs(parse_rfc3339_to_epoch(&t).unwrap())), t);
    }

    #[test]
    fn now_is_monotonic_shape() {
        let s = now_rfc3339();
        assert_eq!(s.len(), 20);
        assert!(s.ends_with('Z'));
    }
}
