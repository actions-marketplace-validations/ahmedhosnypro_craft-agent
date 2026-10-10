//! Strict RFC 3339 timestamps: hostile input parses to None, never panics.

/// A civil calendar date.
struct Date {
    year: i64,
    month: i64,
    day: i64,
}

/// Days from 1970-01-01 for a validated civil date.
fn serial_day(date: &Date) -> i64 {
    let y = date.year - i64::from(date.month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = date.month + i64::from(date.month > 2) * -3 + i64::from(date.month <= 2) * 9;
    let doy = (153 * mp + 2).div_euclid(5) + date.day - 1;
    let doe = yoe * 365 + yoe.div_euclid(4) - yoe.div_euclid(100) + doy;
    era * 146_097 + doe - 719_468
}

fn digits(bytes: &[u8]) -> Option<i64> {
    let mut value: i64 = 0;
    for b in bytes {
        if !b.is_ascii_digit() {
            return None;
        }
        value = value * 10 + i64::from(b - b'0');
    }
    Some(value)
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(date: &Date) -> i64 {
    match date.month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(date.year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Byte-indexed punctuation the timestamp format fixes in place.
const SEPARATORS: [(usize, u8); 5] = [(4, b'-'), (7, b'-'), (10, b'T'), (13, b':'), (16, b':')];

fn separators_ok(bytes: &[u8]) -> bool {
    SEPARATORS
        .iter()
        .all(|&(at, want)| bytes.get(at) == Some(&want))
}

fn zulu_offset(bytes: &[u8]) -> Option<i64> {
    match bytes.get(19) {
        Some(&b'Z') | Some(&b'z') => Some(0),
        _ => None,
    }
}

fn numeric_offset(bytes: &[u8]) -> Option<i64> {
    let sign = match bytes.get(19) {
        Some(&b'+') => 1i64,
        Some(&b'-') => -1i64,
        _ => return None,
    };
    if bytes.get(22) != Some(&b':') {
        return None;
    }
    let hours = digits(bytes.get(20..22)?)?;
    let minutes = digits(bytes.get(23..25)?)?;
    if hours > 23 {
        return None;
    }
    if minutes > 59 {
        return None;
    }
    Some(sign * (hours * 3600 + minutes * 60))
}

fn parse_offset(bytes: &[u8]) -> Option<i64> {
    match bytes.len() {
        20 => zulu_offset(bytes),
        25 => numeric_offset(bytes),
        _ => None,
    }
}

/// `YYYY-MM-DDTHH:MM:SS` plus `Z` or `±HH:MM` into Unix seconds; anything
/// else is None. GitHub emits the `Z` form; the offset form keeps parsing
/// total.
pub fn parse_rfc3339_secs(text: &str) -> Option<u64> {
    let bytes = text.as_bytes();
    let offset = parse_offset(bytes)?;
    if !separators_ok(bytes) {
        return None;
    }
    let date = Date {
        year: digits(bytes.get(0..4)?)?,
        month: digits(bytes.get(5..7)?)?,
        day: digits(bytes.get(8..10)?)?,
    };
    let hour = digits(bytes.get(11..13)?)?;
    let minute = digits(bytes.get(14..16)?)?;
    let second = digits(bytes.get(17..19)?)?;
    if !(1..=12).contains(&date.month) {
        return None;
    }
    if !(1..=days_in_month(&date)).contains(&date.day) {
        return None;
    }
    if hour > 23 {
        return None;
    }
    if minute > 59 {
        return None;
    }
    if second > 59 {
        return None;
    }
    let local = serial_day(&date) * 86_400 + hour * 3600 + minute * 60 + second - offset;
    u64::try_from(local).ok()
}
