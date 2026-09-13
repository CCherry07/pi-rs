use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Current Unix time in milliseconds, clamped to `i64` for wire timestamps.
///
/// A clock before the Unix epoch remains zero, matching the existing callers.
pub fn unix_timestamp_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, duration_ms_i64)
}

/// Current Unix time in milliseconds, clamped to `u64` for expiry timestamps.
pub fn unix_timestamp_ms_u64() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, duration_ms_u64)
}

fn duration_ms_i64(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

fn duration_ms_u64(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::{duration_ms_i64, duration_ms_u64, unix_timestamp_ms, unix_timestamp_ms_u64};
    use std::time::Duration;

    #[test]
    fn timestamp_conversions_clamp_to_the_wire_type() {
        assert_eq!(duration_ms_i64(Duration::from_millis(42)), 42);
        assert_eq!(duration_ms_i64(Duration::MAX), i64::MAX);
        assert_eq!(duration_ms_u64(Duration::from_millis(42)), 42);
        assert_eq!(duration_ms_u64(Duration::MAX), u64::MAX);
    }

    #[test]
    fn timestamps_are_nonnegative() {
        assert!(unix_timestamp_ms() >= 0);
        assert!(unix_timestamp_ms_u64() > 0);
    }
}
