// SPDX-License-Identifier: Apache-2.0
//! Time for LeanCTX: UTC instants, signed durations and their text formats.
//!
//! A thin layer over [`jiff`]. LeanCTX stores timestamps in state files,
//! signs them into receipts and sends them over its wire contracts, so this
//! crate keeps every format LeanCTX has always written byte for byte:
//!
//! - serde and `Debug`: RFC 3339 in UTC with `Z` and 0, 3, 6 or 9
//!   fractional digits (`2026-11-17T10:00:00Z`, `…00.250Z`);
//! - [`UtcDateTime::to_rfc3339`]: the same with `+00:00`;
//! - [`UtcDateTime::to_rfc3339_opts`]: a fixed precision, truncated;
//! - `Display`: `2026-11-17 10:00:00 UTC`;
//! - [`UtcDateTime::format`]: strftime (see [`mod@format`] for the specifiers);
//! - [`Duration`]'s `Display`: ISO 8601 (`PT90S`, `-PT0.5S`, `P0D`).
//!
//! Parsing accepts what LeanCTX has always accepted: strict RFC 3339 in
//! [`UtcDateTime::parse_from_rfc3339`], and the relaxed form (spaces before
//! the offset, `UTC`, offsets without a colon) in `FromStr` and serde.

#![forbid(unsafe_code)]

pub mod format;
mod parse;

use std::fmt;
use std::ops::{Add, AddAssign, Div, Mul, Neg, Sub, SubAssign};
use std::str::FromStr;

pub use jiff;
use jiff::civil;
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};

pub use parse::ParseError;

/// A calendar date without a time zone (chrono's `NaiveDate`).
pub use jiff::civil::Date;
/// A date and wall-clock time without a time zone (chrono's `NaiveDateTime`).
pub use jiff::civil::DateTime as CivilDateTime;
/// A wall-clock time without a date or time zone (chrono's `NaiveTime`).
pub use jiff::civil::Time;

/// Parses a calendar date with a strftime pattern.
///
/// Matches chrono's `NaiveDate::parse_from_str` for every unsigned year
/// (a differential test compared 17.7 million inputs); a leading `+` or `-`
/// on the year is rejected, where chrono accepted some signed forms.
pub fn parse_date(s: &str, fmt: &str) -> Result<Date, ParseError> {
    if s.starts_with(['+', '-']) {
        return Err(ParseError::invalid());
    }
    Date::strptime(fmt, s).map_err(|_| ParseError::invalid())
}

/// Parses a date and wall-clock time with a strftime pattern, like
/// [`parse_date`]. A leap second (`:60`) reads as `:59`.
pub fn parse_civil_datetime(s: &str, fmt: &str) -> Result<CivilDateTime, ParseError> {
    if s.starts_with(['+', '-']) {
        return Err(ParseError::invalid());
    }
    CivilDateTime::strptime(fmt, s).map_err(|_| ParseError::invalid())
}

/// `dt` read in UTC; `None` only in the first and last day of jiff's range.
#[must_use]
pub fn utc_from_civil(dt: CivilDateTime) -> Option<UtcDateTime> {
    dt.to_zoned(TimeZone::UTC)
        .ok()
        .map(|z| UtcDateTime(z.timestamp()))
}

/// `dt` read as wall-clock time in the system time zone. A repeated hour
/// (DST fold) takes the earlier instant; a skipped hour (DST gap) reads `dt`
/// as UTC, at most one DST shift off. `None` only at jiff's range edges.
#[must_use]
pub fn utc_from_local_civil(dt: CivilDateTime) -> Option<UtcDateTime> {
    use jiff::tz::AmbiguousOffset;
    let offset = match TimeZone::system().to_ambiguous_timestamp(dt).offset() {
        AmbiguousOffset::Unambiguous { offset } | AmbiguousOffset::Fold { before: offset, .. } => {
            offset
        }
        AmbiguousOffset::Gap { .. } => return utc_from_civil(dt),
    };
    offset.to_timestamp(dt).ok().map(UtcDateTime)
}

/// Whole days from `earlier` to `later` (negative when `later` is earlier),
/// like chrono's `(later - earlier).num_days()`.
#[must_use]
pub fn days_between(earlier: Date, later: Date) -> i64 {
    i64::from((later - earlier).get_days())
}

// chrono let `NaiveDate`/`NaiveDateTime` take a `TimeDelta` with `+`/`-`
// (whole days for a date, fraction dropped); keep that and its overflow panic.
macro_rules! civil_ops {
    ($t:ty) => {
        impl Add<Duration> for $t {
            type Output = $t;
            fn add(self, d: Duration) -> $t {
                self.checked_add(d.0).expect(concat!(
                    "`",
                    stringify!($t),
                    " + Duration` overflowed"
                ))
            }
        }
        impl Sub<Duration> for $t {
            type Output = $t;
            fn sub(self, d: Duration) -> $t {
                self.checked_sub(d.0).expect(concat!(
                    "`",
                    stringify!($t),
                    " - Duration` overflowed"
                ))
            }
        }
    };
}
civil_ops!(Date);
civil_ops!(CivilDateTime);

const NANOS_PER_SEC: i128 = 1_000_000_000;

/// How many fractional digits [`UtcDateTime::to_rfc3339_opts`] writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SecondsFormat {
    /// No fraction.
    Secs,
    /// Three digits.
    Millis,
    /// Six digits.
    Micros,
    /// Nine digits.
    Nanos,
    /// The fewest of 0, 3, 6 or 9 digits that keep the value exact.
    AutoSi,
}

/// The value is outside the range the target type can hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutOfRangeError;

impl fmt::Display for OutOfRangeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("out of range")
    }
}

impl std::error::Error for OutOfRangeError {}

// ---------------------------------------------------------------------------
// UtcDateTime
// ---------------------------------------------------------------------------

/// An instant in UTC with nanosecond precision.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UtcDateTime(Timestamp);

impl Default for UtcDateTime {
    /// The Unix epoch.
    fn default() -> Self {
        Self::UNIX_EPOCH
    }
}

impl UtcDateTime {
    /// 1970-01-01T00:00:00Z.
    pub const UNIX_EPOCH: Self = Self(Timestamp::UNIX_EPOCH);

    /// The current time from the system clock.
    #[must_use]
    pub fn now() -> Self {
        Self(Timestamp::now())
    }

    /// A UTC calendar date and time; `None` when a field is out of range.
    #[must_use]
    pub fn with_ymd_and_hms(
        year: i32,
        month: u32,
        day: u32,
        hour: u32,
        minute: u32,
        second: u32,
    ) -> Option<Self> {
        let date = civil::Date::new(
            i16::try_from(year).ok()?,
            i8::try_from(month).ok()?,
            i8::try_from(day).ok()?,
        )
        .ok()?;
        let time = civil::Time::new(
            i8::try_from(hour).ok()?,
            i8::try_from(minute).ok()?,
            i8::try_from(second).ok()?,
            0,
        )
        .ok()?;
        date.to_datetime(time)
            .to_zoned(TimeZone::UTC)
            .ok()
            .map(|z| Self(z.timestamp()))
    }

    /// Wraps a jiff timestamp.
    #[must_use]
    pub const fn from_jiff(ts: Timestamp) -> Self {
        Self(ts)
    }

    /// The jiff timestamp underneath.
    #[must_use]
    pub const fn as_jiff(self) -> Timestamp {
        self.0
    }

    /// From whole seconds since the epoch plus a nanosecond part below one
    /// second. `None` outside jiff's range (years -9999 to 9999).
    #[must_use]
    pub fn from_timestamp(secs: i64, nanos: u32) -> Option<Self> {
        if nanos >= 1_000_000_000 {
            return None;
        }
        Self::from_nanos_i128(i128::from(secs) * NANOS_PER_SEC + i128::from(nanos))
    }

    /// From milliseconds since the epoch.
    #[must_use]
    pub fn from_timestamp_millis(millis: i64) -> Option<Self> {
        Self::from_nanos_i128(i128::from(millis) * 1_000_000)
    }

    /// From microseconds since the epoch.
    #[must_use]
    pub fn from_timestamp_micros(micros: i64) -> Option<Self> {
        Self::from_nanos_i128(i128::from(micros) * 1_000)
    }

    /// From nanoseconds since the epoch. Every `i64` is in range.
    #[must_use]
    pub fn from_timestamp_nanos(nanos: i64) -> Self {
        Self::from_nanos_i128(i128::from(nanos)).expect("every i64 nanosecond count is in range")
    }

    fn from_nanos_i128(nanos: i128) -> Option<Self> {
        Timestamp::from_nanosecond(nanos).ok().map(Self)
    }

    fn nanos_i128(self) -> i128 {
        self.0.as_nanosecond()
    }

    /// Whole seconds since the epoch, rounded towards negative infinity.
    #[must_use]
    pub fn timestamp(self) -> i64 {
        to_i64(self.nanos_i128().div_euclid(NANOS_PER_SEC))
    }

    /// Milliseconds since the epoch, rounded towards negative infinity.
    #[must_use]
    pub fn timestamp_millis(self) -> i64 {
        to_i64(self.nanos_i128().div_euclid(1_000_000))
    }

    /// Microseconds since the epoch, rounded towards negative infinity.
    #[must_use]
    pub fn timestamp_micros(self) -> i64 {
        to_i64(self.nanos_i128().div_euclid(1_000))
    }

    /// Nanoseconds since the epoch, `None` past the `i64` range (year 2262).
    #[must_use]
    pub fn timestamp_nanos_opt(self) -> Option<i64> {
        i64::try_from(self.nanos_i128()).ok()
    }

    /// The nanoseconds past [`UtcDateTime::timestamp`], `0..1_000_000_000`.
    #[must_use]
    pub fn timestamp_subsec_nanos(self) -> u32 {
        u32::try_from(self.nanos_i128().rem_euclid(NANOS_PER_SEC)).expect("below one second")
    }

    /// The milliseconds past [`UtcDateTime::timestamp`], `0..1000`.
    #[must_use]
    pub fn timestamp_subsec_millis(self) -> u32 {
        self.timestamp_subsec_nanos() / 1_000_000
    }

    /// The microseconds past [`UtcDateTime::timestamp`], `0..1_000_000`.
    #[must_use]
    pub fn timestamp_subsec_micros(self) -> u32 {
        self.timestamp_subsec_nanos() / 1_000
    }

    /// Strict RFC 3339 (`2026-11-17T10:00:00Z`, `…00.5+01:00`), converted to
    /// UTC. `T`, `t` or a space separates date and time; seconds are
    /// required; the offset is `Z`, `z` or `±hh:mm`.
    pub fn parse_from_rfc3339(s: &str) -> Result<Self, ParseError> {
        parse::rfc3339(s, parse::Mode::Strict).map(|p| p.utc)
    }

    /// Parses `s` with a strftime pattern that includes an offset (`%z`,
    /// `%:z`) or an instant (`%s`), converted to UTC.
    pub fn parse_from_str(s: &str, fmt: &str) -> Result<Self, ParseError> {
        Timestamp::strptime(fmt, s)
            .map(Self)
            .map_err(|_| ParseError::invalid())
    }

    /// RFC 3339 with `+00:00` and 0, 3, 6 or 9 fractional digits.
    #[must_use]
    pub fn to_rfc3339(self) -> String {
        self.to_rfc3339_opts(SecondsFormat::AutoSi, false)
    }

    /// RFC 3339 with the given precision (truncated, never rounded); `use_z`
    /// writes `Z` instead of `+00:00`.
    #[must_use]
    pub fn to_rfc3339_opts(self, secform: SecondsFormat, use_z: bool) -> String {
        let mut out = String::with_capacity(32);
        let parts = format::Parts::utc(self);
        format::write_rfc3339(
            &mut out,
            &parts,
            secform,
            if use_z { "Z" } else { "+00:00" },
        );
        out
    }

    /// strftime; see [`mod@format`] for the supported specifiers.
    #[must_use]
    pub fn format(self, fmt: &str) -> format::Formatted<'_> {
        format::Formatted::new(format::Parts::utc(self), fmt)
    }

    /// The UTC calendar date.
    #[must_use]
    pub fn date_naive(self) -> civil::Date {
        self.civil().date()
    }

    /// The UTC calendar date and wall time.
    #[must_use]
    pub fn naive_utc(self) -> civil::DateTime {
        self.civil()
    }

    fn civil(self) -> civil::DateTime {
        TimeZone::UTC.to_datetime(self.0)
    }

    /// The year in UTC.
    #[must_use]
    pub fn year(self) -> i32 {
        i32::from(self.civil().year())
    }

    /// The month in UTC, `1..=12`.
    #[must_use]
    pub fn month(self) -> u32 {
        u32::try_from(self.civil().month()).expect("month is positive")
    }

    /// The day of the month in UTC, `1..=31`.
    #[must_use]
    pub fn day(self) -> u32 {
        u32::try_from(self.civil().day()).expect("day is positive")
    }

    /// The hour in UTC, `0..=23`.
    #[must_use]
    pub fn hour(self) -> u32 {
        u32::try_from(self.civil().hour()).expect("hour is not negative")
    }

    /// The minute in UTC, `0..=59`.
    #[must_use]
    pub fn minute(self) -> u32 {
        u32::try_from(self.civil().minute()).expect("minute is not negative")
    }

    /// The second in UTC, `0..=59`.
    #[must_use]
    pub fn second(self) -> u32 {
        u32::try_from(self.civil().second()).expect("second is not negative")
    }

    /// The day of the year in UTC, `1..=366`.
    #[must_use]
    pub fn ordinal(self) -> u32 {
        u32::try_from(self.civil().day_of_year()).expect("day of year is positive")
    }

    /// Monday = 0 … Sunday = 6, in UTC.
    #[must_use]
    pub fn weekday_from_monday(self) -> u32 {
        u32::try_from(self.civil().weekday().to_monday_zero_offset()).expect("0..=6")
    }

    /// The same instant in the system time zone.
    #[must_use]
    pub fn to_local(self) -> LocalDateTime {
        LocalDateTime(self.0.to_zoned(TimeZone::system()))
    }

    /// The same instant with offset `+00:00`.
    #[must_use]
    pub const fn fixed_offset(self) -> OffsetDateTime {
        OffsetDateTime {
            utc: self,
            offset_secs: 0,
        }
    }

    /// The same instant shown `secs` east of UTC; `None` unless
    /// `|secs| < 86_400` (chrono's `FixedOffset::east_opt` range).
    #[must_use]
    pub fn with_offset_seconds(self, secs: i32) -> Option<OffsetDateTime> {
        (-86_399..=86_399)
            .contains(&secs)
            .then_some(OffsetDateTime {
                utc: self,
                offset_secs: secs,
            })
    }

    /// Rounded down to a whole multiple of `span` since the Unix epoch, like
    /// chrono's `DurationRound::duration_trunc`: an error for a span that is
    /// not positive or longer than the time since the epoch.
    pub fn duration_trunc(self, span: Duration) -> Result<Self, OutOfRangeError> {
        let span = span.0.as_nanos();
        let stamp = self.nanos_i128();
        if span <= 0 || span > stamp.abs() {
            return Err(OutOfRangeError);
        }
        Self::from_nanos_i128(stamp - stamp.rem_euclid(span)).ok_or(OutOfRangeError)
    }

    /// The same second with the fraction set to `nano`; `None` for
    /// `nano >= 1_000_000_000` (chrono's leap-second range is not kept).
    #[must_use]
    pub fn with_nanosecond(self, nano: u32) -> Option<Self> {
        if i128::from(nano) >= NANOS_PER_SEC {
            return None;
        }
        let secs = i128::from(self.timestamp());
        Self::from_nanos_i128(secs * NANOS_PER_SEC + i128::from(nano))
    }

    /// Itself: the counterpart of [`OffsetDateTime::to_utc`] and
    /// [`LocalDateTime::to_utc`], so code generic over the source reads alike.
    #[must_use]
    pub const fn to_utc(self) -> Self {
        self
    }

    /// `self - other`.
    #[must_use]
    pub fn signed_duration_since(self, other: impl Into<UtcDateTime>) -> Duration {
        Duration::from_nanos_i128(self.nanos_i128() - other.into().nanos_i128())
            .expect("the difference of two instants fits a duration")
    }

    /// `self + d`, `None` outside the representable range.
    #[must_use]
    pub fn checked_add_signed(self, d: Duration) -> Option<Self> {
        self.0.checked_add(d.0).ok().map(Self)
    }

    /// `self - d`, `None` outside the representable range.
    #[must_use]
    pub fn checked_sub_signed(self, d: Duration) -> Option<Self> {
        self.0.checked_sub(d.0).ok().map(Self)
    }

    /// The instant truncated to whole seconds (towards the past).
    #[must_use]
    pub fn trunc_subsecs_to_secs(self) -> Self {
        Self::from_nanos_i128(self.nanos_i128() - self.nanos_i128().rem_euclid(NANOS_PER_SEC))
            .expect("truncation stays in range")
    }

    /// The instant as a `SystemTime`.
    #[must_use]
    pub fn to_system_time(self) -> std::time::SystemTime {
        std::time::SystemTime::from(self.0)
    }
}

fn to_i64(v: i128) -> i64 {
    i64::try_from(v).expect("jiff's range fits i64 at this resolution")
}

impl From<std::time::SystemTime> for UtcDateTime {
    /// Panics outside jiff's range (years -9999 to 9999), as chrono's
    /// conversion does outside its own.
    fn from(t: std::time::SystemTime) -> Self {
        Self(Timestamp::try_from(t).expect("system time within years -9999..=9999"))
    }
}

impl From<UtcDateTime> for std::time::SystemTime {
    fn from(t: UtcDateTime) -> Self {
        t.to_system_time()
    }
}

impl From<Timestamp> for UtcDateTime {
    fn from(ts: Timestamp) -> Self {
        Self(ts)
    }
}

impl From<UtcDateTime> for Timestamp {
    fn from(t: UtcDateTime) -> Self {
        t.0
    }
}

impl fmt::Debug for UtcDateTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_rfc3339_opts(SecondsFormat::AutoSi, true))
    }
}

impl fmt::Display for UtcDateTime {
    /// `2026-11-17 10:00:00 UTC`, with 3, 6 or 9 fractional digits when the
    /// instant has them.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = String::with_capacity(32);
        format::write_display(&mut out, &format::Parts::utc(*self));
        f.pad(&out)
    }
}

impl FromStr for UtcDateTime {
    type Err = ParseError;

    /// Relaxed RFC 3339: what serde accepts.
    fn from_str(s: &str) -> Result<Self, ParseError> {
        parse::rfc3339(s, parse::Mode::Relaxed).map(|p| p.utc)
    }
}

impl serde::Serialize for UtcDateTime {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&format_args!("{self:?}"))
    }
}

impl<'de> serde::Deserialize<'de> for UtcDateTime {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl serde::de::Visitor<'_> for Visitor {
            type Value = UtcDateTime;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an RFC 3339 date and time string")
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<UtcDateTime, E> {
                v.parse().map_err(E::custom)
            }
        }
        deserializer.deserialize_str(Visitor)
    }
}

/// The schema schemars' `chrono04` feature gave `DateTime<Utc>`, so MCP
/// tool schemas stay byte for byte the same.
#[cfg(feature = "schemars")]
impl schemars::JsonSchema for UtcDateTime {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        "DateTime".into()
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        "chrono::DateTime".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "format": "date-time"
        })
    }
}

impl Add<Duration> for UtcDateTime {
    type Output = Self;
    fn add(self, d: Duration) -> Self {
        self.checked_add_signed(d)
            .expect("`UtcDateTime + Duration` overflowed")
    }
}

impl Sub<Duration> for UtcDateTime {
    type Output = Self;
    fn sub(self, d: Duration) -> Self {
        self.checked_sub_signed(d)
            .expect("`UtcDateTime - Duration` overflowed")
    }
}

impl AddAssign<Duration> for UtcDateTime {
    fn add_assign(&mut self, d: Duration) {
        *self = *self + d;
    }
}

impl SubAssign<Duration> for UtcDateTime {
    fn sub_assign(&mut self, d: Duration) {
        *self = *self - d;
    }
}

impl Add<std::time::Duration> for UtcDateTime {
    type Output = Self;
    fn add(self, d: std::time::Duration) -> Self {
        self + Duration::from_std(d).expect("std duration within range")
    }
}

impl Sub<std::time::Duration> for UtcDateTime {
    type Output = Self;
    fn sub(self, d: std::time::Duration) -> Self {
        self - Duration::from_std(d).expect("std duration within range")
    }
}

impl Sub for UtcDateTime {
    type Output = Duration;
    fn sub(self, other: Self) -> Duration {
        self.signed_duration_since(other)
    }
}

// ---------------------------------------------------------------------------
// OffsetDateTime: an instant with the UTC offset it was written in
// ---------------------------------------------------------------------------

/// An instant together with a fixed UTC offset, as parsed from RFC 3339.
///
/// Equality, ordering and hashing look at the instant only, as chrono's
/// `DateTime<FixedOffset>` did: `11:00+01:00 == 10:00Z`.
#[derive(Clone, Copy)]
pub struct OffsetDateTime {
    utc: UtcDateTime,
    offset_secs: i32,
}

impl OffsetDateTime {
    /// Strict RFC 3339, keeping the offset.
    pub fn parse_from_rfc3339(s: &str) -> Result<Self, ParseError> {
        parse::rfc3339(s, parse::Mode::Strict).map(|p| Self {
            utc: p.utc,
            offset_secs: p.offset_secs,
        })
    }

    /// The instant in UTC.
    #[must_use]
    pub const fn to_utc(self) -> UtcDateTime {
        self.utc
    }

    /// Seconds east of UTC.
    #[must_use]
    pub const fn offset_seconds(self) -> i32 {
        self.offset_secs
    }

    /// Unix seconds.
    #[must_use]
    pub fn timestamp(self) -> i64 {
        self.utc.timestamp()
    }

    /// Unix milliseconds.
    #[must_use]
    pub fn timestamp_millis(self) -> i64 {
        self.utc.timestamp_millis()
    }

    /// `self - other` as a signed duration.
    #[must_use]
    pub fn signed_duration_since(self, other: impl Into<UtcDateTime>) -> Duration {
        self.utc.signed_duration_since(other)
    }

    /// `self + d` in the same offset, `None` outside the range.
    #[must_use]
    pub fn checked_add_signed(self, d: Duration) -> Option<Self> {
        self.utc
            .checked_add_signed(d)
            .map(|utc| Self { utc, ..self })
    }

    /// `self - d` in the same offset, `None` outside the range.
    #[must_use]
    pub fn checked_sub_signed(self, d: Duration) -> Option<Self> {
        self.utc
            .checked_sub_signed(d)
            .map(|utc| Self { utc, ..self })
    }

    /// RFC 3339 in the original offset (`+00:00` for UTC).
    #[must_use]
    pub fn to_rfc3339(self) -> String {
        self.to_rfc3339_opts(SecondsFormat::AutoSi, false)
    }

    /// RFC 3339 in the original offset with the given precision; `use_z`
    /// writes `Z` for a zero offset, like chrono.
    #[must_use]
    pub fn to_rfc3339_opts(self, secform: SecondsFormat, use_z: bool) -> String {
        let mut out = String::with_capacity(32);
        let parts = format::Parts::with_offset(self.utc, self.offset_secs, None);
        let offset = if use_z && self.offset_secs == 0 {
            "Z".to_owned()
        } else {
            format::offset_colon(self.offset_secs)
        };
        format::write_rfc3339(&mut out, &parts, secform, &offset);
        out
    }

    /// The nanoseconds past [`OffsetDateTime::timestamp`].
    #[must_use]
    pub fn timestamp_subsec_nanos(self) -> u32 {
        self.utc.timestamp_subsec_nanos()
    }

    /// The same instant in the system time zone.
    #[must_use]
    pub fn to_local(self) -> LocalDateTime {
        self.utc.to_local()
    }

    /// strftime in the original offset.
    #[must_use]
    pub fn format(self, fmt: &str) -> format::Formatted<'_> {
        format::Formatted::new(
            format::Parts::with_offset(self.utc, self.offset_secs, None),
            fmt,
        )
    }
}

impl From<OffsetDateTime> for UtcDateTime {
    fn from(t: OffsetDateTime) -> Self {
        t.utc
    }
}

impl PartialEq for OffsetDateTime {
    fn eq(&self, other: &Self) -> bool {
        self.utc == other.utc
    }
}

impl Eq for OffsetDateTime {}

impl std::hash::Hash for OffsetDateTime {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.utc.hash(state);
    }
}

impl PartialEq<UtcDateTime> for OffsetDateTime {
    fn eq(&self, other: &UtcDateTime) -> bool {
        self.utc == *other
    }
}

impl PartialEq<OffsetDateTime> for UtcDateTime {
    fn eq(&self, other: &OffsetDateTime) -> bool {
        *self == other.utc
    }
}

impl PartialOrd<UtcDateTime> for OffsetDateTime {
    fn partial_cmp(&self, other: &UtcDateTime) -> Option<std::cmp::Ordering> {
        Some(self.utc.cmp(other))
    }
}

impl PartialOrd<OffsetDateTime> for UtcDateTime {
    fn partial_cmp(&self, other: &OffsetDateTime) -> Option<std::cmp::Ordering> {
        Some(self.cmp(&other.utc))
    }
}

impl Sub for OffsetDateTime {
    type Output = Duration;
    fn sub(self, other: Self) -> Duration {
        self.utc.signed_duration_since(other)
    }
}

impl Sub<UtcDateTime> for OffsetDateTime {
    type Output = Duration;
    fn sub(self, other: UtcDateTime) -> Duration {
        self.utc.signed_duration_since(other)
    }
}

impl Sub<OffsetDateTime> for UtcDateTime {
    type Output = Duration;
    fn sub(self, other: OffsetDateTime) -> Duration {
        self.signed_duration_since(other)
    }
}

impl Add<Duration> for OffsetDateTime {
    type Output = Self;
    fn add(self, d: Duration) -> Self {
        self.checked_add_signed(d)
            .expect("`OffsetDateTime + Duration` overflowed")
    }
}

impl Sub<Duration> for OffsetDateTime {
    type Output = Self;
    fn sub(self, d: Duration) -> Self {
        self.checked_sub_signed(d)
            .expect("`OffsetDateTime - Duration` overflowed")
    }
}

impl Ord for OffsetDateTime {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.utc.cmp(&other.utc)
    }
}

impl PartialOrd for OffsetDateTime {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Debug for OffsetDateTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_rfc3339())
    }
}

impl fmt::Display for OffsetDateTime {
    /// `2026-11-17 11:00:00 +01:00`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = self.format("%Y-%m-%d %H:%M:%S%.f %:z").to_string();
        f.pad(&s)
    }
}

// ---------------------------------------------------------------------------
// LocalDateTime: the system time zone, for display
// ---------------------------------------------------------------------------

/// An instant in the system time zone, for text shown to people.
#[derive(Clone, PartialEq, Eq)]
pub struct LocalDateTime(jiff::Zoned);

impl LocalDateTime {
    /// The current time in the system time zone.
    #[must_use]
    pub fn now() -> Self {
        Self(jiff::Zoned::now())
    }

    /// The instant in UTC.
    #[must_use]
    pub fn to_utc(&self) -> UtcDateTime {
        UtcDateTime(self.0.timestamp())
    }

    /// Seconds east of UTC at this instant.
    #[must_use]
    pub fn offset_seconds(&self) -> i32 {
        self.0.offset().seconds()
    }

    /// The local calendar date.
    #[must_use]
    pub fn date_naive(&self) -> civil::Date {
        self.0.date()
    }

    /// The local calendar date and wall time.
    #[must_use]
    pub fn naive_local(&self) -> civil::DateTime {
        self.0.datetime()
    }

    /// The local hour, `0..=23`.
    #[must_use]
    pub fn hour(&self) -> u32 {
        u32::try_from(self.0.hour()).expect("hour is not negative")
    }

    /// RFC 3339 in the local offset.
    #[must_use]
    pub fn to_rfc3339(&self) -> String {
        let mut out = String::with_capacity(32);
        let parts = self.parts();
        let offset = format::offset_colon(self.offset_seconds());
        format::write_rfc3339(&mut out, &parts, SecondsFormat::AutoSi, &offset);
        out
    }

    /// strftime in the local zone; `%Z` writes the offset (`+01:00`).
    #[must_use]
    pub fn format<'a>(&self, fmt: &'a str) -> format::Formatted<'a> {
        format::Formatted::new(self.parts(), fmt)
    }

    fn parts(&self) -> format::Parts {
        format::Parts::with_offset(self.to_utc(), self.offset_seconds(), None)
    }

    /// The jiff value underneath.
    #[must_use]
    pub fn as_jiff(&self) -> &jiff::Zoned {
        &self.0
    }
}

impl fmt::Debug for LocalDateTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_rfc3339())
    }
}

impl fmt::Display for LocalDateTime {
    /// `2026-11-17 11:00:00 +01:00`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = self.format("%Y-%m-%d %H:%M:%S%.f %:z").to_string();
        f.pad(&s)
    }
}

// ---------------------------------------------------------------------------
// Duration
// ---------------------------------------------------------------------------

/// A signed span of time with nanosecond precision.
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Duration(SignedDuration);

macro_rules! ctor {
    ($(#[$doc:meta])* $name:ident, $try_name:ident, $nanos:expr) => {
        $(#[$doc])*
        /// Panics when the span does not fit.
        #[must_use]
        pub const fn $name(n: i64) -> Self {
            match Self::$try_name(n) {
                Some(d) => d,
                None => panic!(concat!("Duration::", stringify!($name), " out of bounds")),
            }
        }

        $(#[$doc])*
        /// `None` when the span does not fit.
        #[must_use]
        #[allow(clippy::cast_lossless)] // `i128::from` is not const
        pub const fn $try_name(n: i64) -> Option<Self> {
            Self::from_nanos_i128(n as i128 * $nanos)
        }
    };
}

impl Duration {
    /// Zero.
    pub const ZERO: Self = Self(SignedDuration::ZERO);

    /// Zero.
    #[must_use]
    pub const fn zero() -> Self {
        Self::ZERO
    }

    ctor!(/// `n` weeks.
        weeks, try_weeks, 7 * 86_400 * NANOS_PER_SEC);
    ctor!(/// `n` days of 24 hours.
        days, try_days, 86_400 * NANOS_PER_SEC);
    ctor!(/// `n` hours.
        hours, try_hours, 3_600 * NANOS_PER_SEC);
    ctor!(/// `n` minutes.
        minutes, try_minutes, 60 * NANOS_PER_SEC);
    ctor!(/// `n` seconds.
        seconds, try_seconds, NANOS_PER_SEC);
    ctor!(/// `n` milliseconds.
        milliseconds, try_milliseconds, 1_000_000);
    ctor!(/// `n` microseconds.
        microseconds, try_microseconds, 1_000);

    /// `n` nanoseconds.
    #[must_use]
    pub fn nanoseconds(n: i64) -> Self {
        Self(SignedDuration::from_nanos(n))
    }

    /// The largest span: `i64::MAX` milliseconds, the range LeanCTX's
    /// durations have always had; the smallest is its negation.
    pub const MAX: Self = Self(SignedDuration::new(
        i64::MAX / 1_000,
        (i64::MAX % 1_000) as i32 * 1_000_000,
    ));
    /// The smallest span: `-i64::MAX` milliseconds.
    pub const MIN: Self = Self(SignedDuration::new(
        -(i64::MAX / 1_000),
        -((i64::MAX % 1_000) as i32 * 1_000_000),
    ));

    // const so that chrono's `const X: TimeDelta = TimeDelta::days(1)` keeps compiling.
    #[allow(clippy::cast_possible_truncation)]
    const fn from_nanos_i128(nanos: i128) -> Option<Self> {
        const LIMIT: i128 = i64::MAX as i128 * 1_000_000;
        if nanos < -LIMIT || nanos > LIMIT {
            return None;
        }
        // Both casts are lossless after the bound check above.
        let secs = (nanos / NANOS_PER_SEC) as i64;
        let sub = (nanos % NANOS_PER_SEC) as i32;
        Some(Self(SignedDuration::new(secs, sub)))
    }

    fn nanos_i128(self) -> i128 {
        self.0.as_nanos()
    }

    /// Whole weeks, truncated towards zero.
    #[must_use]
    pub fn num_weeks(self) -> i64 {
        self.num_days() / 7
    }

    /// Whole days, truncated towards zero.
    #[must_use]
    pub fn num_days(self) -> i64 {
        self.num_seconds() / 86_400
    }

    /// Whole hours, truncated towards zero.
    #[must_use]
    pub fn num_hours(self) -> i64 {
        self.num_seconds() / 3_600
    }

    /// Whole minutes, truncated towards zero.
    #[must_use]
    pub fn num_minutes(self) -> i64 {
        self.num_seconds() / 60
    }

    /// Whole seconds, truncated towards zero.
    #[must_use]
    pub fn num_seconds(self) -> i64 {
        self.0.as_secs()
    }

    /// Whole milliseconds, truncated towards zero.
    #[must_use]
    pub fn num_milliseconds(self) -> i64 {
        i64::try_from(self.nanos_i128() / 1_000_000)
            .expect("milliseconds of an i64-second span fit i64")
    }

    /// Whole microseconds, truncated towards zero; `None` past `i64`.
    #[must_use]
    pub fn num_microseconds(self) -> Option<i64> {
        i64::try_from(self.nanos_i128() / 1_000).ok()
    }

    /// Nanoseconds; `None` past `i64`.
    #[must_use]
    pub fn num_nanoseconds(self) -> Option<i64> {
        i64::try_from(self.nanos_i128()).ok()
    }

    /// The nanoseconds past [`Duration::num_seconds`], with its sign.
    #[must_use]
    pub fn subsec_nanos(self) -> i32 {
        self.0.subsec_nanos()
    }

    /// Whole seconds as `f64`, with the fraction.
    #[must_use]
    pub fn as_seconds_f64(self) -> f64 {
        self.0.as_secs_f64()
    }

    /// Zero?
    #[must_use]
    pub fn is_zero(self) -> bool {
        self.0.is_zero()
    }

    /// Below zero?
    #[must_use]
    pub fn is_negative(self) -> bool {
        self.0.is_negative()
    }

    /// The absolute value.
    #[must_use]
    pub fn abs(self) -> Self {
        Self(self.0.abs())
    }

    /// `self + rhs`, `None` outside [`Duration::MIN`]..=[`Duration::MAX`].
    #[must_use]
    pub fn checked_add(&self, rhs: &Self) -> Option<Self> {
        Self::from_nanos_i128(self.nanos_i128() + rhs.nanos_i128())
    }

    /// `self - rhs`, `None` outside [`Duration::MIN`]..=[`Duration::MAX`].
    #[must_use]
    pub fn checked_sub(&self, rhs: &Self) -> Option<Self> {
        Self::from_nanos_i128(self.nanos_i128() - rhs.nanos_i128())
    }

    /// From a std duration; `Err` past [`Duration::MAX`].
    pub fn from_std(d: std::time::Duration) -> Result<Self, OutOfRangeError> {
        Self::from_nanos_i128(i128::try_from(d.as_nanos()).map_err(|_| OutOfRangeError)?)
            .ok_or(OutOfRangeError)
    }

    /// To a std duration; `Err` when negative.
    pub fn to_std(self) -> Result<std::time::Duration, OutOfRangeError> {
        std::time::Duration::try_from(self.0).map_err(|_| OutOfRangeError)
    }

    /// Wraps a jiff signed duration.
    #[must_use]
    pub const fn from_jiff(d: SignedDuration) -> Self {
        Self(d)
    }

    /// The jiff signed duration underneath.
    #[must_use]
    pub const fn as_jiff(self) -> SignedDuration {
        self.0
    }
}

impl Add for Duration {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        self.checked_add(&rhs)
            .expect("`Duration + Duration` overflowed")
    }
}

impl Sub for Duration {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        self.checked_sub(&rhs)
            .expect("`Duration - Duration` overflowed")
    }
}

impl AddAssign for Duration {
    fn add_assign(&mut self, rhs: Self) {
        *self = *self + rhs;
    }
}

impl SubAssign for Duration {
    fn sub_assign(&mut self, rhs: Self) {
        *self = *self - rhs;
    }
}

impl Neg for Duration {
    type Output = Self;
    fn neg(self) -> Self {
        Self(-self.0)
    }
}

impl Mul<i32> for Duration {
    type Output = Self;
    fn mul(self, rhs: i32) -> Self {
        Self::from_nanos_i128(self.nanos_i128() * i128::from(rhs))
            .expect("`Duration * i32` overflowed")
    }
}

impl Div<i32> for Duration {
    type Output = Self;
    fn div(self, rhs: i32) -> Self {
        assert!(rhs != 0, "`Duration / i32` divided by zero");
        // chrono's two-step division (whole seconds, then the nanosecond
        // part with the carried remainder), so results agree to the
        // nanosecond, including its truncation.
        let total = self.nanos_i128();
        let (secs, nanos) = (
            total.div_euclid(NANOS_PER_SEC),
            total.rem_euclid(NANOS_PER_SEC),
        );
        let rhs = i128::from(rhs);
        let mut q_secs = secs / rhs;
        let carry = secs % rhs;
        let mut q_nanos = nanos / rhs + carry * NANOS_PER_SEC / rhs;
        if q_nanos < 0 {
            q_secs -= 1;
            q_nanos += NANOS_PER_SEC;
        } else if q_nanos >= NANOS_PER_SEC {
            q_secs += 1;
            q_nanos -= NANOS_PER_SEC;
        }
        Self::from_nanos_i128(q_secs * NANOS_PER_SEC + q_nanos).expect("a quotient stays in range")
    }
}

impl std::iter::Sum for Duration {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Self::ZERO, Add::add)
    }
}

impl<'a> std::iter::Sum<&'a Duration> for Duration {
    fn sum<I: Iterator<Item = &'a Self>>(iter: I) -> Self {
        iter.copied().sum()
    }
}

impl fmt::Debug for Duration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl fmt::Display for Duration {
    /// ISO 8601 in seconds: `PT90S`, `PT0.25S`, `-PT1.5S`, `P0D` for zero.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let nanos = self.nanos_i128();
        let sign = if nanos < 0 { "-" } else { "" };
        let abs = nanos.unsigned_abs();
        if abs == 0 {
            return f.write_str("P0D");
        }
        let secs = abs / 1_000_000_000;
        let mut frac = abs % 1_000_000_000;
        write!(f, "{sign}PT{secs}")?;
        if frac > 0 {
            let mut width = 9usize;
            while frac.is_multiple_of(10) {
                frac /= 10;
                width -= 1;
            }
            write!(f, ".{frac:0width$}")?;
        }
        f.write_str("S")
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests;
