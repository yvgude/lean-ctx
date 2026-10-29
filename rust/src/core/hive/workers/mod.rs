// SPDX-License-Identifier: Apache-2.0
//! Durable workers (SPEC-ENGINE §3.6, C0 2.0 section 2): the identity an
//! agent has on Hive instead of its process id.
//!
//! - Workspace places see `wkr_` ids derived from the instance UUID, host
//!   app and role, which the server recomputes.
//! - Public vaults and swarms see place-scoped aliases keyed by the
//!   `worker_alias_key`, derived in memory from the instance signing seed
//!   and never written or sent, so co-members who know the instance UUID
//!   can neither reproduce an alias nor link two places.

pub mod attach;
pub mod card;
pub mod keys;
pub mod register;
pub mod roster;

use ed25519_dalek::SigningKey;
use unicode_normalization::UnicodeNormalization as _;
use zeroize::Zeroizing;

use super::instance_key::InstanceKeys;

/// HKDF salt of the `worker_alias_key` (info empty).
const WORKER_ALIAS_SALT: &str = "leanctx.hive.worker-alias.v1";
/// HKDF salt of a per-place alias signing seed (info = place ref).
const ALIAS_SIGNING_SALT: &str = "leanctx.hive.alias.v1";

/// Role used when none is given or nothing survives normalisation.
pub const GENERAL_ROLE: &str = "general";
/// Longest normalised role.
const ROLE_MAX: usize = 32;
/// Longest worker name, in Unicode scalar values after NFKC.
const NAME_MAX: usize = 64;
/// Case-insensitive substrings no worker name may carry.
const RESERVED: [&str; 4] = ["hive", "thinkery", "leanctx", "moderator"];

/// Lowercase; every character outside `[a-z0-9-]` becomes `-`; runs
/// collapsed and trimmed; at most 32 characters; empty → `general`.
pub fn normalize_role(role: &str) -> String {
    let mut out = String::new();
    for c in role.to_lowercase().chars() {
        let c = if c.is_ascii_lowercase() || c.is_ascii_digit() {
            c
        } else {
            '-'
        };
        if !(c == '-' && out.ends_with('-')) {
            out.push(c);
        }
    }
    let cut: String = out.trim_matches('-').chars().take(ROLE_MAX).collect();
    match cut.trim_end_matches('-') {
        "" => GENERAL_ROLE.to_string(),
        kept => kept.to_string(),
    }
}

/// The instance id as the server hashes it: the dashed lowercase UUID. A
/// 1.x enrolment stores `inst_<32 hex>`; the same id in another spelling
/// would give another worker id (E10 `worker_not_owned`, dogfood 25.10.).
pub fn canonical_instance_id(instance_id: &str) -> String {
    let hex = instance_id
        .strip_prefix("inst_")
        .unwrap_or(instance_id)
        .replace('-', "")
        .to_ascii_lowercase();
    if hex.len() == 32 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        format!(
            "{}-{}-{}-{}-{}",
            &hex[..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..]
        )
    } else {
        instance_id.to_string()
    }
}

/// Workspace worker id: `wkr_` + base32(blake3(instance ‖ "|" ‖ host ‖ "|" ‖ role))[..20],
/// over the canonical instance id.
pub fn workspace_worker_id(instance_id: &str, host_app: &str, role: &str) -> String {
    let instance_id = canonical_instance_id(instance_id);
    worker_id_of(blake3::hash(format!("{instance_id}|{host_app}|{role}").as_bytes()).as_bytes())
}

fn worker_id_of(digest: &[u8; 32]) -> String {
    let mut id = String::from("wkr_");
    id.push_str(&base32_lower(digest)[..20]);
    id
}

/// RFC 4648 base32, lowercase, without padding.
fn base32_lower(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut out = String::with_capacity(bytes.len().div_ceil(5) * 8);
    let (mut buffer, mut bits) = (0u32, 0u32);
    for &byte in bytes {
        buffer = (buffer << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buffer >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// The secret that keys place-scoped worker aliases and `hlc.node`.
pub struct WorkerAliasKey(Zeroizing<[u8; 32]>);

impl WorkerAliasKey {
    pub fn of(keys: &InstanceKeys) -> Self {
        Self(keys.derive_from_signing_seed(WORKER_ALIAS_SALT, b""))
    }

    #[cfg(test)]
    fn from_seed(seed: &[u8; 32]) -> Self {
        Self(super::instance_key::derive_from_seed(
            seed,
            WORKER_ALIAS_SALT,
            b"",
        ))
    }

    /// `wkr_` + base32(blake3_keyed(key, host ‖ "|" ‖ role ‖ "|" ‖ place_ref))[..20].
    pub fn worker_id(&self, host_app: &str, role: &str, place_ref: &str) -> String {
        let input = format!("{host_app}|{role}|{place_ref}");
        worker_id_of(blake3::keyed_hash(&self.0, input.as_bytes()).as_bytes())
    }

    /// `hex(blake3_keyed(key, place_ref))[..16]`.
    pub fn hlc_node(&self, place_ref: &str) -> String {
        blake3::keyed_hash(&self.0, place_ref.as_bytes()).to_hex()[..16].to_string()
    }
}

/// Ed25519 key that signs this instance's events in a public vault or swarm
/// (RESOLUTIONS R38). `place_ref` is `"{kind}:{id}"`.
pub fn alias_signing_key(keys: &InstanceKeys, place_ref: &str) -> SigningKey {
    let seed = keys.derive_from_signing_seed(ALIAS_SIGNING_SALT, place_ref.as_bytes());
    SigningKey::from_bytes(&seed)
}

/// `hlc.node` of this instance's events in a public vault or swarm.
pub fn alias_hlc_node(keys: &InstanceKeys, place_ref: &str) -> String {
    WorkerAliasKey::of(keys).hlc_node(place_ref)
}

/// C0 `key_id`: `hex(blake3(public key))[..16]`.
pub fn key_id(key: &SigningKey) -> String {
    blake3::hash(key.verifying_key().as_bytes()).to_hex()[..16].to_string()
}

/// Why a worker name is refused (C0 2.0 `name` vectors), checked in this order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameRefusal {
    Length,
    Charset,
    Confusable,
    Reserved,
}

impl NameRefusal {
    pub fn code(self) -> &'static str {
        match self {
            Self::Length => "length",
            Self::Charset => "charset",
            Self::Confusable => "confusable",
            Self::Reserved => "reserved",
        }
    }

    /// The text the person sees (SPEC-ENGINE §3.6, WEB §4.4).
    pub fn message(self) -> &'static str {
        match self {
            Self::Length | Self::Charset => {
                "Use 1 to 64 letters, digits, spaces and . _ · - from one alphabet."
            }
            Self::Confusable => "This name mixes alphabets. Use one alphabet.",
            Self::Reserved => {
                "Names may not contain hive, thinkery, leanctx or moderator. Pick another name."
            }
        }
    }
}

/// Scripts told apart by the single-script rule; any other script counts
/// as one more (C0 2.0 server rule).
const SCRIPTS: [&str; 14] = [
    "Latin",
    "Greek",
    "Cyrillic",
    "Armenian",
    "Hebrew",
    "Arabic",
    "Devanagari",
    "Bengali",
    "Thai",
    "Georgian",
    "Hangul",
    "Hiragana",
    "Katakana",
    "Han",
];

struct NameRules {
    charset: regex::Regex,
    letter: regex::Regex,
    scripts: Vec<regex::Regex>,
}

fn name_rules() -> &'static NameRules {
    static RULES: std::sync::OnceLock<NameRules> = std::sync::OnceLock::new();
    RULES.get_or_init(|| NameRules {
        charset: regex::Regex::new(r"^[\p{L}\p{N} ._·-]+$").expect("static regex"),
        letter: regex::Regex::new(r"^\p{L}$").expect("static regex"),
        scripts: SCRIPTS
            .iter()
            .map(|sc| regex::Regex::new(&format!(r"^\p{{sc={sc}}}$")).expect("static regex"))
            .collect(),
    })
}

/// The C0 2.0 worker-name rule, applied before a name leaves the engine.
/// Returns the NFKC form that is sent.
pub fn check_name(name: &str) -> Result<String, NameRefusal> {
    let name: String = name.nfkc().collect();
    let len = name.chars().count();
    if len == 0 || len > NAME_MAX {
        return Err(NameRefusal::Length);
    }
    let rules = name_rules();
    if !rules.charset.is_match(&name) {
        return Err(NameRefusal::Charset);
    }
    let mut script = None;
    let mut buf = [0u8; 4];
    for ch in name.chars() {
        let text = ch.encode_utf8(&mut buf);
        if !rules.letter.is_match(text) {
            continue;
        }
        let this = rules.scripts.iter().position(|r| r.is_match(text));
        match script {
            None => script = Some(this),
            Some(seen) if seen != this => return Err(NameRefusal::Confusable),
            Some(_) => {}
        }
    }
    let lower = name.to_lowercase();
    if RESERVED.iter().any(|r| lower.contains(r)) {
        return Err(NameRefusal::Reserved);
    }
    Ok(name)
}

/// Default name `<instance label> · <Host app> · <role>`, built to pass
/// [`check_name`]: label characters outside the rule are dropped, and a
/// label that still fails is left out.
pub fn default_name(instance_label: &str, host_display: &str, role: &str) -> String {
    let tail = format!("{host_display} · {role}");
    let label: String = instance_label
        .nfkc()
        .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '.' | '_' | '-'))
        .collect();
    let label = label.split_whitespace().collect::<Vec<_>>().join(" ");
    let full = format!("{label} · {tail}");
    if !label.is_empty() && check_name(&full).is_ok() {
        full
    } else {
        tail
    }
}

#[cfg(test)]
mod tests;
