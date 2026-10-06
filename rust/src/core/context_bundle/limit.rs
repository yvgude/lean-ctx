// SPDX-License-Identifier: Apache-2.0
//! Output budget: a size limit and the unit it is measured in.

use std::fmt;

/// What a bundle limit counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Unit {
    /// Unicode scalar values — what chat input boxes count.
    #[default]
    Chars,
    /// `o200k_base` BPE tokens, the tokenizer used for all lean-ctx accounting.
    Tokens,
}

impl Unit {
    pub(crate) fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "chars" | "char" | "characters" => Ok(Self::Chars),
            "tokens" | "token" => Ok(Self::Tokens),
            other => Err(format!("unknown unit '{other}' (expected chars or tokens)")),
        }
    }

    /// Size of `text` in this unit.
    pub(crate) fn measure(self, text: &str) -> usize {
        match self {
            Self::Chars => text.chars().count(),
            Self::Tokens => crate::core::tokens::count_tokens(text),
        }
    }
}

impl fmt::Display for Unit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Chars => "chars",
            Self::Tokens => "tokens",
        })
    }
}

/// Parse a limit such as `128000`, `128k`, `2M` or `1.5m`.
///
/// Suffixes are decimal (`k` = 1 000, `m` = 1 000 000) because chat products
/// advertise their limits that way. Zero and values that overflow are refused.
pub(crate) fn parse_limit(raw: &str) -> Result<usize, String> {
    let trimmed = raw.trim().replace('_', "");
    let (number, multiplier) = match trimmed.char_indices().last() {
        Some((idx, 'k' | 'K')) => (&trimmed[..idx], 1_000.0),
        Some((idx, 'm' | 'M')) => (&trimmed[..idx], 1_000_000.0),
        _ => (trimmed.as_str(), 1.0),
    };
    let value: f64 = number
        .parse()
        .map_err(|_| format!("invalid limit '{raw}' (examples: 128000, 128k, 2M)"))?;
    let scaled = value * multiplier;
    // NaN and infinities fall outside the range too.
    if !(1.0..=1e12).contains(&scaled) {
        return Err(format!("limit '{raw}' is out of range"));
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Ok(scaled.round() as usize)
}

#[cfg(test)]
mod tests {
    use super::{Unit, parse_limit};

    #[test]
    fn limits_accept_plain_and_suffixed_forms() {
        assert_eq!(parse_limit("128000"), Ok(128_000));
        assert_eq!(parse_limit("128k"), Ok(128_000));
        assert_eq!(parse_limit("128K"), Ok(128_000));
        assert_eq!(parse_limit("2M"), Ok(2_000_000));
        assert_eq!(parse_limit("1.5m"), Ok(1_500_000));
        assert_eq!(parse_limit("100_000"), Ok(100_000));
    }

    #[test]
    fn limits_refuse_zero_garbage_and_overflow() {
        assert!(parse_limit("0").is_err());
        assert!(parse_limit("-5k").is_err());
        assert!(parse_limit("lots").is_err());
        assert!(parse_limit("k").is_err());
        assert!(parse_limit("1e20").is_err());
    }

    #[test]
    fn chars_count_scalar_values_not_bytes() {
        assert_eq!(Unit::Chars.measure("héllo"), 5);
        assert_eq!(Unit::Chars.measure("日本"), 2);
    }

    #[test]
    fn unit_parse_is_strict() {
        assert_eq!(Unit::parse("Tokens"), Ok(Unit::Tokens));
        assert_eq!(Unit::parse("chars"), Ok(Unit::Chars));
        assert!(Unit::parse("bytes").is_err());
    }
}
