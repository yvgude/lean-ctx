// SPDX-License-Identifier: Apache-2.0
//! The seam through which the optional runtime is licensed.
//!
//! A lease is opaque bytes signed by LeanCTX Hive: this engine never parses a
//! plan, a seat count or an expiry from it, it only hands the bytes to the
//! runtime, which verifies them against its own Hive key. The connector
//! (`core::hive`) registers the one process-wide [`LeaseSource`]; without a
//! registered source there is no lease.

use std::sync::{Arc, RwLock};

/// One Hive-signed runtime lease, kept as the exact bytes Hive issued.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct RuntimeLeaseV1(Vec<u8>);

impl RuntimeLeaseV1 {
    pub(crate) fn from_bytes(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

// The bytes are a credential: debug output names only their length.
impl std::fmt::Debug for RuntimeLeaseV1 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RuntimeLeaseV1({} bytes)", self.0.len())
    }
}

/// Where the current lease comes from.
pub(crate) trait LeaseSource: Send + Sync {
    /// The lease to present now; `None` means not licensed.
    fn current(&self) -> Option<RuntimeLeaseV1>;
}

static SOURCE: RwLock<Option<Arc<dyn LeaseSource>>> = RwLock::new(None);

/// Registers the process-wide lease source. A later call replaces it.
pub(crate) fn set_lease_source(source: Box<dyn LeaseSource>) {
    let source: Arc<dyn LeaseSource> = Arc::from(source);
    *SOURCE
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(source);
}

/// The lease the registered source offers now; `None` without a source.
pub(crate) fn current_lease() -> Option<RuntimeLeaseV1> {
    // Clone the handle so the source is never asked while the lock is held.
    let source = SOURCE
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()?;
    source.current()
}

/// The current lease as base64, the form the runtime exchange carries;
/// registers the Hive source first.
pub(crate) fn current_lease_b64() -> Option<String> {
    use base64::Engine as _;
    crate::core::hive::runtime_lease::ensure_registered();
    let lease = current_lease()?;
    Some(base64::engine::general_purpose::STANDARD.encode(lease.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed(Option<Vec<u8>>);

    impl LeaseSource for Fixed {
        fn current(&self) -> Option<RuntimeLeaseV1> {
            self.0.clone().map(RuntimeLeaseV1::from_bytes)
        }
    }

    /// A source that asks for the lease again, which would deadlock if the
    /// registry lock were held while a source runs.
    struct Reentrant;

    impl LeaseSource for Reentrant {
        fn current(&self) -> Option<RuntimeLeaseV1> {
            set_lease_source(Box::new(Fixed(Some(b"after".to_vec()))));
            Some(RuntimeLeaseV1::from_bytes(b"during".to_vec()))
        }
    }

    // One test: the registry is process-wide state.
    #[test]
    fn the_registered_source_decides_and_a_later_one_replaces_it() {
        set_lease_source(Box::new(Fixed(None)));
        assert_eq!(current_lease(), None);

        set_lease_source(Box::new(Fixed(Some(b"lease-1".to_vec()))));
        let lease = current_lease().expect("registered lease");
        assert_eq!(lease.as_bytes(), b"lease-1");
        assert_eq!(format!("{lease:?}"), "RuntimeLeaseV1(7 bytes)");

        set_lease_source(Box::new(Reentrant));
        assert_eq!(current_lease().expect("lease").as_bytes(), b"during");
        assert_eq!(current_lease().expect("lease").as_bytes(), b"after");
    }
}
