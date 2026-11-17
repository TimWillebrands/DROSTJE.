use std::time::{SystemTime, UNIX_EPOCH};

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or_default()
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

pub fn parse_utc(text: &str) -> Option<i64> {
    let text = text.trim();
    let number = |range: std::ops::Range<usize>| text.get(range)?.parse::<i64>().ok();
    let separators = [(4, b'-'), (7, b'-'), (10, b'T'), (13, b':'), (16, b':')];
    if separators
        .iter()
        .any(|(index, byte)| text.as_bytes().get(*index) != Some(byte))
    {
        return None;
    }
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second)
}

pub fn ago(now: i64, then: i64) -> String {
    let seconds = (now - then).max(0);
    match seconds {
        0..60 => "just now".into(),
        60..3_600 => format!("{} min ago", seconds / 60),
        3_600..86_400 => format!("{} h ago", seconds / 3_600),
        86_400..604_800 => format!("{} d ago", seconds / 86_400),
        _ => format!("{} wk ago", seconds / 604_800),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_timestamps_parse_to_unix_seconds() {
        assert_eq!(parse_utc("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_utc("2026-11-17T10:00:00Z"), Some(1_794_909_600));
        assert_eq!(parse_utc("2026-11-17T10:00:00.250Z"), Some(1_794_909_600));
        assert_eq!(parse_utc("2024-02-29T12:00:00Z"), Some(1_709_208_000));
        assert_eq!(parse_utc("yesterday"), None);
        assert_eq!(parse_utc("2026-13-01T00:00:00Z"), None);
    }

    #[test]
    fn ages_read_like_a_phone_list() {
        assert_eq!(ago(100, 90), "just now");
        assert_eq!(ago(1_000, 400), "10 min ago");
        assert_eq!(ago(10_000, 0), "2 h ago");
        assert_eq!(ago(200_000, 0), "2 d ago");
        assert_eq!(ago(2_000_000, 0), "3 wk ago");
        assert_eq!(ago(0, 100), "just now");
    }
}
