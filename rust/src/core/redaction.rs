macro_rules! static_regex {
    ($pattern:expr_2021) => {{
        static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        RE.get_or_init(|| {
            regex::Regex::new($pattern).expect(concat!("BUG: invalid static regex: ", $pattern))
        })
    }};
}

pub fn redaction_enabled_for_active_role() -> bool {
    let role = crate::core::roles::active_role();
    if role.role.name == "admin" {
        role.io.redact_outputs
    } else {
        // Contract: redaction never disabled for non-admin roles.
        true
    }
}

/// Redacts text bound for the model (tool output). Only this entry point
/// counts toward the value surface: persistence passes (`redact_text`) keep
/// secrets out of files, not out of the model context, and an already
/// redacted `[REDACTED:…]` marker never matches again, so nested passes over
/// one output do not double count.
pub fn redact_text_if_enabled(input: &str) -> String {
    if !redaction_enabled_for_active_role() {
        return input.to_string();
    }
    let (out, found) = redact_found(input, config_exclude_patterns().as_slice());
    let hits = found.len();
    // Engine metrics K21: fingerprints only, and only while sharing is on.
    if hits > 0 {
        crate::core::hive_signals::note_secrets(&found);
    }
    crate::core::security_events::note(
        crate::core::security_events::SecurityKind::SecretRedacted,
        hits,
    );
    out
}

/// #718: unquoted identifier or property-access chains (`SvelteKit`,
/// `inputEnv.POCKETBASE_SUPERUSER_PASSWORD`, `serverEnv.getStripeSecretKey`,
/// `confirmRequiredEndpointKeySchema`) are code REFERENCES to a secret, never
/// the literal value — the value lives in a gitignored `.env`. Digits make a
/// token secret-shaped (base64/hex), so any digit keeps the redaction
/// (conservative: `password=hunter2` stays covered).
/// #827: detect pure numeric values — integers, floats, scientific notation.
/// These are never secrets even when the key contains `token` or `key`.
fn looks_like_number(v: &str) -> bool {
    if v.is_empty() {
        return false;
    }
    let s = v.trim_start_matches(['+', '-']);
    if s.is_empty() {
        return false;
    }
    // Integer, float, or scientific notation (1.4e-06, 0.5, 600, 1e10)
    s.parse::<f64>().is_ok()
        && s.chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == 'e' || c == 'E' || c == '+' || c == '-')
}

fn looks_like_uuid(value: &str) -> bool {
    let value = value
        .trim()
        .trim_matches(|c| c == '"' || c == '\'' || c == '`' || c == '{' || c == '}');
    let groups: Vec<&str> = value.split('-').collect();
    groups.len() == 5
        && [8, 4, 4, 4, 12].iter().zip(&groups).all(|(len, group)| {
            group.len() == *len && group.chars().all(|ch| ch.is_ascii_hexdigit())
        })
}

/// #827: env-variable reference patterns that should not be redacted.
/// Matches: `os.environ/NAME`, `os.getenv("NAME")`, `process.env.NAME`,
/// `inputEnv.NAME`, `${NAME}`, `$NAME`, `%NAME%` (Windows).
fn is_env_reference(v: &str) -> bool {
    if v.starts_with("os.environ/")
        || v.starts_with("os.getenv(")
        || v.starts_with("process.env.")
        || v.starts_with("System.getenv(")
        || v.starts_with("ENV[")
        || v.starts_with("env(")
    {
        return true;
    }
    // Variable interpolation: ${VAR}, $VAR, %VAR%
    if (v.starts_with("${") && v.ends_with('}'))
        || (v.starts_with('$')
            && v[1..]
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_'))
        || (v.starts_with('%') && v.ends_with('%') && v.len() > 2)
    {
        return true;
    }
    // Dotted identifier chains with an env-like prefix
    if let Some(prefix) = v.split('.').next() {
        let pl = prefix.to_ascii_lowercase();
        if matches!(
            pl.as_str(),
            "env" | "inputenv" | "serverenv" | "secrets" | "vars" | "environ"
        ) && v.contains('.')
        {
            return true;
        }
    }
    false
}

fn is_identifier_reference(value: &str) -> bool {
    let v = value.trim();
    if v.is_empty() || v.starts_with('"') || v.starts_with('\'') || v.starts_with('`') {
        return false;
    }
    // #827: env-variable reference patterns are not secrets.
    // `os.environ/MY_SERVICE_API_KEY`, `process.env.NAME`, `${VAR}`, `$VAR`.
    if is_env_reference(v) {
        return true;
    }
    if v.contains(|c: char| c.is_ascii_digit()) {
        return false;
    }
    v.split('.').all(|segment| {
        let mut chars = segment.chars();
        matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_' || c == '$')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
    })
}

/// #718: obvious provider-token placeholders, `your_key_here`,
/// `<insert-token>`) are documentation, not secrets — `.env.example` files
/// must survive ctx_read verbatim. #1831: so is an asterisk mask (`*****`),
/// which is what a redactor itself writes in place of a secret.
fn is_placeholder_value(value: &str) -> bool {
    let v = value
        .trim()
        .trim_matches(|c| c == '"' || c == '\'' || c == '`')
        .to_ascii_lowercase();
    if v.starts_with('<') && v.ends_with('>') {
        return true;
    }
    if !v.is_empty() && v.chars().all(|c| c == '*') {
        return true;
    }
    const MARKERS: &[&str] = &[
        "change_me",
        "change-me",
        "changeme",
        "example",
        "placeholder",
        "your_",
        "your-",
        "xxx",
        "dummy",
        "sample",
        "todo",
        "fixme",
        "replace_me",
        "replace-me",
        "redacted",
        "...",
        "…",
    ];
    MARKERS.iter().any(|m| v.contains(m))
}

/// Right-hand sides that look like `key: value` but are obviously not secrets:
/// TypeScript type annotations and language literals. Redacting these corrupts
/// source files read through `ctx_read` (GH #430), so the key/value rules skip
/// them. Compared case-insensitively after trimming surrounding quotes.
fn is_non_secret_literal(value: &str) -> bool {
    let v = value
        .trim()
        .trim_matches(|c| c == '"' || c == '\'' || c == '`');
    // #827: pure numbers (integer, float, scientific notation) are never secrets.
    // `input_cost_per_token: 1.4e-06` must not be redacted.
    if looks_like_number(v) {
        return true;
    }
    // Type expressions are never flat secret tokens: real keys/tokens are drawn
    // from `[A-Za-z0-9+/=_-]`, whereas type annotations carry angle brackets,
    // unions, arrays or call/object syntax. `password: Promise<string>` and
    // `apiKey: Record<string, unknown>` must survive ctx_read verbatim (GH #430).
    if v.contains(['<', '>', '|', '(', ')', '[', ']', '{', '}']) {
        return true;
    }
    matches!(
        v.to_ascii_lowercase().as_str(),
        "" | "undefined"
            | "null"
            | "none"
            | "nil"
            | "true"
            | "false"
            | "string"
            | "number"
            | "boolean"
            | "bigint"
            | "symbol"
            | "object"
            | "any"
            | "unknown"
            | "never"
            | "void"
            | "nan"
            | "date"
    ) || is_primitive_type_name(v)
}

/// Primitive type names of Rust, Go, Java and C-family languages. In
/// `const MAX_TOKEN: usize = 4` the "value" after `TOKEN:` is a type
/// annotation, never a credential; masking it corrupts every read of such a
/// source. Matched exactly (case-sensitive), so a password that merely
/// contains one of these words is still protected.
fn is_primitive_type_name(value: &str) -> bool {
    matches!(
        value,
        "bool"
            | "char"
            | "str"
            | "usize"
            | "isize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "f32"
            | "f64"
            | "int"
            | "int8"
            | "int16"
            | "int32"
            | "int64"
            | "uint"
            | "uint8"
            | "uint16"
            | "uint32"
            | "uint64"
            | "uintptr"
            | "float"
            | "float32"
            | "float64"
            | "double"
            | "long"
            | "short"
            | "byte"
            | "rune"
            | "error"
            | "Self"
    )
}

/// One redaction rule: a labelled regex plus how the match is rebuilt.
struct Rule {
    label: &'static str,
    re: &'static regex::Regex,
    /// When set, group 1 is a prefix to keep and group 2 is the secret value;
    /// the match is left untouched if that value is a non-secret literal
    /// (`password: undefined`), an identifier reference
    /// (`serverEnv.getStripeSecretKey`) or a provider-token placeholder —
    /// see `is_benign_secret_value` (GH #430, #718).
    guard_value: bool,
}

/// Normalization used only for matching: remove invisible zero-width format
/// characters and fold full-width ASCII plus ideographic space to their ASCII
/// forms. Original text and byte boundaries are retained for span-only edits.
fn normalize_for_secret_matching(input: &str) -> (String, Vec<usize>) {
    let mut normalized = String::with_capacity(input.len());
    let mut original_boundaries = vec![0usize];

    for (start, ch) in input.char_indices() {
        let end = start + ch.len_utf8();
        let Some(folded) = fold_secret_match_char(ch) else {
            if let Some(boundary) = original_boundaries.last_mut() {
                *boundary = end;
            }
            continue;
        };
        let folded_len = folded.len_utf8();
        normalized.push(folded);
        for byte in 1..=folded_len {
            original_boundaries.push(if byte == folded_len { end } else { start });
        }
    }

    (normalized, original_boundaries)
}

fn fold_secret_match_char(ch: char) -> Option<char> {
    if matches!(
        ch,
        '\u{200b}' | '\u{200c}' | '\u{200d}' | '\u{2060}' | '\u{feff}'
    ) {
        return None;
    }
    Some(match ch {
        '\u{3000}' => ' ',
        '\u{ff01}'..='\u{ff5e}' => char::from_u32(ch as u32 - 0xfee0).unwrap_or(ch),
        _ => ch,
    })
}

/// A normalized view for detection-only callers that do not need span offsets.
pub(crate) fn normalize_secret_text(input: &str) -> String {
    input.chars().filter_map(fold_secret_match_char).collect()
}

/// The markers of the "Private key block" rule, as that rule spells them
/// (only a PRIVATE KEY END closes a block: a certificate's END does not).
const PRIVATE_KEY_KINDS: &str = r"(?:RSA\s+|EC\s+|DSA\s+|OPENSSH\s+)?PRIVATE\s+KEY-----";

fn private_key_marker(end: bool) -> &'static regex::Regex {
    static HEADER: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static END: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let (cell, word) = if end {
        (&END, "END")
    } else {
        (&HEADER, "BEGIN")
    };
    cell.get_or_init(|| {
        regex::Regex::new(&format!(r"-----{word}\s+{PRIVATE_KEY_KINDS}"))
            .expect("BUG: invalid private-key marker")
    })
}

/// Whether `text`, normalized as the "Private key block" rule sees it,
/// holds a private-key header with no private-key END after it: the rule's
/// match would continue in the text that follows.
pub fn ends_inside_private_key_block(text: &str) -> bool {
    let normalized = normalize_secret_text(text);
    let last = |end: bool| {
        private_key_marker(end)
            .find_iter(&normalized)
            .last()
            .map(|found| found.start())
    };
    match (last(false), last(true)) {
        (Some(header), Some(end)) => header > end,
        (Some(_), None) => true,
        (None, _) => false,
    }
}

/// Whether `text`, normalized as the "Private key block" rule sees it,
/// holds a private-key END marker (the one the rule closes a block on).
pub fn contains_private_key_end(text: &str) -> bool {
    private_key_marker(true).is_match(&normalize_secret_text(text))
}

/// Whether the word after a bare `Bearer` is a credential rather than prose or a
/// placeholder. Precision matters on the default redaction path ("Bearer
/// authentication", "Bearer YOUR_TOKEN_HERE" must survive), but the identifier
/// heuristic cannot be used: an opaque base64 token can be letters only. A
/// credential is long and mixes character classes.
pub(crate) fn bearer_value_is_credential(value: &str) -> bool {
    let classes = [
        value.bytes().any(|b| b.is_ascii_uppercase()),
        value.bytes().any(|b| b.is_ascii_lowercase()),
        value.bytes().any(|b| b.is_ascii_digit()),
    ];
    value.len() >= 16
        && classes.iter().filter(|present| **present).count() >= 2
        && !is_placeholder_value(value)
}

/// Combined benign-value check for key/value secret rules (#430 + #718):
/// language literals and type annotations, unquoted identifier/property
/// references, and documentation placeholders are never redacted.
///
/// The secret detector (`core::secret_detection`) and the redactor capture
/// different spans of one expression: the redactor's value runs to the next
/// blank, so it can carry the rest of a comparison operator (`=<x>` from
/// `==<x>`) and trailing syntax (`<x>)`, `<x>:`), while the detector's value
/// stops at the first character outside `[A-Za-z0-9_+/=-]`. Both layers judge
/// through this function, which reduces either span to the same operand, so
/// they cannot disagree about one value (GHSA-xmx8-69qv-7f86): a value one
/// layer reports is never left unchanged by the other.
pub(crate) fn is_benign_secret_value(value: &str) -> bool {
    secret_value_span(value, false).is_none()
}

/// Benign-value check for a *quoted* value. A quoted string is a literal, never
/// an identifier reference, so that heuristic is skipped (`API_KEY="<letters>"`
/// is a credential). Literals, placeholders, UUIDs and plain numbers — digests,
/// counters, ids in JSON — stay benign.
pub(crate) fn is_benign_quoted_value(value: &str) -> bool {
    secret_value_span(value, true).is_none()
}

/// The part of a guarded rule's value that is redacted: `None` when the value
/// is benign, otherwise a byte range of `value`. A value that is a secret as a
/// whole is redacted whole. A value that only reads as benign because of the
/// text after the detector's run (`<run>(arg)`, `<run>|next`) has just that
/// run redacted, so the syntax around it survives.
pub(crate) fn secret_value_span(value: &str, quoted: bool) -> Option<std::ops::Range<usize>> {
    let canonical = canonical_secret_value(value);
    let operand = if quoted {
        canonical
    } else {
        if is_comparison_operator_residue(canonical) {
            return None;
        }
        // The operand of `==<x>`, `:=<x>` or `===<x>` is judged exactly like
        // the value of `key=<x>`.
        let operand = canonical.trim_start_matches(['=', '!', ':']);
        if operand.is_empty() {
            return None;
        }
        operand
    };
    let benign = |v: &str| {
        if quoted {
            is_comparison_operator_residue(v)
                || is_non_secret_literal(v)
                || is_placeholder_value(v)
                || looks_like_uuid(v)
                || (!v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
        } else {
            is_non_secret_literal(v)
                || is_identifier_reference(v)
                || is_placeholder_value(v)
                || looks_like_uuid(v)
        }
    };
    if !benign(operand) {
        return Some(0..value.len());
    }
    let run = detector_run_len(operand);
    if run == operand.len() || run < 20 || benign(&operand[..run]) {
        return None;
    }
    let start = (value.len() - value.trim_start().len()) + (canonical.len() - operand.len());
    Some(start..start + run)
}

/// The value without the syntax that follows it in an expression: a trailing
/// `:` and every unbalanced closing `)`, `]` or `}` (`if (key==<x>):`). Only
/// the judgement uses this text; see `secret_value_span` for what is redacted.
fn canonical_secret_value(value: &str) -> &str {
    let mut v = value.trim();
    loop {
        if let Some(rest) = v.strip_suffix(':') {
            v = rest;
            continue;
        }
        let open = match v.chars().last() {
            Some(')') => '(',
            Some(']') => '[',
            Some('}') => '{',
            _ => break,
        };
        let close = v.chars().last().unwrap_or(')');
        if v.matches(open).count() >= v.matches(close).count() {
            break;
        }
        v = &v[..v.len() - 1];
    }
    v
}

/// The detector reports the longest leading run of `[A-Za-z0-9_+/=-]` (20 or
/// more characters) of a value. When the rest of the value makes it look
/// benign (`<run>(arg)` reads as a call or a type), that run is judged on its
/// own as well, so the redactor never keeps a run the detector reports.
fn detector_run_len(value: &str) -> usize {
    value
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '+' | '/' | '=' | '-')))
        .unwrap_or(value.len())
}

/// Whether a "Generic long secret" match is the slug of a knowledge fact's
/// own key: the value of a JSON member named exactly `key` (`"key":"…"` or
/// `"key": "…"`, the quoted form a serialized fact carries) that reads as
/// words. The rule fires on any 32+ character value after a name ending in
/// `key`, so every long fact key was masked — all to the same marker, which
/// merged distinct facts on export and import and broke the ids readers
/// derive from keys.
///
/// Nothing else is exempt: free text (`key: …`, `SIGNING_KEY=…`), any other
/// member name (`encryption_key`, `auth`, `credential`), and a value that
/// does not read as words stay masked. `prefix` is the rule's kept group
/// (the character before the name, the name, the separator).
///
/// Known residual risk: a passphrase made of real words joined by `-`
/// (diceware) stored as the value of a member named `key` reads as a slug
/// and is kept. A secret belongs under a secret's name, where it is masked.
pub(crate) fn is_fact_key_slug(prefix: &str, value: &str) -> bool {
    matches!(prefix, "\"key\":\"" | "\"key\": \"") && looks_like_word_slug(value)
}

/// Lowercase words and short numbers joined by `-`
/// (`governor-design-pull-forward-2026-10-29`): at least three parts, at
/// least two of them words, and at least half of the letter parts words; a
/// number has at most 8 digits (dates, years, counters — not a PIN or an
/// account number). A word has a vowel in at least a fifth of its letters
/// and no run of more than four consonants, or is a one-vowel word of five
/// to nine letters whose consonants before and after the vowel are a common
/// English onset and coda (`rights`, `scripts`, `strict`). A part that is
/// neither must be one of a fixed list of abbreviations (`mr`, `mcp`), which
/// do not count as words. Random letter groups (`qhztvbkr`, `xkq`) fail.
/// Any part with a digit beside letters, an upper-case letter or another
/// character fails the slug.
fn looks_like_word_slug(value: &str) -> bool {
    let parts: Vec<&str> = value.split('-').collect();
    let mut words = 0usize;
    let mut letter_parts = 0usize;
    let all_parts_ok = parts.iter().all(|part| {
        if !part.is_empty() && part.len() <= 8 && part.bytes().all(|b| b.is_ascii_digit()) {
            return true;
        }
        if part.len() < 2 || part.len() > 24 || !part.bytes().all(|b| b.is_ascii_lowercase()) {
            return false;
        }
        letter_parts += 1;
        let word = is_slug_word(part);
        words += usize::from(word);
        word || SLUG_ABBREVIATIONS.contains(part)
    });
    parts.len() >= 3 && all_parts_ok && words >= 2 && words * 2 >= letter_parts
}

/// Vowel-free abbreviations a fact key may carry. A fixed list: a rule for
/// "short and vowel-free" let random letter groups pass as abbreviations.
const SLUG_ABBREVIATIONS: &[&str] = &[
    "cd", "cdn", "cfg", "cmd", "csv", "ctx", "db", "dns", "gc", "grpc", "http", "js", "jwt", "mcp",
    "md", "ml", "mr", "msg", "npm", "pdf", "pkg", "png", "pr", "rs", "sql", "src", "ssh", "svg",
    "tls", "tmp", "ts", "ttl", "xml",
];

/// Consonants before the single vowel of a one-vowel word.
const SLUG_ONSETS: &[&str] = &[
    "", "b", "c", "d", "f", "g", "h", "j", "k", "l", "m", "n", "p", "q", "r", "s", "t", "v", "w",
    "x", "z", "bl", "br", "ch", "cl", "cr", "dr", "fl", "fr", "gl", "gr", "ph", "pl", "pr", "sc",
    "sh", "sk", "sl", "sm", "sn", "sp", "st", "sw", "th", "tr", "tw", "wh", "wr", "chr", "sch",
    "scr", "shr", "spl", "spr", "str", "thr",
];

/// Consonants after the single vowel of a one-vowel word.
const SLUG_CODAS: &[&str] = &[
    "bs", "ch", "ck", "cks", "ct", "cts", "ds", "ft", "fts", "ght", "ghts", "gs", "ks", "ld",
    "lds", "lf", "lk", "lks", "ll", "lls", "lm", "lp", "ls", "lt", "lts", "mp", "mps", "ms", "nch",
    "nd", "nds", "ng", "ngs", "ngth", "ngths", "nk", "nks", "ns", "nt", "nth", "nths", "nts", "ps",
    "pt", "pts", "rch", "rd", "rds", "rk", "rks", "rm", "rms", "rn", "rns", "rst", "rt", "rth",
    "rts", "sh", "sk", "sks", "sp", "ss", "st", "sts", "tch", "th", "ths", "ts", "ws", "x", "xt",
    "xts",
];

fn is_slug_word(part: &str) -> bool {
    let is_vowel = |b: u8| matches!(b, b'a' | b'e' | b'i' | b'o' | b'u' | b'y');
    let vowels = part.bytes().filter(|b| is_vowel(*b)).count();
    let mut run = 0usize;
    let mut longest_run = 0usize;
    for b in part.bytes() {
        run = if is_vowel(b) { 0 } else { run + 1 };
        longest_run = longest_run.max(run);
    }
    if vowels * 5 >= part.len() && longest_run <= 4 {
        return true;
    }
    if vowels != 1 || !(5..=9).contains(&part.len()) {
        return false;
    }
    let Some(at) = part.bytes().position(is_vowel) else {
        return false;
    };
    SLUG_ONSETS.contains(&&part[..at]) && SLUG_CODAS.contains(&&part[at + 1..])
}

/// #1095: when the regex `key=` consumes one `=` from `==`, the captured
/// "value" is a bare `=` (or `==`, `!=`). A comparison operator is never
/// a secret; redacting it corrupts source semantics.
fn is_comparison_operator_residue(value: &str) -> bool {
    let v = value.trim();
    matches!(v, "=" | "==" | "!=" | "<=" | ">=" | "===" | "!==")
}

/// The single source of truth for secret patterns. `shell::redact` delegates
/// here so the two layers can never drift apart again.
///
/// #718 word boundaries: the key/value alternations start with
/// `(?:^|[^a-z0-9])` (consumed into the kept prefix — the regex crate has no
/// lookbehind) so camelCase subwords (`superuserPassword`,
/// `getStripeSecretKey`) never trigger a rule, while SNAKE_CASE env names
/// (`GITHUB_FEEDBACK_TOKEN`) still do: `_` remains a permitted predecessor.
fn redaction_rules() -> Vec<Rule> {
    vec![
        Rule {
            label: "Bearer token",
            // Not the identifier guard (a letters-only base64 token is still a
            // credential); `bearer_value_is_credential` filters prose instead.
            re: static_regex!(r"(?i)(bearer[ \t]+)([a-zA-Z0-9_\-\.]{8,})"),
            guard_value: false,
        },
        Rule {
            label: "Authorization header",
            // Any token-character credential, however short: `Basic dTpw` (u:p) is
            // four characters. Token characters only, so adjacent text survives.
            re: static_regex!(
                r"(?i)(authorization[ \t]*:[ \t]*(?:basic|bearer|token)[ \t]+)([A-Za-z0-9._~+/=-]+)"
            ),
            guard_value: false,
        },
        // Whole token is the secret — no prefix group, so the entire match is
        // replaced. (Previously group 1 captured the key itself and leaked it.)
        Rule {
            label: "AWS key",
            re: static_regex!(r"(?:AKIA|ASIA)[0-9A-Z]{16}"),
            guard_value: false,
        },
        Rule {
            label: "Private key block",
            // No kept prefix: the whole block, markers included, is replaced.
            // A block without its END marker (a truncated file, a line window,
            // a log tail) is still masked: the header plus every following
            // key-material line — PEM headers of encrypted keys, then pure
            // base64 lines. The first other line (code, prose) ends it, so a
            // source that only mentions the header keeps the rest of the file.
            re: static_regex!(
                r"(?s)-----BEGIN\s+(?:RSA\s+|EC\s+|DSA\s+|OPENSSH\s+)?PRIVATE\s+KEY-----(?:.+?-----END\s+(?:RSA\s+|EC\s+|DSA\s+|OPENSSH\s+)?PRIVATE\s+KEY-----|(?-s:[^\n]*)(?:\r?\n[A-Za-z-]+:(?-s:[^\n]*))*(?:\r?\n[A-Za-z0-9+/=]+[ \t]*(?m:$))*)"
            ),
            guard_value: false,
        },
        Rule {
            label: "GitHub token",
            re: static_regex!(r"(gh[pousr]_)[a-zA-Z0-9_]{20,}"),
            guard_value: false,
        },
        Rule {
            label: "GitHub fine-grained token",
            re: static_regex!(r"(github_pat_)[A-Za-z0-9_]{20,}"),
            guard_value: false,
        },
        Rule {
            label: "GitLab token",
            re: static_regex!(r"(glpat-)[A-Za-z0-9_\-]{20,}"),
            guard_value: false,
        },
        // Provider keys are masked whole — the prefix names the provider and
        // key class, so it is not kept. Same patterns as the detector, which
        // also matches them inside nested values without a `key=` prefix.
        Rule {
            label: "Anthropic API key",
            re: crate::core::secret_detection::anthropic_key_re(),
            guard_value: false,
        },
        Rule {
            label: "OpenAI API key",
            re: crate::core::secret_detection::openai_key_re(),
            guard_value: false,
        },
        Rule {
            label: "JWT",
            re: static_regex!(
                r"eyJ[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}(?:\.[A-Za-z0-9_\-]{8,})?"
            ),
            guard_value: false,
        },
        Rule {
            label: "Slack token",
            re: static_regex!(r"(xox[bpas]-)[0-9a-zA-Z\-]{10,}"),
            guard_value: false,
        },
        Rule {
            label: "Stripe key",
            re: static_regex!(r"([sr]k_live_)[0-9a-zA-Z]{10,}"),
            guard_value: false,
        },
        Rule {
            label: "npm token",
            re: static_regex!(r"(npm_)[A-Za-z0-9]{10,}"),
            guard_value: false,
        },
        // Key/value secrets run after provider-specific forms so `key=<AWS key id>`
        // retains the more precise provider class label.
        Rule {
            label: "API key param",
            re: static_regex!(
                r#"(?im)((?:^|[^a-z0-9])(?:aws[_-]?secret[_-]?access[_-]?key|secret[_-]?access[_-]?key|api[_-]?key|apikey|access[_-]?key|secret[_-]?key|access[_-]?token|client[_-]?secret|token|password|passwd|pwd|secret)(?:\\?["'])?[ \t]*[=:][ \t]*(?:\\?["'])?)([^\s\r\n,;&"']+)"#
            ),
            guard_value: true,
        },
        Rule {
            label: "URL-encoded secret",
            re: static_regex!(
                r"(?im)((?:^|[^a-z0-9])(?:aws[_-]?secret[_-]?access[_-]?key|secret[_-]?access[_-]?key|api[_-]?key|apikey|access[_-]?key|secret[_-]?key|access[_-]?token|client[_-]?secret|token|password|passwd|pwd|secret)[ \t]*[=:][ \t]*)(?:((?:%[0-9a-f]{2}|[A-Za-z0-9_\-]){20,}))"
            ),
            guard_value: true,
        },
        // Group 1 = prefix (kept), group 2 = the 32+ char value. Guarded since
        // #718: 32-char identifiers like `confirmRequiredEndpointKeySchema`
        // are references, not secrets.
        Rule {
            label: "Generic long secret",
            re: static_regex!(
                r#"(?im)((?:^|[^a-z0-9])(?:key|token|secret|password|credential|auth)(?:\\?["'])?[ \t]*[=:][ \t]*(?:\\?["'])?)([a-zA-Z0-9+/=\-_]{32,})"#
            ),
            guard_value: true,
        },
    ]
}

pub fn redact_text(input: &str) -> String {
    redact_text_with_excludes(input, &[])
}

thread_local! {
    /// The public key a package signature was verified with, while its
    /// document is being released on this thread.
    static VERIFIED_SIGNER_KEY: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

/// Run `release` with `key` known as the Ed25519 PUBLIC key a package
/// signature has just been verified with. Inside, the built-in "Generic long
/// secret" rule — and only that rule — does not read the member
/// `"signer_public_key":"<key>"` (a bundle) or `"public_key":"<key>"` (a
/// package manifest's signature) as a secret: a public key is none, and the
/// rule fires on it only because a 64-digit value follows a name ending in
/// `key`. Every other rule, every configured pattern, every policy pack and
/// the gateway still see the original text with the real key in it and
/// decide as always. A `key` that is not 64 lowercase hex digits exempts
/// nothing.
pub fn with_verified_signer_key<T>(key: &str, release: impl FnOnce() -> T) -> T {
    let well_formed = key.len() == 64
        && key
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    if !well_formed {
        return release();
    }
    struct Restore(Option<String>);
    impl Drop for Restore {
        fn drop(&mut self) {
            VERIFIED_SIGNER_KEY.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }
    let _restore = Restore(VERIFIED_SIGNER_KEY.with(|slot| slot.replace(Some(key.to_owned()))));
    release()
}

/// Whether the generic rule's match is the verified signer key in its own
/// member: `before` is the text up to the match, which starts at the `_` of
/// `_key`. The secret detector's `high_entropy_secret` pattern (the same
/// names, the same predecessor) applies the same exemption, so admission
/// never reports a value this rule keeps.
pub(crate) fn is_verified_signer_key(before: &str, value: &str) -> bool {
    (before.ends_with("\"signer_public") || before.ends_with("\"public"))
        && VERIFIED_SIGNER_KEY.with(|slot| slot.borrow().as_deref() == Some(value))
}

/// #718: `redact_text` with subtractive user patterns from
/// `[secret_detection].exclude_patterns` — a match covered by any exclude
/// regex is kept verbatim, so known-safe naming conventions can be carved out
/// without disabling secret detection wholesale.
pub fn redact_text_with_excludes(input: &str, excludes: &[regex::Regex]) -> String {
    redact_counting(input, excludes).0
}

/// Database URL with inline credentials: scheme (1), user (2, may be empty —
/// `redis://:pw@host`), password (3), host (4). The password runs to the last
/// `@` before the host, so an unencoded `@` in it cannot leak a tail. Shared
/// with the detector so both layers agree on what a credential is.
pub(crate) fn database_url_re() -> &'static regex::Regex {
    static_regex!(
        r"(?i)((?:postgres(?:ql)?|mysql|mongodb(?:\+srv)?|redis)://)([^:/@?#\s]*):([^/?#\s]+)@([^@/?#\s]+)"
    )
}

/// userinfo with a password in any other URL (`https://[REDACTED:URL credentials]@host`): the
/// tokens elsewhere in such a URL were masked, the password stayed (focus
/// analysis 2026-10-15). Same groups as [`database_url_re`], and shared with
/// the detector, so a placeholder password (`https://user:****@host`) is
/// neither reported nor redacted (GHSA-xmx8-69qv-7f86).
pub(crate) fn url_credentials_re() -> &'static regex::Regex {
    static_regex!(
        r"(?i)((?:https?|ftps?|sftp|ssh|amqps?|smtps?|ldaps?|wss?|git)://)([^:/@?#\s]+):([^@/?#\s]+)@([^/?#\s]+)"
    )
}

/// True when a [`database_url_re`] or [`url_credentials_re`] match carries a real credential. A
/// placeholder password is not one, and neither is an already redacted
/// `scheme://[REDACTED:db_url]@host` — its `:` would otherwise read as
/// `user:password`, so a re-scan of redacted output found a "residual" secret
/// and the context gateway withheld the whole file.
pub(crate) fn database_url_has_credential(caps: &regex::Captures) -> bool {
    caps.get(2)
        .is_none_or(|user| !user.as_str().starts_with("[REDACTED"))
        && caps
            .get(3)
            .is_some_and(|password| !is_placeholder_value(password.as_str()))
}

/// [`redact_text_with_excludes`] plus the number of secrets it replaced.
///
/// One mask label per secret kind: the rule's own `label`, whoever asks. The
/// gateway's scanner (`secret_detection::scan_and_redact`) and a project
/// pack's built-in pass (`policy::content::evaluate_text`) both end here, so a
/// value masked by one is spelled exactly as the other would spell it and a
/// second pass over masked text changes nothing. (The scanner used to map the
/// labels to its pattern names — `[REDACTED:aws_key]` next to
/// `[REDACTED:AWS key]` — which made every order-of-masking comparison fail
/// for a built-in secret.)
fn redact_counting(input: &str, excludes: &[regex::Regex]) -> (String, usize) {
    let (out, found) = redact_found(input, excludes);
    (out, found.len())
}

/// The redacted text and the label of each redacted span (never its text).
fn redact_found(input: &str, excludes: &[regex::Regex]) -> (String, Vec<&'static str>) {
    let (normalized, boundaries) = normalize_for_secret_matching(input);
    let mut replacements = Vec::new();
    // The label of each replacement, index for index.
    let mut labels: Vec<&'static str> = Vec::new();

    for (re, label) in [
        (database_url_re(), "Database credentials"),
        (url_credentials_re(), "URL credentials"),
    ] {
        for caps in re.captures_iter(&normalized) {
            let Some(whole) = caps.get(0) else {
                continue;
            };
            let start = boundaries[whole.start()];
            let end = boundaries[whole.end()];
            let original = input.get(start..end).unwrap_or(whole.as_str());
            if excluded(excludes, whole.as_str()) || excluded(excludes, original) {
                continue;
            }
            // A placeholder password or an already redacted URL is no
            // credential (shared with the detector, see `database_url_re`).
            if !database_url_has_credential(&caps) {
                continue;
            }
            let scheme = caps
                .get(1)
                .map_or("", |m| original_span(input, &boundaries, m));
            let host = caps
                .get(4)
                .map_or("", |m| original_span(input, &boundaries, m));
            replacements.push((start, end, format!("{scheme}[REDACTED:{label}]@{host}")));
            labels.push(label);
        }
    }

    for rule in redaction_rules() {
        for caps in rule.re.captures_iter(&normalized) {
            let Some(whole) = caps.get(0) else {
                continue;
            };
            let start = boundaries[whole.start()];
            let mut end = boundaries[whole.end()];
            let original = input.get(start..end).unwrap_or(whole.as_str());
            if excluded(excludes, whole.as_str()) || excluded(excludes, original) {
                continue;
            }
            // A quoted value is a literal, never an identifier reference, so the
            // benign-value guard does not apply to it. When only the detector's
            // run of the value is a secret, the redaction ends with that run
            // and starts after the operator (GHSA-xmx8-69qv-7f86).
            let mut masked_from = None;
            if rule.guard_value
                && let Some(value) = caps.get(2)
            {
                let quoted = is_quoted_at(&normalized, value.start());
                match secret_value_span(value.as_str(), quoted) {
                    None => continue,
                    Some(span) if span.len() < value.len() => {
                        masked_from = Some(boundaries[value.start() + span.start]);
                        end = boundaries[value.start() + span.end];
                    }
                    Some(_) => {}
                }
            }
            if rule.label == "Bearer token"
                && caps
                    .get(2)
                    .is_some_and(|value| !bearer_value_is_credential(value.as_str()))
            {
                continue;
            }
            if rule.label == "Generic long secret"
                && caps.get(2).is_some_and(|value| {
                    caps.get(1)
                        .is_some_and(|prefix| is_fact_key_slug(prefix.as_str(), value.as_str()))
                        || is_verified_signer_key(&normalized[..whole.start()], value.as_str())
                })
            {
                continue;
            }
            // Overlapping matches: the wider span wins. Skipping a wider match (a whole
            // private-key block) because a narrower rule already hit inside it would
            // leak the rest of the block.
            let overlapping: Vec<usize> = replacements
                .iter()
                .enumerate()
                .filter(|(_, (other_start, other_end, _))| start < *other_end && *other_start < end)
                .map(|(i, _)| i)
                .collect();
            if overlapping.iter().any(|&i| {
                let (other_start, other_end, _) = &replacements[i];
                *other_start <= start && end <= *other_end
            }) {
                continue;
            }
            let start = overlapping
                .iter()
                .map(|&i| replacements[i].0)
                .fold(start, usize::min);
            let end = overlapping
                .iter()
                .map(|&i| replacements[i].1)
                .fold(end, usize::max);
            for &i in overlapping.iter().rev() {
                replacements.remove(i);
                labels.remove(i);
            }

            let label = rule.label;
            let replacement = if let Some(masked_from) = masked_from {
                let kept = input
                    .get(boundaries[whole.start()]..masked_from)
                    .unwrap_or("");
                format!("{kept}[REDACTED:{label}]")
            } else {
                caps.get(1).map_or_else(
                    || format!("[REDACTED:{label}]"),
                    |prefix| {
                        let original_prefix = original_span(input, &boundaries, prefix);
                        format!("{original_prefix}[REDACTED:{label}]")
                    },
                )
            };
            replacements.push((start, end, replacement));
            labels.push(label);
        }
    }

    let mut spans: Vec<((usize, usize, String), &'static str)> =
        replacements.into_iter().zip(labels).collect();
    spans.sort_by_key(|((start, _, _), _)| *start);
    let mut found = Vec::with_capacity(spans.len());
    let mut out = input.to_string();
    for ((start, end, replacement), label) in spans.into_iter().rev() {
        found.push(label);
        out.replace_range(start..end, &replacement);
    }
    found.reverse();
    (out, found)
}

fn original_span<'a>(input: &'a str, boundaries: &[usize], matched: regex::Match<'_>) -> &'a str {
    let start = boundaries.get(matched.start()).copied().unwrap_or_default();
    let end = boundaries.get(matched.end()).copied().unwrap_or(start);
    input.get(start..end).unwrap_or("")
}

/// Whether the byte before `pos` in `text` is a quote (the value is a quoted literal).
fn is_quoted_at(text: &str, pos: usize) -> bool {
    pos > 0 && matches!(text.as_bytes()[pos - 1], b'"' | b'\'')
}

fn excluded(excludes: &[regex::Regex], value: &str) -> bool {
    excludes.iter().any(|pattern| pattern.is_match(value))
}

/// Compile the configured `exclude_patterns` (#718). Invalid regexes are
/// skipped — a broken exclude must never disable redaction.
///
/// #952: regex compilation — the expensive part of this call — used to run
/// on every redaction call (`ctx_read`, `ctx_shell`, `ctx_execute` all funnel
/// through `redact_text_if_enabled`) regardless of whether the config had
/// changed. Cached here, keyed on `Arc::ptr_eq` against the config `Arc`:
/// `Config::load_arc` returns the *same* `Arc` when the underlying config
/// content hash is unchanged, so a real config edit is exactly what
/// invalidates this cache too — no separate change-detection needed.
pub fn config_exclude_patterns() -> std::sync::Arc<Vec<regex::Regex>> {
    type Cache = Option<(
        std::sync::Arc<crate::core::config::Config>,
        std::sync::Arc<Vec<regex::Regex>>,
    )>;
    static CACHE: std::sync::Mutex<Cache> = std::sync::Mutex::new(None);

    let cfg = crate::core::config::Config::load_arc();

    if let Ok(guard) = CACHE.lock()
        && let Some((cached_cfg, patterns)) = &*guard
        && std::sync::Arc::ptr_eq(cached_cfg, &cfg)
    {
        return std::sync::Arc::clone(patterns);
    }

    let compiled = std::sync::Arc::new(
        cfg.secret_detection
            .exclude_patterns
            .iter()
            .filter_map(|p| regex::Regex::new(p).ok())
            .collect::<Vec<_>>(),
    );

    if let Ok(mut guard) = CACHE.lock() {
        *guard = Some((cfg, std::sync::Arc::clone(&compiled)));
    }

    compiled
}

/// Apply caller-supplied policy redaction patterns on top of the built-in
/// secret rules: each regex match becomes `[REDACTED:<label>]`. Returns the
/// transformed text and the number of redactions applied (for audit counts).
///
/// Used by context policy packs (GL #673) so a pack's `[redaction]` block
/// actually removes matching content from what the model sees. The patterns are
/// the pack's `[redaction]` entries, precompiled by
/// [`crate::core::policy::runtime`].
#[must_use]
pub fn redact_with_patterns(input: &str, patterns: &[(String, regex::Regex)]) -> (String, usize) {
    let mut out = input.to_string();
    let mut hits = 0usize;
    for (label, re) in patterns {
        let mut local = 0usize;
        out = re
            .replace_all(&out, |_caps: &regex::Captures| {
                local += 1;
                format!("[REDACTED:{label}]")
            })
            .to_string();
        hits += local;
    }
    (out, hits)
}

#[cfg(test)]
mod tests;
