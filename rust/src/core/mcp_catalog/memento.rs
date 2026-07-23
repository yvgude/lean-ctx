//! Secret mementos for downstream MCP auth.
//!
//! Gateway config stores opaque memento ids. Secret material is restored at the
//! transport edge and kept out of serialized config/debug output.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

/// Runtime secret store used by gateway mementos.
///
/// In-memory values take precedence over the deterministic environment fallback
/// returned by [`env_name`]. Empty values are treated as unavailable.
#[derive(Default)]
pub struct SecretMementoStore {
    values: Mutex<BTreeMap<String, String>>,
}

impl SecretMementoStore {
    #[must_use]
    pub fn global() -> &'static Self {
        static STORE: OnceLock<SecretMementoStore> = OnceLock::new();
        STORE.get_or_init(Self::default)
    }

    pub fn put(&self, id: impl Into<String>, secret: impl Into<String>) {
        self.values
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id.into(), secret.into());
    }

    pub fn remove(&self, id: &str) {
        self.values
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(id);
    }

    #[must_use]
    pub fn restore(&self, id: &str) -> Option<String> {
        if let Some(secret) = self
            .values
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .cloned()
        {
            return (!secret.is_empty()).then_some(secret);
        }
        std::env::var(env_name(id)).ok().filter(|v| !v.is_empty())
    }
}

#[must_use]
pub fn env_name(id: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";

    let mut name = String::with_capacity("LEAN_CTX_SECRET_".len() + id.len() * 2);
    name.push_str("LEAN_CTX_SECRET_");
    for byte in id.as_bytes() {
        name.push(char::from(HEX[usize::from(byte >> 4)]));
        name.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    name
}

#[must_use]
pub fn fingerprint(secret: &str) -> String {
    blake3::hash(secret.as_bytes()).to_hex()[..12].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_name_is_stable_and_shell_safe() {
        assert_eq!(
            env_name("mcp/gitlab/default"),
            "LEAN_CTX_SECRET_6D63702F6769746C61622F64656661756C74"
        );
    }

    #[test]
    fn env_name_is_injective_for_previously_colliding_ids() {
        let encoded = ["a/b", "a-b", "A_B"].map(env_name);
        let names = encoded
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();

        assert_eq!(names.len(), 3);
    }

    #[test]
    fn store_restores_seeded_secret() {
        let store = SecretMementoStore::global();
        store.put("mcp/test/store", "secret");
        assert_eq!(store.restore("mcp/test/store").as_deref(), Some("secret"));
        store.remove("mcp/test/store");
    }

    #[test]
    fn store_falls_back_to_derived_environment_name() {
        let id = "mcp/test/env-fallback";
        let name = env_name(id);
        crate::test_env::set_var(&name, "environment-secret");
        assert_eq!(
            SecretMementoStore::global().restore(id).as_deref(),
            Some("environment-secret")
        );
        crate::test_env::remove_var(&name);
    }

    #[test]
    fn empty_seeded_secret_is_unavailable() {
        let store = SecretMementoStore::global();
        store.put("mcp/test/empty", "");
        assert_eq!(store.restore("mcp/test/empty"), None);
        store.remove("mcp/test/empty");
    }
}
