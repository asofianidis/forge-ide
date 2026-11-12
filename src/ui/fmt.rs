//! Formatting helpers: relative times and timestamp parsing.

use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// "just now", "5 minutes ago", "3 days ago", …
pub fn relative_time(unix: i64) -> String {
    relative_between(unix, now_unix())
}

fn relative_between(then: i64, now: i64) -> String {
    let secs = (now - then).max(0);
    let plural = |n: i64, unit: &str| {
        if n == 1 {
            format!("1 {unit} ago")
        } else {
            format!("{n} {unit}s ago")
        }
    };
    match secs {
        0..60 => "just now".to_owned(),
        60..3600 => plural(secs / 60, "minute"),
        3600..86_400 => plural(secs / 3600, "hour"),
        86_400..2_592_000 => plural(secs / 86_400, "day"),
        2_592_000..31_536_000 => plural(secs / 2_592_000, "month"),
        _ => plural(secs / 31_536_000, "year"),
    }
}

/// Parses an RFC 3339 / ISO 8601 UTC timestamp like `2024-05-01T12:34:56Z`
/// (fractional seconds and numeric offsets are accepted).
pub fn parse_timestamp(s: &str) -> Option<i64> {
    let s = s.trim();
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    let mut rest = &s[19..];
    if let Some(frac) = rest.strip_prefix('.') {
        let digits = frac.bytes().take_while(u8::is_ascii_digit).count();
        rest = &frac[digits..];
    }
    let offset = match rest.as_bytes().first() {
        Some(b'+' | b'-') => {
            let sign = if rest.starts_with('-') { -1 } else { 1 };
            let oh: i64 = rest.get(1..3)?.parse().ok()?;
            let om: i64 = rest.get(4..6).and_then(|m| m.parse().ok()).unwrap_or(0);
            sign * (oh * 3600 + om * 60)
        }
        _ => 0,
    };
    Some(days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + sec - offset)
}

/// Days since 1970-01-01 (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Relative time for an ISO timestamp, or the raw text if it doesn't parse.
pub fn relative_iso(s: &str) -> String {
    parse_timestamp(s).map_or_else(|| s.to_owned(), relative_time)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_timestamps() {
        assert_eq!(parse_timestamp("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_timestamp("2024-02-29T12:00:00Z"), Some(1_709_208_000));
        assert_eq!(
            parse_timestamp("2024-02-29T12:00:00.123Z"),
            Some(1_709_208_000)
        );
        assert_eq!(
            parse_timestamp("2024-02-29T14:00:00+02:00"),
            Some(1_709_208_000)
        );
        assert_eq!(parse_timestamp("garbage"), None);
    }

    #[test]
    fn relative_times() {
        assert_eq!(relative_between(100, 130), "just now");
        assert_eq!(relative_between(0, 60), "1 minute ago");
        assert_eq!(relative_between(0, 7200), "2 hours ago");
        assert_eq!(relative_between(0, 86_400 * 3), "3 days ago");
    }
}
