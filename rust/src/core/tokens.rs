use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use tiktoken_rs::CoreBPE;

static BPE: OnceLock<CoreBPE> = OnceLock::new();

fn get_bpe() -> &'static CoreBPE {
    BPE.get_or_init(|| tiktoken_rs::o200k_base().expect("failed to load o200k_base tokenizer"))
}

const TOKEN_CACHE_MAX: usize = 256;

static TOKEN_CACHE: Mutex<Option<HashMap<u64, usize>>> = Mutex::new(None);

fn hash_text(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.len().hash(&mut hasher);
    if text.len() <= 512 {
        text.hash(&mut hasher);
    } else {
        let start_end = floor_char_boundary(text, 256);
        let tail_start = ceil_char_boundary(text, text.len() - 256);
        text[..start_end].hash(&mut hasher);
        text[tail_start..].hash(&mut hasher);
    }
    hasher.finish()
}

fn floor_char_boundary(s: &str, idx: usize) -> usize {
    let idx = idx.min(s.len());
    let mut i = idx;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_char_boundary(s: &str, idx: usize) -> usize {
    let idx = idx.min(s.len());
    let mut i = idx;
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// Counts the number of BPE tokens (o200k_base) in the given text, with caching.
pub fn count_tokens(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }

    let key = hash_text(text);

    if let Ok(guard) = TOKEN_CACHE.lock() {
        if let Some(ref map) = *guard {
            if let Some(&cached) = map.get(&key) {
                return cached;
            }
        }
    }

    let count = get_bpe().encode_with_special_tokens(text).len();

    if let Ok(mut guard) = TOKEN_CACHE.lock() {
        let map = guard.get_or_insert_with(HashMap::new);
        if map.len() >= TOKEN_CACHE_MAX {
            map.clear();
        }
        map.insert(key, count);
    }

    count
}

/// Encodes text into BPE token IDs (o200k_base).
pub fn encode_tokens(text: &str) -> Vec<u32> {
    if text.is_empty() {
        return Vec::new();
    }
    get_bpe().encode_with_special_tokens(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    fn token_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn reset_cache() {
        if let Ok(mut guard) = TOKEN_CACHE.lock() {
            *guard = Some(HashMap::new());
        }
    }

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

        // Second call should return same value (cache hit path).
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
        let h1 = hash_text(&long);
        let h2 = hash_text(&long);
        assert_eq!(h1, h2);
        assert!(count_tokens(&long) > 0);
    }
}
