//! Date/time presentation helpers.
//!
//! Wire values are either fractional Unix seconds (instants) or Unix seconds of
//! UTC midnight (calendar days). Internally days are stored as epoch days.

use chrono::{DateTime, Datelike, Local, NaiveDate, SecondsFormat, Utc};

/// Days between 0001-01-01 (chrono's CE origin) and 1970-01-01.
const UNIX_EPOCH_CE: i64 = 719_163;

pub fn epoch_day_to_date(day: i64) -> Option<NaiveDate> {
    let ce = day.checked_add(UNIX_EPOCH_CE)?;
    i32::try_from(ce)
        .ok()
        .and_then(NaiveDate::from_num_days_from_ce_opt)
}

/// `YYYY-MM-DD` for an epoch day.
pub fn date_string(day: i64) -> Option<String> {
    epoch_day_to_date(day).map(|date| date.format("%Y-%m-%d").to_string())
}

/// RFC-3339 (UTC) for a fractional Unix timestamp.
pub fn instant_string(seconds: f64) -> Option<String> {
    if !seconds.is_finite() {
        return None;
    }
    let millis = (seconds * 1000.0).round() as i64;
    DateTime::<Utc>::from_timestamp_millis(millis)
        .map(|dt| dt.to_rfc3339_opts(SecondsFormat::Secs, true))
}

/// Epoch day for the local calendar date.
pub fn today_epoch_day() -> i64 {
    Local::now().date_naive().num_days_from_ce() as i64 - UNIX_EPOCH_CE
}

/// Current time as fractional Unix seconds (the wire format for `cd`/`md`).
pub fn now_seconds() -> f64 {
    Utc::now().timestamp_micros() as f64 / 1_000_000.0
}

/// Epoch day → Unix seconds of UTC midnight (the wire format for dates).
pub fn day_to_wire(day: i64) -> i64 {
    day * 86_400
}

pub fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_epoch_days() {
        assert_eq!(date_string(0).as_deref(), Some("1970-01-01"));
        // 2024-01-01 is 19723 days after the Unix epoch.
        assert_eq!(date_string(19_723).as_deref(), Some("2024-01-01"));
        assert!(today_epoch_day() > 20_000);
    }

    #[test]
    fn formats_fractional_instants() {
        assert_eq!(instant_string(0.0).as_deref(), Some("1970-01-01T00:00:00Z"));
    }
}
