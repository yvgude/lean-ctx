// SPDX-License-Identifier: Apache-2.0
//! strftime and the fixed text formats.
//!
//! Specifiers (the set LeanCTX has always used):
//!
//! | spec | meaning | example |
//! |---|---|---|
//! | `%Y` `%C` `%y` | year, century, two-digit year | `2026` `20` `26` |
//! | `%m` `%b`/`%h` `%B` | month, short name, long name | `11` `Nov` `November` |
//! | `%d` `%e` `%j` | day, space-padded day, day of year | `07` ` 7` `311` |
//! | `%a` `%A` `%u` `%w` | weekday short, long, Mon=1, Sun=0 | `Tue` `Tuesday` `2` `2` |
//! | `%U` `%W` `%G` `%g` `%V` | week numbers (Sun, Mon, ISO) | |
//! | `%H` `%k` `%I` `%l` `%p` `%P` | hours, AM/PM | `09` ` 9` `09` ` 9` `AM` `am` |
//! | `%M` `%S` `%s` | minute, second, Unix seconds | |
//! | `%f` `%3f` `%6f` `%9f` | fraction without a dot (9, 3, 6, 9 digits) | `250000000` |
//! | `%.f` `%.3f` `%.6f` `%.9f` | fraction with a dot (0/3/6/9, or fixed) | `.250` |
//! | `%z` `%:z` `%::z` `%:::z` | offset | `+0100` `+01:00` `+01:00:00` `+01` |
//! | `%Z` | `UTC`, or the offset `+01:00` | |
//! | `%F` `%T` `%R` `%D` `%x` `%X` `%r` `%c` `%v` `%+` | composites | |
//! | `%%` `%n` `%t` | `%`, newline, tab | |
//!
//! `-` (no padding), `_` (spaces) and `0` (zeros) may follow `%` for the
//! numeric specifiers. An unknown specifier is written unchanged.

use std::fmt::{self, Write};

use jiff::civil;
use jiff::tz::{Offset, TimeZone};

use crate::{SecondsFormat, UtcDateTime};

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const WEEKDAYS: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];

/// The fields a format reads.
#[derive(Clone, Debug)]
pub(crate) struct Parts {
    civil: civil::DateTime,
    nanos: u32,
    offset_secs: i32,
    unix_secs: i64,
    zone_name: Option<&'static str>,
}

impl Parts {
    pub(crate) fn utc(t: UtcDateTime) -> Self {
        Self::with_offset(t, 0, Some("UTC"))
    }

    pub(crate) fn with_offset(
        t: UtcDateTime,
        offset_secs: i32,
        zone_name: Option<&'static str>,
    ) -> Self {
        let offset = Offset::from_seconds(offset_secs).expect("offset within ±25:59:59");
        let civil = TimeZone::fixed(offset).to_datetime(t.as_jiff());
        Self {
            civil,
            nanos: t.timestamp_subsec_nanos(),
            offset_secs,
            unix_secs: t.timestamp(),
            zone_name,
        }
    }

    fn year(&self) -> i32 {
        i32::from(self.civil.year())
    }
    fn month(&self) -> u32 {
        self.civil.month().unsigned_abs().into()
    }
    fn day(&self) -> u32 {
        self.civil.day().unsigned_abs().into()
    }
    fn hour(&self) -> u32 {
        self.civil.hour().unsigned_abs().into()
    }
    fn minute(&self) -> u32 {
        self.civil.minute().unsigned_abs().into()
    }
    fn second(&self) -> u32 {
        self.civil.second().unsigned_abs().into()
    }
    /// Monday = 0.
    fn weekday0(&self) -> usize {
        usize::from(self.civil.weekday().to_monday_zero_offset().unsigned_abs())
    }
    fn ordinal(&self) -> u32 {
        self.civil.day_of_year().unsigned_abs().into()
    }
}

/// `+01:00`.
pub(crate) fn offset_colon(secs: i32) -> String {
    let mut s = String::with_capacity(6);
    write_offset(&mut s, secs, OffsetStyle::Colon);
    s
}

#[derive(Clone, Copy)]
enum OffsetStyle {
    /// `+0100`
    Plain,
    /// `+01:00`
    Colon,
    /// `+01:00:00`
    ColonSeconds,
    /// `+01`
    HoursOnly,
}

fn write_offset(out: &mut String, secs: i32, style: OffsetStyle) {
    let sign = if secs < 0 { '-' } else { '+' };
    let abs = secs.unsigned_abs();
    let (h, m, s) = (abs / 3600, abs / 60 % 60, abs % 60);
    let _ = match style {
        OffsetStyle::Plain => write!(out, "{sign}{h:02}{m:02}"),
        OffsetStyle::Colon => write!(out, "{sign}{h:02}:{m:02}"),
        OffsetStyle::ColonSeconds => write!(out, "{sign}{h:02}:{m:02}:{s:02}"),
        OffsetStyle::HoursOnly => write!(out, "{sign}{h:02}"),
    };
}

fn write_year(out: &mut String, year: i32) {
    let _ = if (0..=9999).contains(&year) {
        write!(out, "{year:04}")
    } else {
        write!(out, "{year:+05}")
    };
}

/// `.250`, `.250000`, `.250000001` or nothing.
fn write_auto_fraction(out: &mut String, nanos: u32) {
    let _ = if nanos == 0 {
        Ok(())
    } else if nanos.is_multiple_of(1_000_000) {
        write!(out, ".{:03}", nanos / 1_000_000)
    } else if nanos.is_multiple_of(1_000) {
        write!(out, ".{:06}", nanos / 1_000)
    } else {
        write!(out, ".{nanos:09}")
    };
}

fn write_fixed_fraction(out: &mut String, nanos: u32, digits: u32, dot: bool) {
    if dot {
        out.push('.');
    }
    let _ = match digits {
        3 => write!(out, "{:03}", nanos / 1_000_000),
        6 => write!(out, "{:06}", nanos / 1_000),
        _ => write!(out, "{nanos:09}"),
    };
}

pub(crate) fn write_rfc3339(out: &mut String, p: &Parts, secform: SecondsFormat, offset: &str) {
    write_year(out, p.year());
    let _ = write!(
        out,
        "-{:02}-{:02}T{:02}:{:02}:{:02}",
        p.month(),
        p.day(),
        p.hour(),
        p.minute(),
        p.second()
    );
    match secform {
        SecondsFormat::Secs => {}
        SecondsFormat::Millis => write_fixed_fraction(out, p.nanos, 3, true),
        SecondsFormat::Micros => write_fixed_fraction(out, p.nanos, 6, true),
        SecondsFormat::Nanos => write_fixed_fraction(out, p.nanos, 9, true),
        SecondsFormat::AutoSi => write_auto_fraction(out, p.nanos),
    }
    out.push_str(offset);
}

/// `2026-11-17 10:00:00 UTC`.
pub(crate) fn write_display(out: &mut String, p: &Parts) {
    write_year(out, p.year());
    let _ = write!(
        out,
        "-{:02}-{:02} {:02}:{:02}:{:02}",
        p.month(),
        p.day(),
        p.hour(),
        p.minute(),
        p.second()
    );
    write_auto_fraction(out, p.nanos);
    out.push(' ');
    match p.zone_name {
        Some(name) => out.push_str(name),
        None => write_offset(out, p.offset_secs, OffsetStyle::Colon),
    }
}

/// A strftime result; render it with `Display` or `to_string()`.
#[derive(Clone, Debug)]
pub struct Formatted<'a> {
    parts: Parts,
    fmt: &'a str,
}

impl<'a> Formatted<'a> {
    pub(crate) fn new(parts: Parts, fmt: &'a str) -> Self {
        Self { parts, fmt }
    }
}

impl fmt::Display for Formatted<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = String::with_capacity(self.fmt.len() + 16);
        strftime(&mut out, &self.parts, self.fmt);
        f.pad(&out)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pad {
    Default,
    None,
    Space,
    Zero,
}

fn num(out: &mut String, v: i64, width: usize, default: Pad, pad: Pad) {
    let pad = if pad == Pad::Default { default } else { pad };
    let _ = match pad {
        Pad::None => write!(out, "{v}"),
        Pad::Space => write!(out, "{v:>width$}"),
        Pad::Zero | Pad::Default => {
            if v < 0 {
                write!(
                    out,
                    "-{:0w$}",
                    v.unsigned_abs(),
                    w = width.saturating_sub(1)
                )
            } else {
                write!(out, "{v:0width$}")
            }
        }
    };
}

/// Week of the year with weeks starting on `first` (0 = Monday, 6 = Sunday);
/// days before the first such weekday are week 0.
fn week_number(p: &Parts, first_is_sunday: bool) -> i64 {
    let ordinal0 = i64::from(p.ordinal()) - 1;
    let wd_from_first = if first_is_sunday {
        (p.weekday0() as i64 + 1) % 7
    } else {
        p.weekday0() as i64
    };
    (ordinal0 - wd_from_first + 7) / 7
}

fn strftime(out: &mut String, p: &Parts, fmt: &str) {
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let mut pad = Pad::Default;
        match chars.peek() {
            Some('-') => pad = Pad::None,
            Some('_') => pad = Pad::Space,
            Some('0') => pad = Pad::Zero,
            _ => {}
        }
        if pad != Pad::Default {
            chars.next();
        }
        let Some(spec) = chars.next() else {
            out.push('%');
            break;
        };
        match spec {
            'Y' => {
                if pad == Pad::Default {
                    write_year(out, p.year());
                } else {
                    num(out, i64::from(p.year()), 4, Pad::Zero, pad);
                }
            }
            'C' => num(out, i64::from(p.year().div_euclid(100)), 2, Pad::Zero, pad),
            'y' => num(out, i64::from(p.year().rem_euclid(100)), 2, Pad::Zero, pad),
            'm' => num(out, i64::from(p.month()), 2, Pad::Zero, pad),
            'b' | 'h' => out.push_str(&MONTHS[(p.month() - 1) as usize][..3]),
            'B' => out.push_str(MONTHS[(p.month() - 1) as usize]),
            'd' => num(out, i64::from(p.day()), 2, Pad::Zero, pad),
            'e' => num(out, i64::from(p.day()), 2, Pad::Space, pad),
            'j' => num(out, i64::from(p.ordinal()), 3, Pad::Zero, pad),
            'a' => out.push_str(&WEEKDAYS[p.weekday0()][..3]),
            'A' => out.push_str(WEEKDAYS[p.weekday0()]),
            'u' => num(out, p.weekday0() as i64 + 1, 1, Pad::Zero, pad),
            'w' => num(out, (p.weekday0() as i64 + 1) % 7, 1, Pad::Zero, pad),
            'U' => num(out, week_number(p, true), 2, Pad::Zero, pad),
            'W' => num(out, week_number(p, false), 2, Pad::Zero, pad),
            'G' | 'g' | 'V' => {
                let iso = p.civil.date().iso_week_date();
                match spec {
                    'G' => num(out, i64::from(iso.year()), 4, Pad::Zero, pad),
                    'g' => num(
                        out,
                        i64::from(i32::from(iso.year()).rem_euclid(100)),
                        2,
                        Pad::Zero,
                        pad,
                    ),
                    _ => num(out, i64::from(iso.week()), 2, Pad::Zero, pad),
                }
            }
            'H' => num(out, i64::from(p.hour()), 2, Pad::Zero, pad),
            'k' => num(out, i64::from(p.hour()), 2, Pad::Space, pad),
            'I' => num(out, i64::from((p.hour() + 11) % 12 + 1), 2, Pad::Zero, pad),
            'l' => num(out, i64::from((p.hour() + 11) % 12 + 1), 2, Pad::Space, pad),
            'p' => out.push_str(if p.hour() < 12 { "AM" } else { "PM" }),
            'P' => out.push_str(if p.hour() < 12 { "am" } else { "pm" }),
            'M' => num(out, i64::from(p.minute()), 2, Pad::Zero, pad),
            'S' => num(out, i64::from(p.second()), 2, Pad::Zero, pad),
            's' => {
                let _ = write!(out, "{}", p.unix_secs);
            }
            'f' => write_fixed_fraction(out, p.nanos, 9, false),
            '3' | '6' | '9' if chars.peek() == Some(&'f') => {
                chars.next();
                write_fixed_fraction(out, p.nanos, spec.to_digit(10).unwrap_or(9), false);
            }
            '.' => match (chars.peek().copied(), chars.clone().nth(1)) {
                (Some('f'), _) => {
                    chars.next();
                    write_auto_fraction(out, p.nanos);
                }
                (Some(d @ ('3' | '6' | '9')), Some('f')) => {
                    chars.next();
                    chars.next();
                    write_fixed_fraction(out, p.nanos, d.to_digit(10).unwrap_or(9), true);
                }
                _ => out.push_str("%."),
            },
            'z' => write_offset(out, p.offset_secs, OffsetStyle::Plain),
            ':' => {
                let mut colons = 1;
                while chars.peek() == Some(&':') {
                    chars.next();
                    colons += 1;
                }
                if chars.peek() == Some(&'z') && colons <= 3 {
                    chars.next();
                    let style = match colons {
                        1 => OffsetStyle::Colon,
                        2 => OffsetStyle::ColonSeconds,
                        _ => OffsetStyle::HoursOnly,
                    };
                    write_offset(out, p.offset_secs, style);
                } else {
                    out.push('%');
                    out.extend(std::iter::repeat_n(':', colons));
                }
            }
            'Z' => match p.zone_name {
                Some(name) => out.push_str(name),
                None => write_offset(out, p.offset_secs, OffsetStyle::Colon),
            },
            'F' => strftime(out, p, "%Y-%m-%d"),
            'T' | 'X' => strftime(out, p, "%H:%M:%S"),
            'R' => strftime(out, p, "%H:%M"),
            'D' | 'x' => strftime(out, p, "%m/%d/%y"),
            'r' => strftime(out, p, "%I:%M:%S %p"),
            'c' => strftime(out, p, "%a %b %e %H:%M:%S %Y"),
            'v' => strftime(out, p, "%e-%b-%Y"),
            '+' => strftime(out, p, "%Y-%m-%dT%H:%M:%S%.f%:z"),
            '%' => out.push('%'),
            'n' => out.push('\n'),
            't' => out.push('\t'),
            other => {
                out.push('%');
                match pad {
                    Pad::None => out.push('-'),
                    Pad::Space => out.push('_'),
                    Pad::Zero => out.push('0'),
                    Pad::Default => {}
                }
                out.push(other);
            }
        }
    }
}
