// SPDX-License-Identifier: Apache-2.0
//! RFC 3339 parsing with the strict and relaxed rules LeanCTX has always used.

use std::fmt;

use jiff::tz::{Offset, TimeZone};
use jiff::{Timestamp, civil};

use crate::UtcDateTime;

/// Why a date and time string did not parse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParseError(Kind);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Invalid,
    TooShort,
    TooLong,
    OutOfRange,
}

impl ParseError {
    pub(crate) const fn invalid() -> Self {
        Self(Kind::Invalid)
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self.0 {
            Kind::Invalid => "input contains invalid characters",
            Kind::TooShort => "premature end of input",
            Kind::TooLong => "trailing input",
            Kind::OutOfRange => "input is out of range",
        })
    }
}

impl std::error::Error for ParseError {}

const INVALID: ParseError = ParseError(Kind::Invalid);
const TOO_SHORT: ParseError = ParseError(Kind::TooShort);
const TOO_LONG: ParseError = ParseError(Kind::TooLong);
const OUT_OF_RANGE: ParseError = ParseError(Kind::OutOfRange);

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// `YYYY-MM-DD(T|t| )hh:mm:ss[.f](Z|z|±hh:mm)`, nothing around it.
    Strict,
    /// Also: whitespace around fields and before the offset, one-digit
    /// month/day/time fields, signed or longer years, `UTC`, and offsets
    /// written `±hhmm` or `±hh mm`.
    Relaxed,
}

pub(crate) struct Parsed {
    pub(crate) utc: UtcDateTime,
    pub(crate) offset_secs: i32,
}

struct Cursor<'a> {
    s: &'a [u8],
}

impl Cursor<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.first().copied()
    }

    fn bump(&mut self) {
        self.s = &self.s[1..];
    }

    fn expect(&mut self, want: u8) -> Result<(), ParseError> {
        match self.peek() {
            Some(b) if b == want => {
                self.bump();
                Ok(())
            }
            Some(_) => Err(INVALID),
            None => Err(TOO_SHORT),
        }
    }

    /// The minus sign may also be U+2212.
    fn sign(&mut self) -> Option<i64> {
        match self.peek() {
            Some(b'+') => {
                self.bump();
                Some(1)
            }
            Some(b'-') => {
                self.bump();
                Some(-1)
            }
            Some(0xE2) if self.s.starts_with("\u{2212}".as_bytes()) => {
                self.s = &self.s[3..];
                Some(-1)
            }
            _ => None,
        }
    }
}

/// Builds the instant. jiff's instants end at 9999-12-30T22:00:00.999999999Z
/// (and start at -009999-01-02T01:59:59Z), while chrono accepted every valid
/// calendar value of those years. A valid date and time past either end is
/// clamped to it, in UTC (offset 0): far-future sentinels such as
/// `9999-12-31T23:59:59Z` ("never expires", cloud key sets) keep parsing and
/// keep comparing later than any real instant. Invalid values stay errors.
fn build(
    year: i64,
    month: i64,
    day: i64,
    hms: (i64, i64, i64),
    nanos: i64,
    offset_secs: i32,
) -> Result<Parsed, ParseError> {
    let small = |v: i64| i8::try_from(v).map_err(|_| OUT_OF_RANGE);
    let year = i16::try_from(year).map_err(|_| OUT_OF_RANGE)?;
    let date = civil::Date::new(year, small(month)?, small(day)?).map_err(|_| OUT_OF_RANGE)?;
    let time = civil::Time::new(
        small(hms.0)?,
        small(hms.1)?,
        small(hms.2)?,
        i32::try_from(nanos).map_err(|_| OUT_OF_RANGE)?,
    )
    .map_err(|_| OUT_OF_RANGE)?;
    let tz = TimeZone::fixed(Offset::from_seconds(offset_secs).map_err(|_| OUT_OF_RANGE)?);
    if let Ok(ts) = tz.to_timestamp(date.to_datetime(time)) {
        return Ok(Parsed {
            utc: UtcDateTime::from_jiff(ts),
            offset_secs,
        });
    }
    let edge = match year {
        9999 => Timestamp::MAX,
        -9999 => Timestamp::MIN,
        _ => return Err(OUT_OF_RANGE),
    };
    Ok(Parsed {
        utc: UtcDateTime::from_jiff(edge),
        offset_secs: 0,
    })
}

/// The strict RFC 3339 reader, in chrono's order: a fixed 19-byte
/// `YYYY-MM-DDThh:mm:ss` head (the date is validated before the time is
/// read), an optional fraction, then the offset. The error kind for a bad
/// input is therefore the one chrono reported.
fn strict(input: &str) -> Result<Parsed, ParseError> {
    let b = input.as_bytes();
    if b.len() < 19 {
        return Err(TOO_SHORT);
    }
    let digit = |i: usize| {
        if b[i].is_ascii_digit() {
            Ok(i64::from(b[i] - b'0'))
        } else {
            Err(INVALID)
        }
    };
    let lit = |i: usize, want: u8| if b[i] == want { Ok(()) } else { Err(INVALID) };
    let year = digit(0)? * 1000 + digit(1)? * 100 + digit(2)? * 10 + digit(3)?;
    lit(4, b'-')?;
    let month = digit(5)? * 10 + digit(6)?;
    lit(7, b'-')?;
    let day = digit(8)? * 10 + digit(9)?;
    let valid_date = (1..=12).contains(&month)
        && i8::try_from(month).is_ok_and(|m| {
            civil::Date::new(year as i16, m, 1)
                .is_ok_and(|d| (1..=i64::from(d.days_in_month())).contains(&day))
        });
    if !valid_date {
        return Err(OUT_OF_RANGE);
    }
    if !matches!(b[10], b'T' | b't' | b' ') {
        return Err(INVALID);
    }
    let hour = digit(11)? * 10 + digit(12)?;
    lit(13, b':')?;
    let minute = digit(14)? * 10 + digit(15)?;
    lit(16, b':')?;
    let second = digit(17)? * 10 + digit(18)?;

    let mut c = Cursor { s: &b[19..] };
    let mut nanos = 0;
    if c.peek() == Some(b'.') {
        c.bump();
        let mut n = 0;
        while let Some(d) = c.peek().filter(u8::is_ascii_digit) {
            if n < 9 {
                nanos = nanos * 10 + i64::from(d - b'0');
                n += 1;
            }
            c.bump();
        }
        if n == 0 {
            return Err(if c.s.is_empty() { TOO_SHORT } else { INVALID });
        }
        nanos *= 10_i64.pow(9 - n);
    }
    if hour > 23 || minute > 59 || second > 60 {
        return Err(OUT_OF_RANGE);
    }
    // A leap second (`:60`) is kept as the last nanosecond of `:59`.
    let (second, nanos) = if second == 60 {
        (59, 999_999_999)
    } else {
        (second, nanos)
    };

    let offset_secs = if let Some(b'Z' | b'z') = c.peek() {
        c.bump();
        0
    } else {
        let sign = match c.sign() {
            Some(s) => s,
            None if c.s.is_empty() => return Err(TOO_SHORT),
            None => return Err(INVALID),
        };
        if c.s.len() < 2 {
            return Err(TOO_SHORT);
        }
        let hours = match (c.s[0], c.s[1]) {
            (h1 @ b'0'..=b'9', h2 @ b'0'..=b'9') => i64::from((h1 - b'0') * 10 + (h2 - b'0')),
            _ => return Err(INVALID),
        };
        c.s = &c.s[2..];
        c.expect(b':')?;
        if c.s.len() < 2 {
            return Err(TOO_SHORT);
        }
        let minutes = match (c.s[0], c.s[1]) {
            (m1 @ b'0'..=b'5', m2 @ b'0'..=b'9') => i64::from((m1 - b'0') * 10 + (m2 - b'0')),
            (b'6'..=b'9', b'0'..=b'9') => return Err(OUT_OF_RANGE),
            _ => return Err(INVALID),
        };
        c.s = &c.s[2..];
        sign * (hours * 3600 + minutes * 60)
    };
    if !c.s.is_empty() {
        return Err(TOO_LONG);
    }
    if offset_secs.abs() >= 86_400 {
        return Err(OUT_OF_RANGE);
    }
    build(
        year,
        month,
        day,
        (hour, minute, second),
        nanos,
        offset_secs as i32,
    )
}

pub(crate) fn rfc3339(input: &str, mode: Mode) -> Result<Parsed, ParseError> {
    match mode {
        Mode::Strict => strict(input),
        Mode::Relaxed => relaxed(input),
    }
}

/// `min..=max` ASCII digits, as chrono's scanner read them.
fn number(s: &str, min: usize, max: usize) -> Result<(&str, i64), ParseError> {
    let b = s.as_bytes();
    if b.len() < min {
        return Err(TOO_SHORT);
    }
    let mut n: i64 = 0;
    for (i, &c) in b.iter().take(max).enumerate() {
        if !c.is_ascii_digit() {
            return if i < min {
                Err(INVALID)
            } else {
                Ok((&s[i..], n))
            };
        }
        n = n
            .checked_mul(10)
            .and_then(|n| n.checked_add(i64::from(c - b'0')))
            .ok_or(OUT_OF_RANGE)?;
    }
    Ok((&s[max.min(b.len())..], n))
}

fn literal<'a>(s: &'a str, want: &str) -> Result<&'a str, ParseError> {
    if s.len() < want.len() {
        Err(TOO_SHORT)
    } else if let Some(rest) = s.strip_prefix(want) {
        Ok(rest)
    } else {
        Err(INVALID)
    }
}

/// A one- or two-digit field after optional whitespace, range-checked at once.
fn field(s: &str, lo: i64, hi: i64) -> Result<(&str, i64), ParseError> {
    let (s, v) = number(s.trim_start(), 1, 2)?;
    if (lo..=hi).contains(&v) {
        Ok((s, v))
    } else {
        Err(OUT_OF_RANGE)
    }
}

/// The serde and `FromStr` reader, in chrono's order: whitespace before
/// fields and separators, one- or two-digit fields, a signed year of any
/// length, `UTC`, and offsets whose `:` may be spaces, repeated, or absent.
fn relaxed(input: &str) -> Result<Parsed, ParseError> {
    let s = input.trim_start();
    let (s, year) = if let Some(rest) = s.strip_prefix('-') {
        let (s, v) = number(rest, 1, usize::MAX)?;
        (s, -v)
    } else if let Some(rest) = s.strip_prefix('+') {
        number(rest, 1, usize::MAX)?
    } else {
        number(s, 1, 4)?
    };
    if i32::try_from(year).is_err() {
        return Err(OUT_OF_RANGE);
    }
    let s = literal(s.trim_start(), "-")?;
    let (s, month) = field(s, 1, 12)?;
    let s = literal(s.trim_start(), "-")?;
    let (s, day) = field(s, 1, 31)?;
    let s = match s.as_bytes().first() {
        Some(b't' | b'T' | b' ') => &s[1..],
        Some(_) => return Err(INVALID),
        None => return Err(TOO_SHORT),
    };
    let (s, hour) = field(s, 0, 23)?;
    let s = literal(s.trim_start(), ":")?;
    let (s, minute) = field(s, 0, 59)?;
    let s = literal(s.trim_start(), ":")?;
    let (mut s, second) = field(s, 0, 60)?;
    let mut nanos = 0;
    if let Some(rest) = s.strip_prefix('.') {
        let (r, v) = number(rest, 1, 9)?;
        nanos = v * 10_i64.pow(9 - (rest.len() - r.len()) as u32);
        s = r.trim_start_matches(|c: char| c.is_ascii_digit());
    }
    let s = s.trim_start();
    let (s, offset_secs) = if s.len() >= 3 && s.as_bytes()[..3].eq_ignore_ascii_case(b"UTC") {
        (&s[3..], 0)
    } else {
        offset_colon_or_space(s)?
    };
    if !s.trim_start().is_empty() {
        return Err(TOO_LONG);
    }
    if offset_secs.abs() >= 86_400 {
        return Err(OUT_OF_RANGE);
    }
    // A leap second (`:60`) is kept as the last nanosecond of `:59`.
    let (second, nanos) = if second == 60 {
        (59, 999_999_999)
    } else {
        (second, nanos)
    };
    build(
        year,
        month,
        day,
        (hour, minute, second),
        nanos,
        offset_secs as i32,
    )
}

/// `Z`, or `±hh` then any run of `:` and whitespace, then `mm`.
fn offset_colon_or_space(s: &str) -> Result<(&str, i64), ParseError> {
    if let Some(rest) = s.strip_prefix(['Z', 'z']) {
        return Ok((rest, 0));
    }
    let (sign, s) = if let Some(r) = s.strip_prefix('+') {
        (1, r)
    } else if let Some(r) = s.strip_prefix('-') {
        (-1, r)
    } else if let Some(r) = s.strip_prefix('\u{2212}') {
        (-1, r)
    } else if s.is_empty() {
        return Err(TOO_SHORT);
    } else {
        return Err(INVALID);
    };
    let b = s.as_bytes();
    if b.len() < 2 {
        return Err(TOO_SHORT);
    }
    let hours = match (b[0], b[1]) {
        (h1 @ b'0'..=b'9', h2 @ b'0'..=b'9') => i64::from((h1 - b'0') * 10 + (h2 - b'0')),
        _ => return Err(INVALID),
    };
    let s = s[2..].trim_start_matches(|c: char| c == ':' || c.is_whitespace());
    let b = s.as_bytes();
    if b.len() < 2 {
        return Err(TOO_SHORT);
    }
    let minutes = match (b[0], b[1]) {
        (m1 @ b'0'..=b'5', m2 @ b'0'..=b'9') => i64::from((m1 - b'0') * 10 + (m2 - b'0')),
        (b'6'..=b'9', b'0'..=b'9') => return Err(OUT_OF_RANGE),
        _ => return Err(INVALID),
    };
    Ok((&s[2..], sign * (hours * 3600 + minutes * 60)))
}
