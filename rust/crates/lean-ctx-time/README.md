# lean-ctx-time

Time for LeanCTX: UTC instants, signed durations and fixed-offset or local
date-times over [jiff](https://crates.io/crates/jiff).

LeanCTX used chrono until RUSTSEC-2026-0543 marked it unmaintained. This crate
keeps chrono's text formats byte for byte, so state, receipts and wire data
written before the switch read the same afterwards:

- serde and `Debug`: RFC 3339 with `Z` and 0, 3, 6 or 9 fraction digits;
- `to_rfc3339()` with `+00:00`, `to_rfc3339_opts` with every `SecondsFormat`;
- `Display`: `YYYY-MM-DD HH:MM:SS[.f] UTC`; durations as ISO 8601 (`PT90S`);
- strftime, and chrono's strict (`parse_from_rfc3339`) and relaxed
  (`FromStr`, serde) parsers, including their error kinds.

A differential test against chrono 0.4.45 compared 37.5 million outputs and
1.6 million parse inputs without a difference before chrono was removed.

Calendar dates and wall-clock times are jiff's civil types, re-exported as
`Date`, `Time` and `CivilDateTime`. `parse_date` and `parse_civil_datetime`
parse them like chrono's `parse_from_str` (17.7 million compared inputs) but
reject a leading `+` or `-` on the year. `utc_from_civil`,
`utc_from_local_civil` and `days_between` cover what `NaiveDate` and
`NaiveDateTime` did; `+`/`-` with a `Duration` works on both.

One deliberate difference: a leap second (`:60`) is stored as the last
nanosecond of `:59`, because jiff has no leap seconds.

Licence: Apache-2.0.
