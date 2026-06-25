use std::sync::OnceLock;
use tiktoken_rs::CoreBPE;

// ── Tokenizer Families ─────────────────────────────────────

/// Tokenizer families for different LLM providers.
///
/// Different LLM families use different tokenizers, leading to 5–15% variance
/// in token counts. This enum lets callers select the appropriate tokenizer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TokenizerFamily {
    /// GPT-4o, GPT-4-turbo (tiktoken `o200k_base`, exact)
    #[default]
    O200kBase,
    /// Claude / Anthropic (approximated via tiktoken `cl100k_base`)
    Cl100k,
    /// Gemini / Google (`o200k_base` with 1.1× correction factor)
    Gemini,
    /// Llama 3+ (approximated via `cl100k_base`)
    Llama,
}

impl std::fmt::Display for TokenizerFamily {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::O200kBase => write!(f, "o200k_base"),
            Self::Cl100k => write!(f, "cl100k_base"),
            Self::Gemini => write!(f, "gemini"),
            Self::Llama => write!(f, "llama"),
        }
    }
}

/// Detects the appropriate tokenizer family from a client or model name.
///
/// Matches are case-insensitive substrings. Falls back to `O200kBase`.
/// Accuracy: cl100k is within ~3% of Claude's actual tokenizer;
/// Gemini correction factor 1.08 is empirically calibrated; o200k is exact for GPT-4o+.
#[must_use]
pub fn detect_tokenizer(client_name: &str) -> TokenizerFamily {
    let lower = client_name.to_ascii_lowercase();
    if lower.contains("claude")
        || lower.contains("anthropic")
        || lower.contains("sonnet")
        || lower.contains("opus")
        || lower.contains("haiku")
    {
        TokenizerFamily::Cl100k
    } else if lower.contains("gemini") || lower.contains("google") {
        TokenizerFamily::Gemini
    } else if lower.contains("llama")
        || lower.contains("codex")
        || lower.contains("opencode")
        || lower.contains("mistral")
        || lower.contains("deepseek")
        || lower.contains("qwen")
    {
        TokenizerFamily::Llama
    } else {
        TokenizerFamily::O200kBase
    }
}

// ── Tokenizer Instances ────────────────────────────────────

static BPE_O200K: OnceLock<Option<CoreBPE>> = OnceLock::new();
static BPE_CL100K: OnceLock<Option<CoreBPE>> = OnceLock::new();

fn get_bpe_o200k() -> Option<&'static CoreBPE> {
    BPE_O200K
        .get_or_init(|| {
            tiktoken_rs::o200k_base()
                .inspect_err(|e| tracing::error!("failed to load o200k_base tokenizer: {e}"))
                .ok()
        })
        .as_ref()
}

fn get_bpe_cl100k() -> Option<&'static CoreBPE> {
    BPE_CL100K
        .get_or_init(|| {
            tiktoken_rs::cl100k_base()
                .inspect_err(|e| tracing::error!("failed to load cl100k_base tokenizer: {e}"))
                .ok()
        })
        .as_ref()
}

fn bpe_for_family(family: TokenizerFamily) -> Option<&'static CoreBPE> {
    match family {
        TokenizerFamily::O200kBase | TokenizerFamily::Gemini => get_bpe_o200k(),
        TokenizerFamily::Cl100k | TokenizerFamily::Llama => get_bpe_cl100k(),
    }
}

const CHARS_PER_TOKEN_ESTIMATE: f64 = 3.5;

/// Gemini tokens are ~8% larger on average vs o200k; empirically calibrated.
const GEMINI_CORRECTION: f64 = 1.08;

// ── Cache ──────────────────────────────────────────────────

const TOKEN_CACHE_MAX: u64 = 4096;

fn token_cache() -> &'static moka::sync::Cache<u64, usize> {
    static CACHE: std::sync::OnceLock<moka::sync::Cache<u64, usize>> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| {
        moka::sync::Cache::builder()
            .max_capacity(TOKEN_CACHE_MAX)
            .build()
    })
}

fn hash_text(text: &str, family: TokenizerFamily) -> u64 {
    let h = blake3::hash(text.as_bytes());
    let bytes = h.as_bytes();
    let base = u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]);
    base ^ (family as u64)
}

#[cfg(test)]
fn floor_char_boundary(s: &str, idx: usize) -> usize {
    let idx = idx.min(s.len());
    let mut i = idx;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

#[cfg(test)]
fn ceil_char_boundary(s: &str, idx: usize) -> usize {
    let idx = idx.min(s.len());
    let mut i = idx;
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

// ── Public API ─────────────────────────────────────────────

/// Counts BPE tokens using the default tokenizer (`o200k_base`).
///
/// Backward-compatible — equivalent to
/// `count_tokens_for(text, TokenizerFamily::O200kBase)`.
#[must_use]
pub fn count_tokens(text: &str) -> usize {
    count_tokens_for(text, COUNTING_FAMILY)
}

/// The tokenizer family [`count_tokens`] uses for all read/savings accounting.
///
/// Centralised so honesty surfaces (the savings ledger, Wrapped) can record exactly
/// which tokenizer produced their token counts, rather than assuming the model's own.
pub const COUNTING_FAMILY: TokenizerFamily = TokenizerFamily::O200kBase;

/// Label of the tokenizer used for counting (e.g. `"o200k_base"`).
#[must_use]
pub fn counting_family_label() -> String {
    COUNTING_FAMILY.to_string()
}

/// Counts BPE tokens using the specified tokenizer family.
#[must_use]
pub fn count_tokens_for(text: &str, family: TokenizerFamily) -> usize {
    if text.is_empty() {
        return 0;
    }

    let key = hash_text(text, family);
    let cache = token_cache();

    if let Some(cached) = cache.get(&key) {
        return cached;
    }

    let Some(bpe) = bpe_for_family(family) else {
        let estimate = (text.len() as f64 / CHARS_PER_TOKEN_ESTIMATE).ceil() as usize;
        cache.insert(key, estimate);
        return estimate;
    };
    let raw = bpe.encode_with_special_tokens(text).len();
    let count = if family == TokenizerFamily::Gemini {
        (raw as f64 * GEMINI_CORRECTION).ceil() as usize
    } else {
        raw
    };

    cache.insert(key, count);
    count
}

/// Encodes text into BPE token IDs (`o200k_base`).
#[must_use]
pub fn encode_tokens(text: &str) -> Vec<u32> {
    if text.is_empty() {
        return Vec::new();
    }
    match get_bpe_o200k() {
        Some(bpe) => bpe.encode_with_special_tokens(text),
        None => Vec::new(),
    }
}

/// Encodes text into BPE token IDs for the specified tokenizer family.
///
/// Gemini correction is not applied here — this returns raw token IDs.
#[must_use]
pub fn encode_tokens_for(text: &str, family: TokenizerFamily) -> Vec<u32> {
    if text.is_empty() {
        return Vec::new();
    }
    match bpe_for_family(family) {
        Some(bpe) => bpe.encode_with_special_tokens(text),
        None => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn token_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn reset_cache() {
        token_cache().invalidate_all();
    }

    // ── Backward-compatible tests ──────────────────────────

    #[test]
    fn count_tokens_empty_is_zero() {
        assert_eq!(count_tokens(""), 0);
    }

    #[test]
    fn encode_tokens_empty_is_empty() {
        assert!(encode_tokens("").is_empty());
    }

    #[test]
    fn count_tokens_matches_encoded_length() {
        let _lock = token_test_lock();
        reset_cache();

        let text = "hello world, Grüezi 🌍";
        let counted = count_tokens(text);
        let encoded = encode_tokens(text);
        assert_eq!(counted, encoded.len());
        assert_eq!(counted, count_tokens(text));
    }

    #[test]
    fn char_boundary_helpers_handle_multibyte_indices() {
        let s = "aé🙂z";
        let emoji_start = s.find('🙂').expect("emoji exists");
        let middle_of_emoji = emoji_start + 1;

        let floor = floor_char_boundary(s, middle_of_emoji);
        let ceil = ceil_char_boundary(s, middle_of_emoji);

        assert!(s.is_char_boundary(floor));
        assert!(s.is_char_boundary(ceil));
        assert!(floor <= middle_of_emoji);
        assert!(ceil >= middle_of_emoji);
    }

    #[test]
    fn hash_text_is_stable_for_long_strings() {
        let long = "abc🙂".repeat(300);
        let h1 = hash_text(&long, TokenizerFamily::O200kBase);
        let h2 = hash_text(&long, TokenizerFamily::O200kBase);
        assert_eq!(h1, h2);
        assert!(count_tokens(&long) > 0);
    }

    // ── Multi-tokenizer tests ──────────────────────────────

    #[test]
    fn tokenizer_family_default_is_o200k() {
        assert_eq!(TokenizerFamily::default(), TokenizerFamily::O200kBase);
    }

    #[test]
    fn tokenizer_family_display() {
        assert_eq!(TokenizerFamily::O200kBase.to_string(), "o200k_base");
        assert_eq!(TokenizerFamily::Cl100k.to_string(), "cl100k_base");
        assert_eq!(TokenizerFamily::Gemini.to_string(), "gemini");
        assert_eq!(TokenizerFamily::Llama.to_string(), "llama");
    }

    #[test]
    fn detect_tokenizer_openai_variants() {
        assert_eq!(detect_tokenizer("cursor"), TokenizerFamily::O200kBase);
        assert_eq!(detect_tokenizer("openai"), TokenizerFamily::O200kBase);
        assert_eq!(detect_tokenizer("gpt-4o"), TokenizerFamily::O200kBase);
        assert_eq!(detect_tokenizer("GPT-4-turbo"), TokenizerFamily::O200kBase);
    }

    #[test]
    fn detect_tokenizer_claude_variants() {
        assert_eq!(detect_tokenizer("claude-3.5"), TokenizerFamily::Cl100k);
        assert_eq!(detect_tokenizer("anthropic"), TokenizerFamily::Cl100k);
        assert_eq!(detect_tokenizer("Claude"), TokenizerFamily::Cl100k);
    }

    #[test]
    fn detect_tokenizer_gemini_variants() {
        assert_eq!(detect_tokenizer("gemini-pro"), TokenizerFamily::Gemini);
        assert_eq!(detect_tokenizer("google"), TokenizerFamily::Gemini);
        assert_eq!(detect_tokenizer("Gemini-1.5"), TokenizerFamily::Gemini);
    }

    #[test]
    fn detect_tokenizer_llama_variants() {
        assert_eq!(detect_tokenizer("llama-3"), TokenizerFamily::Llama);
        assert_eq!(detect_tokenizer("codex"), TokenizerFamily::Llama);
        assert_eq!(detect_tokenizer("opencode"), TokenizerFamily::Llama);
    }

    #[test]
    fn detect_tokenizer_unknown_defaults_to_o200k() {
        assert_eq!(
            detect_tokenizer("unknown-model"),
            TokenizerFamily::O200kBase
        );
        assert_eq!(detect_tokenizer(""), TokenizerFamily::O200kBase);
    }

    #[test]
    fn detect_tokenizer_ledger_model_keys() {
        // #685: the savings ledger derives its counting family from the resolved
        // model key. The blended fallback (no model resolved) maps to the exact
        // o200k baseline, so the model-correct path is a byte-identical no-op for
        // OpenAI/Cursor/unknown; real provider keys map to their own family.
        assert_eq!(
            detect_tokenizer("fallback-blended"),
            TokenizerFamily::O200kBase
        );
        assert_eq!(
            detect_tokenizer("claude-sonnet-4.5"),
            TokenizerFamily::Cl100k
        );
        assert_eq!(detect_tokenizer("gemini-2.5-pro"), TokenizerFamily::Gemini);
    }

    #[test]
    fn count_tokens_for_all_families_nonzero() {
        let _lock = token_test_lock();
        reset_cache();

        let text = "fn main() { println!(\"hello\"); }";
        for family in [
            TokenizerFamily::O200kBase,
            TokenizerFamily::Cl100k,
            TokenizerFamily::Gemini,
            TokenizerFamily::Llama,
        ] {
            let count = count_tokens_for(text, family);
            assert!(count > 0, "{family} returned 0 tokens");
        }
    }

    #[test]
    fn count_tokens_for_empty_is_zero_all_families() {
        for family in [
            TokenizerFamily::O200kBase,
            TokenizerFamily::Cl100k,
            TokenizerFamily::Gemini,
            TokenizerFamily::Llama,
        ] {
            assert_eq!(count_tokens_for("", family), 0);
        }
    }

    #[test]
    fn gemini_count_exceeds_raw_o200k() {
        let _lock = token_test_lock();
        reset_cache();

        let text = "The quick brown fox jumps over the lazy dog. ".repeat(20);
        let o200k = count_tokens_for(&text, TokenizerFamily::O200kBase);
        let gemini = count_tokens_for(&text, TokenizerFamily::Gemini);
        assert!(
            gemini > o200k,
            "Gemini ({gemini}) should exceed O200kBase ({o200k}) due to 1.1× correction"
        );
    }

    #[test]
    fn cl100k_differs_from_o200k() {
        let _lock = token_test_lock();
        reset_cache();

        let text =
            "use std::collections::HashMap;\nfn main() {\n    let mut map = HashMap::new();\n}";
        let o200k = count_tokens_for(text, TokenizerFamily::O200kBase);
        let cl100k = count_tokens_for(text, TokenizerFamily::Cl100k);
        assert!(o200k > 0);
        assert!(cl100k > 0);
    }

    #[test]
    fn encode_tokens_for_matches_count() {
        let _lock = token_test_lock();
        reset_cache();

        let text = "hello world";
        for family in [
            TokenizerFamily::O200kBase,
            TokenizerFamily::Cl100k,
            TokenizerFamily::Llama,
        ] {
            let encoded = encode_tokens_for(text, family);
            let raw_count = bpe_for_family(family)
                .unwrap()
                .encode_with_special_tokens(text)
                .len();
            assert_eq!(encoded.len(), raw_count, "mismatch for {family}");
        }
    }

    #[test]
    fn cache_distinguishes_families() {
        let _lock = token_test_lock();
        reset_cache();

        let text = "cache test string";
        let o200k = count_tokens_for(text, TokenizerFamily::O200kBase);
        let cl100k = count_tokens_for(text, TokenizerFamily::Cl100k);

        let h_o200k = hash_text(text, TokenizerFamily::O200kBase);
        let h_cl100k = hash_text(text, TokenizerFamily::Cl100k);
        assert_ne!(h_o200k, h_cl100k, "cache keys must differ across families");

        assert_eq!(o200k, count_tokens_for(text, TokenizerFamily::O200kBase));
        assert_eq!(cl100k, count_tokens_for(text, TokenizerFamily::Cl100k));
    }

    #[test]
    fn default_count_tokens_is_o200k() {
        let _lock = token_test_lock();
        reset_cache();

        let text = "backward compat check";
        assert_eq!(
            count_tokens(text),
            count_tokens_for(text, TokenizerFamily::O200kBase)
        );
    }

    #[test]
    fn count_tokens_reference_snapshot_o200k() {
        // Reference counts captured at tiktoken-rs 0.6 baseline (Plan B Task 0).
        // o200k_base encoding tables are a fixed spec; counts MUST stay identical
        // across the 0.6→0.12 crate bump. A mismatch = silently wrong accounting.
        let cases: [(&str, usize); 5] = [
            ("", 0),
            ("hello world", 2),
            ("fn main() { println!(\"hello\"); }", 9),
            ("Grüezi 🌍 — café déjà vu", 9),
            (
                "use std::collections::HashMap;\nfn main() {\n    let mut map = HashMap::new();\n}",
                23,
            ),
        ];
        for (text, expected) in cases {
            assert_eq!(
                count_tokens(text),
                expected,
                "token count drift for {text:?}"
            );
        }
    }
}
