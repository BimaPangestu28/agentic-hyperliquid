//! Pure helpers for the daily risk cap. No I/O.

/// UNIX-seconds timestamp of the start of the UTC day containing `now_secs`.
pub fn start_of_utc_day(now_secs: i64) -> i64 {
    (now_secs / 86_400) * 86_400
}

/// True if committing `new_risk` on top of `used_today` stays within `cap_amount`.
/// A non-positive cap means "no cap" (always allowed) — but callers should only
/// invoke this when a cap is configured.
pub fn within_daily_cap(used_today: f64, new_risk: f64, cap_amount: f64) -> bool {
    if cap_amount <= 0.0 { return true; }
    used_today + new_risk <= cap_amount + 1e-9 // tiny epsilon for float equality
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_of_utc_day_truncates_to_midnight() {
        // 2021-01-01 12:00:00 UTC = 1609502400; midnight = 1609459200
        assert_eq!(start_of_utc_day(1_609_502_400), 1_609_459_200);
    }

    #[test]
    fn allows_within_cap_and_blocks_over() {
        // cap $50: used $30, new $19 -> ok; used $30, new $21 -> blocked
        assert!(within_daily_cap(30.0, 19.0, 50.0));
        assert!(within_daily_cap(30.0, 20.0, 50.0)); // exactly at cap is allowed
        assert!(!within_daily_cap(30.0, 21.0, 50.0));
    }

    #[test]
    fn non_positive_cap_means_unlimited() {
        assert!(within_daily_cap(1000.0, 1000.0, 0.0));
    }
}
