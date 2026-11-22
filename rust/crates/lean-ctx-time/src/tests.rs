// SPDX-License-Identifier: Apache-2.0
//! Golden byte tests: these strings are what chrono wrote, and what LeanCTX
//! has stored and signed. A change here breaks stored state.

use crate::{Duration, OffsetDateTime, SecondsFormat, UtcDateTime};

fn at(secs: i64, nanos: u32) -> UtcDateTime {
    UtcDateTime::from_timestamp(secs, nanos).unwrap()
}

#[test]
fn rfc3339_and_serde_bytes() {
    let t = at(1_763_373_600, 0); // 2025-11-17T10:00:00Z
    assert_eq!(t.to_rfc3339(), "2025-11-17T10:00:00+00:00");
    assert_eq!(format!("{t:?}"), "2025-11-17T10:00:00Z");
    assert_eq!(t.to_string(), "2025-11-17 10:00:00 UTC");
    assert_eq!(
        serde_json::to_string(&t).unwrap(),
        "\"2025-11-17T10:00:00Z\""
    );
    let t = at(1_763_373_600, 120_000_000);
    assert_eq!(format!("{t:?}"), "2025-11-17T10:00:00.120Z");
    assert_eq!(
        t.to_rfc3339_opts(SecondsFormat::Secs, true),
        "2025-11-17T10:00:00Z"
    );
    assert_eq!(
        t.to_rfc3339_opts(SecondsFormat::Micros, false),
        "2025-11-17T10:00:00.120000+00:00"
    );
    assert_eq!(at(0, 1).to_string(), "1970-01-01 00:00:00.000000001 UTC");
    assert_eq!(at(-1, 0).to_rfc3339(), "1969-12-31T23:59:59+00:00");
}

#[test]
fn strftime_bytes() {
    let t = at(1_763_373_600, 123_456_789);
    assert_eq!(
        t.format("%Y-%m-%d %H:%M UTC").to_string(),
        "2025-11-17 10:00 UTC"
    );
    assert_eq!(t.format("%Y%m%dT%H%M%SZ").to_string(), "20251117T100000Z");
    assert_eq!(
        t.format("%Y-%m-%dT%H:%M:%S%.3f").to_string(),
        "2025-11-17T10:00:00.123"
    );
    assert_eq!(t.format("%B %Y").to_string(), "November 2025");
    assert_eq!(t.format("%b %d, %Y").to_string(), "Nov 17, 2025");
    assert_eq!(
        t.format("%Y%m%dT%H%M%S%6fZ").to_string(),
        "20251117T100000123456Z"
    );
}

#[test]
fn parse_strict_and_relaxed() {
    let t = UtcDateTime::parse_from_rfc3339("2025-11-17T11:00:00+01:00").unwrap();
    assert_eq!(t, at(1_763_373_600, 0));
    assert_eq!(
        UtcDateTime::parse_from_rfc3339("2025-11-17T10:00Z")
            .unwrap_err()
            .to_string(),
        "premature end of input"
    );
    assert_eq!(
        UtcDateTime::parse_from_rfc3339("2025-11-17T10:00:00Zx")
            .unwrap_err()
            .to_string(),
        "trailing input"
    );
    assert_eq!(
        UtcDateTime::parse_from_rfc3339("2025-13-17T10:00:00Z")
            .unwrap_err()
            .to_string(),
        "input is out of range"
    );
    assert_eq!(
        UtcDateTime::parse_from_rfc3339("2025-11-17X10:00:00Z")
            .unwrap_err()
            .to_string(),
        "input contains invalid characters"
    );
    assert!(UtcDateTime::parse_from_rfc3339("2025-11-17T10:00:00 UTC").is_err());
    assert_eq!(
        "2025-11-17T10:00:00 UTC".parse::<UtcDateTime>().unwrap(),
        at(1_763_373_600, 0)
    );
    let back: UtcDateTime = serde_json::from_str("\"2025-11-17T10:00:00.120Z\"").unwrap();
    assert_eq!(back, at(1_763_373_600, 120_000_000));
    let o = OffsetDateTime::parse_from_rfc3339("2025-11-17T11:00:00+01:00").unwrap();
    assert_eq!(o.to_rfc3339(), "2025-11-17T11:00:00+01:00");
    assert_eq!(o.offset_seconds(), 3600);
}

#[test]
fn duration_bytes_and_range() {
    assert_eq!(Duration::zero().to_string(), "P0D");
    assert_eq!(Duration::seconds(90).to_string(), "PT90S");
    assert_eq!(Duration::milliseconds(-500).to_string(), "-PT0.5S");
    assert_eq!(Duration::days(2).to_string(), "PT172800S");
    assert_eq!(Duration::minutes(-90).num_hours(), -1);
    assert!(Duration::try_seconds(i64::MAX).is_none());
    assert_eq!(Duration::MAX.num_milliseconds(), i64::MAX);
    assert!(
        Duration::MAX
            .checked_add(&Duration::nanoseconds(1_000_000))
            .is_none()
    );
    let t = at(100, 0);
    assert_eq!((t + Duration::seconds(5)) - t, Duration::seconds(5));
}

#[test]
fn dates_parse_like_chrono_without_a_sign() {
    use crate::{Date, days_between, parse_civil_datetime, parse_date};
    let d = parse_date("2026-11-17", "%Y-%m-%d").unwrap();
    assert_eq!(d.to_string(), "2026-11-17");
    assert_eq!(parse_date("20261117", "%Y%m%d").unwrap(), d);
    // chrono accepted some signed years; LeanCTX never wrote one.
    assert!(parse_date("+2026-11-17", "%Y-%m-%d").is_err());
    assert!(parse_date("-2026-11-17", "%Y-%m-%d").is_err());
    assert!(parse_civil_datetime("+20261117100000", "%Y%m%d%H%M%S").is_err());
    assert!(parse_date("2026-02-30", "%Y-%m-%d").is_err());
    let dt = parse_civil_datetime("2026-11-17 10:00:60", "%Y-%m-%d %H:%M:%S").unwrap();
    assert_eq!(dt.second(), 59, "a leap second reads as :59");
    let later = Date::new(2027, 1, 1).unwrap();
    assert_eq!(days_between(d, later), 45);
    assert_eq!(days_between(later, d), -45);
    assert_eq!((d + Duration::days(45)), later);
    assert_eq!((later - Duration::days(45)), d);
}

#[test]
fn civil_to_utc_and_offsets() {
    use crate::{CivilDateTime, utc_from_civil, utc_from_local_civil};
    let dt: CivilDateTime = "2025-11-17T10:00:00".parse().unwrap();
    assert_eq!(utc_from_civil(dt), Some(at(1_763_373_600, 0)));
    // The local reading is the same instant some whole minutes away.
    let local = utc_from_local_civil(dt).unwrap();
    assert_eq!(
        local
            .signed_duration_since(at(1_763_373_600, 0))
            .num_seconds()
            % 60,
        0
    );

    let t = at(1_763_373_600, 250_000_000);
    assert_eq!(
        t.fixed_offset().to_rfc3339(),
        "2025-11-17T10:00:00.250+00:00"
    );
    let east = t.with_offset_seconds(3600).unwrap();
    assert_eq!(east.to_rfc3339(), "2025-11-17T11:00:00.250+01:00");
    assert_eq!(east.to_utc(), t);
    assert!(t.with_offset_seconds(86_400).is_none());
    assert!(t.with_offset_seconds(-86_399).is_some());
    assert_eq!(
        t.fixed_offset().to_rfc3339_opts(SecondsFormat::Secs, true),
        "2025-11-17T10:00:00Z"
    );
    assert_eq!(
        east.to_rfc3339_opts(SecondsFormat::Millis, true),
        "2025-11-17T11:00:00.250+01:00"
    );
    assert_eq!(t.with_nanosecond(5).unwrap(), at(1_763_373_600, 5));
    assert!(t.with_nanosecond(1_000_000_000).is_none());
}

#[test]
fn duration_trunc_floors_like_chrono() {
    let t = at(1_763_375_999, 999_999_999); // 10:39:59.999…
    assert_eq!(
        t.duration_trunc(Duration::hours(1)).unwrap(),
        at(1_763_373_600, 0)
    );
    assert_eq!(
        t.duration_trunc(Duration::minutes(1)).unwrap(),
        at(1_763_375_940, 0)
    );
    let exact = at(1_763_373_600, 0);
    assert_eq!(exact.duration_trunc(Duration::hours(1)).unwrap(), exact);
    // Before the epoch it still rounds down, towards the past.
    assert_eq!(
        at(-90, 0).duration_trunc(Duration::minutes(1)).unwrap(),
        at(-120, 0)
    );
    assert!(t.duration_trunc(Duration::zero()).is_err());
    assert!(t.duration_trunc(Duration::seconds(-1)).is_err());
    assert!(at(10, 0).duration_trunc(Duration::minutes(1)).is_err());
}

#[test]
fn duration_constructors_are_const() {
    const WEEK: Duration = Duration::days(7);
    const NONE: Option<Duration> = Duration::try_days(i64::MAX);
    assert_eq!(WEEK.num_hours(), 168);
    assert!(NONE.is_none());
}

#[test]
fn far_future_sentinels_clamp_to_the_last_instant() {
    // chrono accepted every valid value of year 9999; jiff ends at
    // 9999-12-30T22:00:00.999999999Z. Cloud key sets write
    // 9999-12-31T23:59:59Z for a key that never expires.
    let never = UtcDateTime::parse_from_rfc3339("9999-12-31T23:59:59Z").unwrap();
    assert_eq!(never.as_jiff(), jiff::Timestamp::MAX);
    let off = OffsetDateTime::parse_from_rfc3339("9999-12-31T23:59:59-05:00").unwrap();
    assert_eq!(off.to_utc(), never);
    assert_eq!(off.offset_seconds(), 0);
    assert!(never > UtcDateTime::parse_from_rfc3339("9999-12-30T21:59:59Z").unwrap());
    // Values jiff can hold keep their offset; invalid ones stay errors.
    let last = OffsetDateTime::parse_from_rfc3339("9999-12-30T21:00:00Z").unwrap();
    assert_eq!(last.offset_seconds(), 0);
    assert!(UtcDateTime::parse_from_rfc3339("9999-12-32T00:00:00Z").is_err());
    assert!(UtcDateTime::parse_from_rfc3339("9999-02-30T00:00:00Z").is_err());
}
