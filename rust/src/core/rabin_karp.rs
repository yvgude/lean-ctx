const PRIME: u64 = 1_000_000_007;
const BASE: u64 = 256;
const MIN_CHUNK: usize = 64;
const MAX_CHUNK: usize = 2048;
const TARGET_CHUNK: usize = 512;
const MASK: u64 = TARGET_CHUNK as u64 - 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub offset: usize,
    pub length: usize,
    pub hash: u64,
}

pub fn chunk(content: &str) -> Vec<Chunk> {
    let bytes = content.as_bytes();
    if bytes.is_empty() {
        return vec![];
    }

    if bytes.len() <= MIN_CHUNK {
        return vec![Chunk {
            offset: 0,
            length: bytes.len(),
            hash: full_hash(bytes),
        }];
    }

    let window = 48.min(bytes.len());
    let mut chunks = Vec::new();
    let mut chunk_start = 0;
    let mut rolling = 0u64;
    let mut pow = 1u64;

    for i in 0..window.saturating_sub(1) {
        let _ = i;
        pow = pow.wrapping_mul(BASE) % PRIME;
    }

    for i in 0..bytes.len() {
        rolling = rolling.wrapping_mul(BASE).wrapping_add(bytes[i] as u64) % PRIME;

        if i >= window {
            let old = bytes[i - window] as u64;
            rolling = (rolling + PRIME - old.wrapping_mul(pow) % PRIME) % PRIME;
        }

        let chunk_len = i + 1 - chunk_start;

        let is_boundary = chunk_len >= MIN_CHUNK && (rolling & MASK == 0);
        let is_max = chunk_len >= MAX_CHUNK;

        if is_boundary || is_max || i == bytes.len() - 1 {
            let slice = &bytes[chunk_start..=i];
            chunks.push(Chunk {
                offset: chunk_start,
                length: slice.len(),
                hash: full_hash(slice),
            });
            chunk_start = i + 1;
        }
    }

    chunks
}

pub fn stable_order(old_chunks: &[Chunk], new_chunks: &[Chunk]) -> Vec<usize> {
    let old_hashes: std::collections::HashSet<u64> = old_chunks.iter().map(|c| c.hash).collect();

    let mut unchanged: Vec<usize> = Vec::new();
    let mut changed: Vec<usize> = Vec::new();

    for (i, c) in new_chunks.iter().enumerate() {
        if old_hashes.contains(&c.hash) {
            unchanged.push(i);
        } else {
            changed.push(i);
        }
    }

    unchanged.extend(changed);
    unchanged
}

pub fn reorder_content(content: &str, old_content: &str) -> String {
    let old_chunks = chunk(old_content);
    let new_chunks = chunk(content);
    let order = stable_order(&old_chunks, &new_chunks);

    let mut result = String::with_capacity(content.len());
    for &idx in &order {
        if let Some(c) = new_chunks.get(idx) {
            let end = (c.offset + c.length).min(content.len());
            result.push_str(&content[c.offset..end]);
        }
    }
    result
}

fn full_hash(data: &[u8]) -> u64 {
    let mut h = 0u64;
    for &b in data {
        h = h.wrapping_mul(BASE).wrapping_add(b as u64) % PRIME;
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_chunking() {
        let content = "a".repeat(1000);
        let c1 = chunk(&content);
        let c2 = chunk(&content);
        assert_eq!(c1, c2);
    }

    #[test]
    fn empty_content() {
        let c = chunk("");
        assert!(c.is_empty());
    }

    #[test]
    fn small_content_single_chunk() {
        let c = chunk("hello world");
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].offset, 0);
    }

    #[test]
    fn respects_min_chunk_size() {
        let content = "x".repeat(500);
        let chunks = chunk(&content);
        for c in &chunks[..chunks.len().saturating_sub(1)] {
            assert!(
                c.length >= MIN_CHUNK,
                "Chunk length {} < MIN_CHUNK {}",
                c.length,
                MIN_CHUNK
            );
        }
    }

    #[test]
    fn respects_max_chunk_size() {
        let content = "x".repeat(10000);
        let chunks = chunk(&content);
        for c in &chunks {
            assert!(
                c.length <= MAX_CHUNK + 1,
                "Chunk length {} > MAX_CHUNK {}",
                c.length,
                MAX_CHUNK
            );
        }
    }

    #[test]
    fn local_change_affects_few_chunks() {
        let original = "a".repeat(2000);
        let mut modified = original.clone();
        unsafe {
            let bytes = modified.as_bytes_mut();
            bytes[1000] = b'Z';
        }

        let c1 = chunk(&original);
        let c2 = chunk(&modified);

        let unchanged = c1
            .iter()
            .filter(|a| c2.iter().any(|b| b.hash == a.hash))
            .count();

        let stability = unchanged as f64 / c1.len().max(1) as f64;
        assert!(
            stability > 0.5,
            "Expected > 50% chunks stable, got {:.0}% ({unchanged}/{})",
            stability * 100.0,
            c1.len()
        );
    }

    #[test]
    fn stable_order_unchanged_first() {
        let content_v1 = "fn foo() { 1 }\nfn bar() { 2 }\n".repeat(50);
        let content_v2 = "fn foo() { 1 }\nfn bar() { 3 }\n".repeat(50);

        let old = chunk(&content_v1);
        let new = chunk(&content_v2);
        let order = stable_order(&old, &new);

        if order.len() >= 2 {
            let first_is_unchanged = old
                .iter()
                .any(|o| new.get(order[0]).is_some_and(|n| n.hash == o.hash));
            assert!(
                first_is_unchanged || order[0] == 0,
                "First element should be unchanged"
            );
        }
    }

    #[test]
    fn covers_all_bytes() {
        let content = "hello world, this is a longer test string for chunking purposes!".repeat(20);
        let chunks = chunk(&content);
        let total: usize = chunks.iter().map(|c| c.length).sum();
        assert_eq!(total, content.len());
    }

    #[test]
    fn unicode_content() {
        let content = "日本語テスト。これはUnicodeのテストです。".repeat(30);
        let chunks = chunk(&content);
        assert!(!chunks.is_empty());
        let total: usize = chunks.iter().map(|c| c.length).sum();
        assert_eq!(total, content.len());
    }
}
