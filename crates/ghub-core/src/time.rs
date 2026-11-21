//! Calendar and clock helpers, hand-rolled so the binary carries no date library.
//!
//! The C# reached for `DateTimeOffset.Now` and `DateTime.UtcNow` in a dozen places. Three of
//! those uses differ in a way that matters here:
//!
//! * backup file names need a *fixed-width, lexicographically sortable* UTC stamp,
//! * diagnostics entries want a human-readable wall-clock stamp,
//! * everything else only needs monotonic-ish seconds for comparisons.
//!
//! All three come from the same two primitives below. There is deliberately no local-time
//! conversion: Windows time-zone data would have to be read from the registry and a daylight
//! saving change can make a newer stamp sort behind an older one. Diagnostics therefore say UTC
//! and the UI labels the column that way.

/// Seconds since the Unix epoch.
pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

/// Milliseconds since the Unix epoch.
///
/// `i64` rather than `u64`: a clock set before 1970 gives a negative value instead of wrapping to
/// the year 584 million, which would silently sort to the end of every list.
pub fn now_unix_millis() -> i64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_millis() as i64,
        Err(e) => -(e.duration().as_millis() as i64),
    }
}

/// `yyyyMMdd-HHmmss-fff` from a Unix timestamp, in UTC.
///
/// Fixed width and lexicographically ordered, which is what lets the backup list sort by name.
pub fn format_stamp(unix_seconds: i64, millis: u32) -> String {
    let days = unix_seconds.div_euclid(86_400);
    let seconds_of_day = unix_seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}{month:02}{day:02}-{:02}{:02}{:02}-{millis:03}",
        seconds_of_day / 3600,
        (seconds_of_day % 3600) / 60,
        seconds_of_day % 60
    )
}

/// `yyyy-MM-dd HH:mm:ss.fff` in UTC, for diagnostics rows and exported logs.
pub fn format_readable(unix_millis: i64) -> String {
    let seconds = unix_millis.div_euclid(1000);
    let millis = unix_millis.rem_euclid(1000) as u32;
    let days = seconds.div_euclid(86_400);
    let seconds_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}.{millis:03}",
        seconds_of_day / 3600,
        (seconds_of_day % 3600) / 60,
        seconds_of_day % 60
    )
}

/// `HH:mm:ss.fff` in UTC: the diagnostics table shows only the time, the date is in the header.
pub fn format_time_of_day(unix_millis: i64) -> String {
    let seconds = unix_millis.div_euclid(1000);
    let millis = unix_millis.rem_euclid(1000) as u32;
    let seconds_of_day = seconds.rem_euclid(86_400);
    format!(
        "{:02}:{:02}:{:02}.{millis:03}",
        seconds_of_day / 3600,
        (seconds_of_day % 3600) / 60,
        seconds_of_day % 60
    )
}

/// Days since the Unix epoch to a calendar date. Howard Hinnant's `civil_from_days`, which is
/// exact for the whole proleptic Gregorian range and needs no date library.
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_is_the_first_of_january_1970() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(format_stamp(0, 0), "19700101-000000-000");
        assert_eq!(format_readable(0), "1970-01-01 00:00:00.000");
    }

    #[test]
    fn known_timestamps_round_trip() {
        // 2024-02-29T13:45:07Z - a leap day, which is where naive date maths breaks.
        assert_eq!(format_stamp(1_709_214_307, 42), "20240229-134507-042");
        assert_eq!(format_readable(1_709_214_307_042), "2024-02-29 13:45:07.042");
        assert_eq!(format_time_of_day(1_709_214_307_042), "13:45:07.042");
    }

    #[test]
    fn century_boundaries_are_right() {
        // 1900 was not a leap year, 2000 was. Both fall inside the era arithmetic.
        assert_eq!(civil_from_days(-25_567), (1900, 1, 1));
        // 2000-01-01 is day 10957; +31 for January and +28 reaches the leap day.
        assert_eq!(civil_from_days(10_957), (2000, 1, 1));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        assert_eq!(civil_from_days(11_017), (2000, 3, 1));
        assert_eq!(civil_from_days(2_932_896), (9999, 12, 31));
    }

    #[test]
    fn pre_epoch_times_do_not_wrap() {
        // One second before the epoch must be 1969, not the far future: div_euclid, not `/`.
        assert_eq!(format_readable(-1000), "1969-12-31 23:59:59.000");
        assert_eq!(format_stamp(-1, 0), "19691231-235959-000");
    }

    #[test]
    fn stamps_sort_in_chronological_order() {
        let mut stamps = [
            format_stamp(1_709_214_307, 999),
            format_stamp(1_709_214_307, 0),
            format_stamp(1_609_214_307, 500),
            format_stamp(1_809_214_307, 1),
        ];
        let expected = [stamps[2].clone(), stamps[1].clone(), stamps[0].clone(), stamps[3].clone()];
        stamps.sort();
        assert_eq!(stamps.to_vec(), expected.to_vec());
    }

    #[test]
    fn the_clock_is_past_2020_and_moves_forward() {
        let a = now_unix_millis();
        assert!(a > 1_577_836_800_000, "system clock reads before 2020: {a}");
        assert!(now_unix() * 1000 <= now_unix_millis() + 1000);
    }
}
