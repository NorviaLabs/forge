//! The `--since` argument.

use chrono::{DateTime, NaiveDate, Utc};

/// `--since`: a date (`2025-06-01`, midnight UTC) or a time before `now`
/// (`7d`, `24h`).
pub fn parse_since(text: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>, String> {
    let invalid =
        || format!("`{text}` is not a date (YYYY-MM-DD) or a number of days or hours (7d, 24h)");
    if let Ok(date) = NaiveDate::parse_from_str(text, "%Y-%m-%d") {
        // Exactly `YYYY-MM-DD`: chrono also accepts unpadded fields.
        if text.len() == 10 {
            return Ok(date.and_time(chrono::NaiveTime::MIN).and_utc());
        }
        return Err(invalid());
    }
    let (number, unit) = text.split_at(text.len().saturating_sub(1));
    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid());
    }
    let count: i64 = number.parse().map_err(|_| invalid())?;
    let span = match unit {
        "d" => chrono::Duration::try_days(count),
        "h" => chrono::Duration::try_hours(count),
        _ => None,
    }
    .ok_or_else(invalid)?;
    now.checked_sub_signed(span).ok_or_else(invalid)
}
