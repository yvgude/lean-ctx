// SPDX-License-Identifier: Apache-2.0
//! Ids of C0 2.0 events and nodes (`vectors/node_id.vectors.json`).

/// RFC 4648 base32, lowercase, MSB first, no padding.
const BASE32_LOWER: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
/// Crockford base32, uppercase (ULID alphabet).
const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Base32 in RFC 4648 bit order over `alphabet`, no padding.
fn base32(bytes: &[u8], alphabet: &[u8; 32]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(5) * 8);
    let mut acc: u32 = 0;
    let mut bits = 0;
    for &b in bytes {
        acc = (acc << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(alphabet[((acc >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(alphabet[((acc << (5 - bits)) & 31) as usize] as char);
    }
    out
}

pub fn base32_lower(bytes: &[u8]) -> String {
    base32(bytes, BASE32_LOWER)
}

/// Crockford-upper base32 in RFC 4648 bit order (conversion event ids).
pub fn crockford_upper(bytes: &[u8]) -> String {
    base32(bytes, CROCKFORD)
}

/// Claim node id from its exact dedupe key: `"n_" + base32lower(dk)[..26]`.
pub fn claim_node_id(dk: &[u8; 32]) -> String {
    format!("n_{}", &base32_lower(dk)[..26])
}

/// Random id: `prefix + "_" + base32lower(16 bytes)[..26]` (other node
/// types, edges, batches, …).
pub fn random_id(prefix: &str, bytes: &[u8; 16]) -> String {
    format!("{prefix}_{}", &base32_lower(bytes)[..26])
}

pub fn new_random_id(prefix: &str) -> Result<String, crate::core::hive::HiveError> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|e| crate::core::hive::HiveError::Local(format!("random: {e}")))?;
    Ok(random_id(prefix, &bytes))
}

/// Event id in ULID layout: 48-bit `wall_ms` ‖ 80 random bits as one
/// 128-bit big-endian number, Crockford upper, 26 characters.
pub fn event_id(wall_ms: u64, random: &[u8; 10]) -> String {
    let mut n: u128 = u128::from(wall_ms & 0xffff_ffff_ffff) << 80;
    for (i, b) in random.iter().enumerate() {
        n |= u128::from(*b) << (8 * (9 - i));
    }
    // 26 × 5 = 130 bits: the top two bits are zero.
    (0..26)
        .rev()
        .map(|i| CROCKFORD[((n >> (5 * i)) & 31) as usize] as char)
        .collect()
}

pub fn new_event_id(wall_ms: u64) -> Result<String, crate::core::hive::HiveError> {
    let mut random = [0u8; 10];
    getrandom::fill(&mut random)
        .map_err(|e| crate::core::hive::HiveError::Local(format!("random: {e}")))?;
    Ok(event_id(wall_ms, &random))
}

/// Event id of a converted 1.x item (`legacy_conversion`): first 26 chars of
/// Crockford-upper base32 of `blake3_keyed(pk_id, "conv" ‖ revision_be8 ‖
/// item_key)`.
pub fn legacy_conversion_event_id(pk_id: &[u8; 32], revision: u64, item_key: &str) -> String {
    let mut h = blake3::Hasher::new_keyed(pk_id);
    h.update(b"conv");
    h.update(&revision.to_be_bytes());
    h.update(item_key.as_bytes());
    crockford_upper(h.finalize().as_bytes())[..26].to_string()
}

/// Event id of a sealing conversion copy (`seal_conversion`, R90).
pub fn seal_conversion_event_id(pk_id: &[u8; 32], seq: u64, original_event_id: &str) -> String {
    let mut h = blake3::Hasher::new_keyed(pk_id);
    h.update(b"seal");
    h.update(&seq.to_be_bytes());
    h.update(original_event_id.as_bytes());
    crockford_upper(h.finalize().as_bytes())[..26].to_string()
}

/// `pk_id` of a sealed place: `HKDF-SHA256(ikm = place_secret, salt =
/// empty, info = "leanctx.hive.node-id.v1", L = 32)`.
pub fn sealed_pk_id(place_secret: &[u8; 32]) -> [u8; 32] {
    let mut out = [0u8; 32];
    hkdf::Hkdf::<sha2::Sha256>::new(None, place_secret)
        .expand(b"leanctx.hive.node-id.v1", &mut out)
        .expect("32 bytes is a valid HKDF length");
    out
}
