// SPDX-License-Identifier: Apache-2.0
//! Source-text decoding shared by every reader that feeds the indexes.
//!
//! `std::fs::read_to_string` rejects anything that is not strict UTF-8, and the
//! index builders used to `continue` past that error without a trace. On
//! Windows that silently dropped whole projects: editors there still save
//! "ANSI" (Windows-1252) files, and Visual Studio / PowerShell write UTF-16
//! with a byte-order mark. Such files were missing from BM25, the dense index,
//! the graph and `ctx_search`, so `ctx_compose` answered "no match" for code
//! that was plainly on disk.
//!
//! [`decode`] never fails:
//! - a UTF-8 BOM is stripped (it is an encoding artifact, not content);
//! - a UTF-16 LE/BE BOM selects UTF-16;
//! - valid UTF-8 passes through without copying;
//! - otherwise the bytes are mostly UTF-8 with a few corrupt sequences (decoded
//!   lossily) or a legacy single-byte file (decoded as Windows-1252, a superset
//!   of Latin-1 that maps every byte, so line numbers stay exact).

use std::path::Path;

/// Windows-1252 code points for bytes 0x80..=0x9F. The five bytes the code
/// page leaves undefined map to the matching C1 control, as WHATWG does.
const CP1252_HIGH: [char; 32] = [
    '\u{20AC}', '\u{0081}', '\u{201A}', '\u{0192}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{02C6}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{008D}', '\u{017D}', '\u{008F}',
    '\u{0090}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{02DC}', '\u{2122}', '\u{0161}', '\u{203A}', '\u{0153}', '\u{009D}', '\u{017E}', '\u{0178}',
];

const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];
const UTF16_LE_BOM: &[u8] = &[0xFF, 0xFE];
const UTF16_BE_BOM: &[u8] = &[0xFE, 0xFF];
/// Prefix inspected for NUL bytes when deciding "binary".
const SNIFF_BYTES: usize = 8192;

/// Whether `bytes` start with a UTF-16 byte-order mark. UTF-16 text is full of
/// NUL bytes, so binary sniffers must ask this before rejecting a file.
#[must_use]
pub(crate) fn has_utf16_bom(bytes: &[u8]) -> bool {
    bytes.starts_with(UTF16_LE_BOM) || bytes.starts_with(UTF16_BE_BOM)
}

/// Read a source file as text, whatever its encoding (see module docs).
/// Replaces `std::fs::read_to_string` in index readers: an encoding is never an
/// error any more, but binary content (a NUL byte in the first 8 KiB of a file
/// without a UTF-16 BOM, the same test `ctx_read` applies) still is, so a
/// binary with an unknown extension is not searched as mojibake.
pub(crate) fn read_text(path: impl AsRef<Path>) -> std::io::Result<String> {
    let bytes = std::fs::read(path.as_ref())?;
    if looks_binary(&bytes) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "binary content",
        ));
    }
    Ok(decode(bytes))
}

/// [`read_text`] with the decoder's outcome: the text, the encoding it was
/// decoded as, and whether any byte could not be represented. Readers that
/// feed admission, caches or indexes use this form — a `String` alone cannot
/// say that the text is not the whole source.
pub(crate) fn read_text_checked(path: impl AsRef<Path>) -> std::io::Result<Decoded> {
    let bytes = std::fs::read(path.as_ref())?;
    if looks_binary(&bytes) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "binary content",
        ));
    }
    Ok(decode_checked(bytes))
}

/// What a decode could not represent faithfully.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DecodeLoss {
    /// Invalid UTF-8 sequences were replaced by U+FFFD.
    InvalidUtf8,
    /// A dangling odd byte at the end of UTF-16 text was dropped.
    Utf16OddTail,
    /// An unpaired UTF-16 surrogate was replaced by U+FFFD.
    Utf16UnpairedSurrogate,
}

/// The one decoder outcome every decoding path produces.
///
/// `loss == None` means every input byte is represented in `text`: valid
/// UTF-8 (with or without BOM), well-formed UTF-16, and Windows-1252 — a
/// total mapping of all 256 byte values, so nothing is dropped or replaced
/// (the encoding is a guess, the decoding is lossless). Anything else is not
/// a complete source: it may be delivered as the mode defines, but it is
/// never complete for admission and never storable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Decoded {
    pub(crate) text: String,
    pub(crate) encoding: Encoding,
    pub(crate) loss: Option<DecodeLoss>,
}

impl Decoded {
    #[must_use]
    pub(crate) fn complete(&self) -> bool {
        self.loss.is_none()
    }
}

/// Decode file bytes to text and report what was lost; never fails.
#[must_use]
pub(crate) fn decode_checked(mut bytes: Vec<u8>) -> Decoded {
    if bytes.starts_with(UTF16_LE_BOM) {
        return decode_utf16(&bytes[2..], u16::from_le_bytes, Encoding::Utf16Le);
    }
    if bytes.starts_with(UTF16_BE_BOM) {
        return decode_utf16(&bytes[2..], u16::from_be_bytes, Encoding::Utf16Be);
    }
    let bom = bytes.starts_with(UTF8_BOM);
    if bom {
        bytes.drain(..UTF8_BOM.len());
    }
    match String::from_utf8(bytes) {
        Ok(text) => Decoded {
            text,
            encoding: if bom {
                Encoding::Utf8Bom
            } else {
                Encoding::Utf8
            },
            loss: None,
        },
        Err(err) => {
            let bytes = err.into_bytes();
            if is_mostly_utf8(&bytes) {
                Decoded {
                    text: String::from_utf8_lossy(&bytes).into_owned(),
                    encoding: Encoding::Utf8Lossy,
                    loss: Some(DecodeLoss::InvalidUtf8),
                }
            } else {
                Decoded {
                    text: decode_cp1252(&bytes),
                    encoding: Encoding::Windows1252,
                    loss: None,
                }
            }
        }
    }
}

/// NUL bytes mark binary data — except in UTF-16, where they are half the text.
#[must_use]
pub(crate) fn looks_binary(bytes: &[u8]) -> bool {
    !has_utf16_bom(bytes) && bytes.iter().take(SNIFF_BYTES).any(|&b| b == 0)
}

/// Decode file bytes to text; never fails (see module docs).
#[must_use]
pub(crate) fn decode(bytes: Vec<u8>) -> String {
    decode_checked(bytes).text
}

/// The encoding [`decode`] picks for a byte sequence, for diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Encoding {
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
    /// Mostly UTF-8 with corrupt sequences, which decode to U+FFFD.
    Utf8Lossy,
    Windows1252,
}

impl Encoding {
    #[must_use]
    pub(crate) fn label(self) -> &'static str {
        match self {
            Encoding::Utf8 => "utf-8",
            Encoding::Utf8Bom => "utf-8 with BOM",
            Encoding::Utf16Le => "utf-16le (BOM)",
            Encoding::Utf16Be => "utf-16be (BOM)",
            Encoding::Utf8Lossy => "utf-8 with invalid bytes (replaced by U+FFFD)",
            Encoding::Windows1252 => "windows-1252 (legacy ANSI)",
        }
    }
}

/// Which encoding [`decode`] uses for `bytes`. Kept out of `decode` itself so
/// the hot path validates UTF-8 only once.
#[must_use]
pub(crate) fn detect_encoding(bytes: &[u8]) -> Encoding {
    if bytes.starts_with(UTF16_LE_BOM) {
        Encoding::Utf16Le
    } else if bytes.starts_with(UTF16_BE_BOM) {
        Encoding::Utf16Be
    } else if let Some(body) = bytes.strip_prefix(UTF8_BOM) {
        match detect_single_byte_or_utf8(body) {
            Encoding::Utf8 => Encoding::Utf8Bom,
            other => other,
        }
    } else {
        detect_single_byte_or_utf8(bytes)
    }
}

/// The BOM-less tail of [`decode`]: UTF-8, lossy UTF-8 or Windows-1252.
fn detect_single_byte_or_utf8(bytes: &[u8]) -> Encoding {
    if std::str::from_utf8(bytes).is_ok() {
        Encoding::Utf8
    } else if is_mostly_utf8(bytes) {
        Encoding::Utf8Lossy
    } else {
        Encoding::Windows1252
    }
}

fn decode_utf16(body: &[u8], unit: fn([u8; 2]) -> u16, encoding: Encoding) -> Decoded {
    // A dangling odd byte cannot form a code unit; drop it rather than fail —
    // and say so: the text is then not the whole source.
    let (pairs, tail) = body.as_chunks::<2>();
    let mut loss = (!tail.is_empty()).then_some(DecodeLoss::Utf16OddTail);
    let units = pairs.iter().map(|&pair| unit(pair));
    let text = char::decode_utf16(units)
        .map(|unit| {
            unit.unwrap_or_else(|_| {
                loss = Some(DecodeLoss::Utf16UnpairedSurrogate);
                char::REPLACEMENT_CHARACTER
            })
        })
        .collect();
    Decoded {
        text,
        encoding,
        loss,
    }
}

/// A UTF-8 file with a stray corrupt byte still has far more valid multi-byte
/// sequences than invalid ones; a Windows-1252 file with umlauts has none (its
/// high bytes almost never form valid UTF-8 sequences).
fn is_mostly_utf8(bytes: &[u8]) -> bool {
    let mut multibyte = 0usize;
    let mut invalid = 0usize;
    for chunk in bytes.utf8_chunks() {
        multibyte += chunk.valid().chars().filter(|c| c.len_utf8() > 1).count();
        if !chunk.invalid().is_empty() {
            invalid += 1;
        }
    }
    multibyte > invalid
}

fn decode_cp1252(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| match b {
            0x80..=0x9F => CP1252_HIGH[usize::from(b - 0x80)],
            _ => char::from(b),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Before, the decoder returned only a `String`: a dropped UTF-16 byte, a
    /// replaced surrogate and replaced UTF-8 sequences all looked complete.
    #[test]
    fn decoder_reports_every_loss_and_keeps_faithful_encodings_complete() {
        let utf16 = |bom: &[u8], units: &[u16], be: bool, tail: &[u8]| {
            let mut bytes = bom.to_vec();
            for unit in units {
                bytes.extend_from_slice(&if be {
                    unit.to_be_bytes()
                } else {
                    unit.to_le_bytes()
                });
            }
            bytes.extend_from_slice(tail);
            decode_checked(bytes)
        };
        let hi: Vec<u16> = "hi\n".encode_utf16().collect();
        for (bom, be) in [(UTF16_LE_BOM, false), (UTF16_BE_BOM, true)] {
            let clean = utf16(bom, &hi, be, &[]);
            assert_eq!((clean.text.as_str(), clean.loss), ("hi\n", None));
            assert_eq!(
                utf16(bom, &hi, be, &[0x41]).loss,
                Some(DecodeLoss::Utf16OddTail)
            );
            let unpaired = utf16(bom, &[0x0068, 0xD800, 0x0069], be, &[]);
            assert_eq!(unpaired.loss, Some(DecodeLoss::Utf16UnpairedSurrogate));
            assert!(!unpaired.complete());
        }

        let mut stray = "fn grüezi() { let ä = \"ö\"; }\n".as_bytes().to_vec();
        stray.push(0xFF);
        let lossy = decode_checked(stray);
        assert_eq!(lossy.encoding, Encoding::Utf8Lossy);
        assert_eq!(lossy.loss, Some(DecodeLoss::InvalidUtf8));

        // Controls: nothing is lost, text and encoding are as before.
        let ansi = decode_checked(b"// Gr\xF6\xDFe\r\n".to_vec());
        assert_eq!((ansi.encoding, ansi.loss), (Encoding::Windows1252, None));
        assert_eq!(ansi.text, "// Größe\r\n");
        let mut bom = UTF8_BOM.to_vec();
        bom.extend_from_slice(b"class A {}\n");
        let with_bom = decode_checked(bom);
        assert_eq!(
            (with_bom.encoding, with_bom.loss),
            (Encoding::Utf8Bom, None)
        );
        assert!(decode_checked(b"plain\n".to_vec()).complete());
    }

    #[test]
    fn utf8_passes_through_and_loses_its_bom() {
        assert_eq!(decode("fn grüezi() {}\n".into()), "fn grüezi() {}\n");
        let mut bom = UTF8_BOM.to_vec();
        bom.extend_from_slice(b"class A {}\n");
        assert_eq!(decode(bom), "class A {}\n");
    }

    #[test]
    fn windows_1252_umlauts_and_punctuation_decode() {
        // "// Größe – “Wert” €" as saved by a Windows ANSI editor.
        let ansi = b"// Gr\xF6\xDFe \x96 \x93Wert\x94 \x80\r\nint x;\r\n".to_vec();
        let text = decode(ansi);
        assert_eq!(text, "// Größe – “Wert” €\r\nint x;\r\n");
        assert_eq!(text.lines().count(), 2, "line structure must be preserved");
    }

    #[test]
    fn every_cp1252_byte_maps_to_one_char() {
        let all: Vec<u8> = (0x80..=0xFF).collect();
        assert_eq!(decode_cp1252(&all).chars().count(), all.len());
    }

    #[test]
    fn utf16_with_bom_decodes_in_both_byte_orders() {
        let text = "public class Größe {}\r\n";
        let mut le = UTF16_LE_BOM.to_vec();
        let mut be = UTF16_BE_BOM.to_vec();
        for unit in text.encode_utf16() {
            le.extend_from_slice(&unit.to_le_bytes());
            be.extend_from_slice(&unit.to_be_bytes());
        }
        assert!(has_utf16_bom(&le) && has_utf16_bom(&be));
        assert_eq!(decode(le.clone()), text);
        assert_eq!(decode(be), text);
        le.push(b'x'); // odd trailing byte
        assert_eq!(decode(le), text);
    }

    #[test]
    fn mostly_utf8_with_a_corrupt_byte_stays_utf8() {
        let mut bytes = "// äöü éè — fine\n".as_bytes().to_vec();
        bytes.push(0xFF);
        bytes.extend_from_slice(b"\nint y;\n");
        let text = decode(bytes);
        assert!(text.starts_with("// äöü éè — fine\n"), "{text}");
        assert!(text.contains('\u{FFFD}'));
    }

    #[test]
    fn detect_encoding_names_the_branch_decode_takes() {
        assert_eq!(detect_encoding(b"plain"), Encoding::Utf8);
        assert_eq!(detect_encoding(b"\xEF\xBB\xBFx"), Encoding::Utf8Bom);
        assert_eq!(detect_encoding(b"\xFF\xFEx\x00"), Encoding::Utf16Le);
        assert_eq!(detect_encoding(b"\xFE\xFF\x00x"), Encoding::Utf16Be);
        assert_eq!(detect_encoding(b"Gr\xF6\xDFe"), Encoding::Windows1252);
        let mut lossy = "äöü".as_bytes().to_vec();
        lossy.push(0xFF);
        assert_eq!(detect_encoding(&lossy), Encoding::Utf8Lossy);
        // A UTF-8 BOM followed by UTF-16 BOM bytes is not UTF-16: `decode`
        // checks the UTF-16 BOM on the raw bytes only.
        assert_eq!(
            detect_encoding(b"\xEF\xBB\xBF\xFF\xFE"),
            Encoding::Windows1252
        );
        assert_eq!(decode(b"\xEF\xBB\xBF\xFF\xFE".to_vec()), "\u{FF}\u{FE}");
    }

    #[test]
    fn read_text_reads_files_read_to_string_rejects() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.cs");
        std::fs::write(&path, b"// K\xFCndigung\nclass K {}\n").unwrap();
        assert!(std::fs::read_to_string(&path).is_err());
        assert_eq!(read_text(&path).unwrap(), "// Kündigung\nclass K {}\n");
    }
}
