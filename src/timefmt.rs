//! Epoch-seconds helpers: UTC RFC 3339 formatting without a time crate.
//!
//! `SNAPREF_TIME` (unix epoch seconds) overrides the clock so that
//! snapshots — and therefore snapshot ids — are fully reproducible in
//! tests, demos and CI pipelines.

/// Current unix time in seconds, honoring the `SNAPREF_TIME` override.
pub fn now() -> i64 {
    if let Ok(v) = std::env::var("SNAPREF_TIME") {
        if let Ok(n) = v.trim().parse::<i64>() {
            return n;
        }
    }
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(_) => 0,
    }
}

/// Format epoch seconds as `YYYY-MM-DDTHH:MM:SSZ` (UTC, RFC 3339).
///
/// Uses the days-from-civil inverse algorithm, exact for the whole i64
/// range that fits a year counter — including pre-1970 timestamps.
pub fn utc(epoch: i64) -> String {
    let days = epoch.div_euclid(86_400);
    let secs = epoch.rem_euclid(86_400);
    let (hh, mm, ss) = (secs / 3600, (secs % 3600) / 60, secs % 60);

    // Civil-from-days (Howard Hinnant's algorithm), era = 400-year cycle.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // day of era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);

    format!("{year:04}-{month:02}-{day:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_zero_and_the_last_second_of_day_one() {
        assert_eq!(utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc(86_399), "1970-01-01T23:59:59Z");
    }

    #[test]
    fn leap_day_2000_is_handled() {
        // 2000 is a leap year despite being divisible by 100 (also by 400).
        assert_eq!(utc(951_782_400), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn negative_epochs_format_pre_1970_dates() {
        assert_eq!(utc(-1), "1969-12-31T23:59:59Z");
        assert_eq!(utc(-86_400), "1969-12-31T00:00:00Z");
    }

    #[test]
    fn a_2026_timestamp_round_trips_by_inspection() {
        // 2026-01-01T00:00:00Z is epoch 1767225600; add 192 days + 10h.
        assert_eq!(utc(1_767_225_600), "2026-01-01T00:00:00Z");
        assert_eq!(
            utc(1_767_225_600 + 192 * 86_400 + 36_000),
            "2026-07-12T10:00:00Z"
        );
    }
}
