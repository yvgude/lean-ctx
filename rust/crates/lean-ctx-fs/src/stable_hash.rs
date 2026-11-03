// SPDX-License-Identifier: Apache-2.0
//! A hasher whose output never changes between Rust releases, for names that
//! are stored: the project directories under the data dir are keyed by it.
//!
//! [`StableHasher`] is SipHash-1-3 with the key `(0, 0)`, the algorithm
//! `std::collections::hash_map::DefaultHasher::new()` has used since Rust
//! 1.13. Std documents that algorithm as unspecified, so a future toolchain
//! could change it and move every store; this copy keeps the names the
//! engine has written so far. The pinned vectors in the tests are the
//! contract, not a comparison with std.

use std::hash::Hasher;

/// SipHash-1-3, key `(0, 0)`: one compression round per 8-byte word, three
/// finalization rounds.
#[derive(Debug, Clone)]
pub struct StableHasher {
    v: [u64; 4],
    /// Bytes not yet compressed (fewer than 8), little-endian.
    tail: u64,
    tail_len: usize,
    /// Total input length; only its low byte enters the last word.
    len: usize,
}

impl Default for StableHasher {
    fn default() -> Self {
        Self::new()
    }
}

impl StableHasher {
    #[must_use]
    pub fn new() -> Self {
        Self {
            v: [
                0x736f_6d65_7073_6575,
                0x646f_7261_6e64_6f6d,
                0x6c79_6765_6e65_7261,
                0x7465_6462_7974_6573,
            ],
            tail: 0,
            tail_len: 0,
            len: 0,
        }
    }

    fn round(&mut self) {
        let [v0, v1, v2, v3] = &mut self.v;
        *v0 = v0.wrapping_add(*v1);
        *v1 = v1.rotate_left(13) ^ *v0;
        *v0 = v0.rotate_left(32);
        *v2 = v2.wrapping_add(*v3);
        *v3 = v3.rotate_left(16) ^ *v2;
        *v0 = v0.wrapping_add(*v3);
        *v3 = v3.rotate_left(21) ^ *v0;
        *v2 = v2.wrapping_add(*v1);
        *v1 = v1.rotate_left(17) ^ *v2;
        *v2 = v2.rotate_left(32);
    }

    fn compress(&mut self, word: u64) {
        self.v[3] ^= word;
        self.round();
        self.v[0] ^= word;
    }
}

impl Hasher for StableHasher {
    fn write(&mut self, bytes: &[u8]) {
        self.len = self.len.wrapping_add(bytes.len());
        for &byte in bytes {
            self.tail |= u64::from(byte) << (8 * self.tail_len);
            self.tail_len += 1;
            if self.tail_len == 8 {
                let word = self.tail;
                self.compress(word);
                self.tail = 0;
                self.tail_len = 0;
            }
        }
    }

    fn finish(&self) -> u64 {
        let mut state = self.clone();
        let last = ((state.len as u64 & 0xff) << 56) | state.tail;
        state.compress(last);
        state.v[2] ^= 0xff;
        for _ in 0..3 {
            state.round();
        }
        state.v[0] ^ state.v[1] ^ state.v[2] ^ state.v[3]
    }
}

/// The 16-hex-digit name of `parts`, each hashed as a `str` (its bytes, then
/// `0xff`): the form of every project directory name.
#[must_use]
pub fn hex_of_strs<S: AsRef<str>>(parts: &[S]) -> String {
    use std::hash::Hash;
    let mut hasher = StableHasher::new();
    for part in parts {
        part.as_ref().hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::hash::Hash;

    /// Pinned: these names exist on users' disks. Computed independently
    /// (reference SipHash-1-3) and equal to `DefaultHasher::new()` of Rust
    /// 1.99.1. A change here moves every store.
    #[test]
    fn names_are_pinned() {
        let long = "a".repeat(100);
        let cases: [(&[&str], &str); 5] = [
            (&[""], "30406ea523c53def"),
            (&["/workspace"], "ffb2f5bddb564686"),
            (
                &["/Users/a/repo", "git:https://github.com/acme/repo.git"],
                "d6b034eec52f1cb5",
            ),
            (&["D:/repo"], "330132ce3328cddd"),
            (&[long.as_str()], "5c4d00aef4d91df1"),
        ];
        for (parts, name) in cases {
            assert_eq!(hex_of_strs(parts), name, "{parts:?}");
        }
    }

    /// Every word boundary: the buffered tail must give what one write does.
    #[test]
    fn split_writes_hash_like_one_write() {
        let data: Vec<u8> = (0u8..=40).collect();
        for cut in 0..data.len() {
            let mut whole = StableHasher::new();
            whole.write(&data);
            let mut split = StableHasher::new();
            split.write(&data[..cut]);
            split.write(&data[cut..]);
            assert_eq!(whole.finish(), split.finish(), "cut at {cut}");
        }
    }

    /// `hex_of_strs` is the same stream as hashing each part in turn.
    #[test]
    fn parts_hash_in_order() {
        let mut hasher = StableHasher::new();
        "/workspace".hash(&mut hasher);
        "git:x".hash(&mut hasher);
        assert_eq!(
            hex_of_strs(&["/workspace", "git:x"]),
            format!("{:016x}", hasher.finish())
        );
        assert_ne!(
            hex_of_strs(&["/workspace", "git:x"]),
            hex_of_strs(&["/workspacegit:x"])
        );
    }
}
