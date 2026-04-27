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
