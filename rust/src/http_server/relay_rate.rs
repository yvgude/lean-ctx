// SPDX-License-Identifier: Apache-2.0

//! Fixed-cardinality, atomic quotas for an authenticated relay receiver.
//!
//! A receiver serves one configured tenant/project. Peer identities are supplied
//! from trusted configuration, never inserted from a request.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

const CREDIT_SCALE: u128 = 1_000_000_000;
const MAX_PEERS: usize = 32;
const MAX_ID_BYTES: usize = 128;
const MAX_RATE: u32 = 100_000;

#[derive(Debug, Clone, Copy)]
pub struct RelayQuota {
    pub requests_per_second: u32,
    pub burst: u32,
}

impl RelayQuota {
    fn valid(self) -> bool {
        (1..=MAX_RATE).contains(&self.requests_per_second) && (1..=MAX_RATE).contains(&self.burst)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RelayQuotaConfig {
    pub peer: RelayQuota,
    pub tenant: RelayQuota,
    pub project: RelayQuota,
}

impl Default for RelayQuotaConfig {
    fn default() -> Self {
        Self {
            peer: RelayQuota {
                requests_per_second: 20,
                burst: 40,
            },
            tenant: RelayQuota {
                requests_per_second: 40,
                burst: 80,
            },
            project: RelayQuota {
                requests_per_second: 30,
                burst: 60,
            },
        }
    }
}

impl RelayQuotaConfig {
    pub fn validate(self) -> Result<(), &'static str> {
        if [self.peer, self.tenant, self.project]
            .into_iter()
            .all(RelayQuota::valid)
        {
            Ok(())
        } else {
            Err("relay quota rates and bursts must be between 1 and 100000")
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Bucket {
    quota: RelayQuota,
    credit: u128,
    last: Instant,
}

impl Bucket {
    fn new(quota: RelayQuota, now: Instant) -> Self {
        Self {
            quota,
            credit: u128::from(quota.burst) * CREDIT_SCALE,
            last: now,
        }
    }

    fn refill(self, now: Instant) -> Self {
        let added = now
            .saturating_duration_since(self.last)
            .as_nanos()
            .saturating_mul(u128::from(self.quota.requests_per_second));
        Self {
            credit: self
                .credit
                .saturating_add(added)
                .min(u128::from(self.quota.burst) * CREDIT_SCALE),
            last: self.last.max(now),
            ..self
        }
    }

    fn retry_after(self) -> Duration {
        let nanos = CREDIT_SCALE
            .saturating_sub(self.credit)
            .div_ceil(u128::from(self.quota.requests_per_second));
        // At least one token per second; a deficit is at most one token.
        Duration::from_nanos(nanos.min(CREDIT_SCALE) as u64)
    }

    fn consume(mut self) -> Self {
        self.credit -= CREDIT_SCALE;
        self
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum QuotaRejection {
    UnknownPeer,
    WrongScope,
    Limited { retry_after: Duration },
}

#[derive(Debug)]
pub(super) struct RelayQuotaState {
    peers: BTreeMap<String, Bucket>,
    tenant_id: String,
    project_id: String,
    tenant: Bucket,
    project: Bucket,
}

impl RelayQuotaState {
    pub(super) fn new(
        config: RelayQuotaConfig,
        peers: &[String],
        tenant_id: &str,
        project_id: &str,
        now: Instant,
    ) -> Result<Self, &'static str> {
        config.validate()?;
        let valid_id = |id: &str| !id.is_empty() && id.len() <= MAX_ID_BYTES;
        if peers.is_empty()
            || peers.len() > MAX_PEERS
            || !valid_id(tenant_id)
            || !valid_id(project_id)
            || peers.iter().any(|id| !valid_id(id))
        {
            return Err("relay quota requires bounded configured peer and scope identities");
        }
        let buckets: BTreeMap<_, _> = peers
            .iter()
            .map(|peer| (peer.clone(), Bucket::new(config.peer, now)))
            .collect();
        if buckets.len() != peers.len() {
            return Err("relay quota peer identities must be unique");
        }
        Ok(Self {
            peers: buckets,
            tenant_id: tenant_id.into(),
            project_id: project_id.into(),
            tenant: Bucket::new(config.tenant, now),
            project: Bucket::new(config.project, now),
        })
    }

    /// The caller must serialize this short operation and authenticate all IDs
    /// before calling it. No bucket is debited unless every quota permits it.
    pub(super) fn check_at(
        &mut self,
        current_peer: &str,
        origin_peer: &str,
        tenant_id: &str,
        project_id: &str,
        now: Instant,
    ) -> Result<(), QuotaRejection> {
        if tenant_id != self.tenant_id || project_id != self.project_id {
            return Err(QuotaRejection::WrongScope);
        }
        let current = self
            .peers
            .get(current_peer)
            .ok_or(QuotaRejection::UnknownPeer)?
            .refill(now);
        let origin = self
            .peers
            .get(origin_peer)
            .ok_or(QuotaRejection::UnknownPeer)?
            .refill(now);
        let tenant = self.tenant.refill(now);
        let project = self.project.refill(now);
        let retry_after = [current, origin, tenant, project]
            .into_iter()
            .map(Bucket::retry_after)
            .max()
            .unwrap_or_default();
        if !retry_after.is_zero() {
            return Err(QuotaRejection::Limited { retry_after });
        }
        // Only replace keys which were looked up above; cardinality cannot grow.
        if let Some(bucket) = self.peers.get_mut(current_peer) {
            *bucket = current.consume();
        }
        if origin_peer != current_peer
            && let Some(bucket) = self.peers.get_mut(origin_peer)
        {
            *bucket = origin.consume();
        }
        self.tenant = tenant.consume();
        self.project = project.consume();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(
        peer_burst: u32,
        tenant_burst: u32,
        project_burst: u32,
        now: Instant,
    ) -> RelayQuotaState {
        RelayQuotaState::new(
            RelayQuotaConfig {
                peer: RelayQuota {
                    requests_per_second: 1,
                    burst: peer_burst,
                },
                tenant: RelayQuota {
                    requests_per_second: 1,
                    burst: tenant_burst,
                },
                project: RelayQuota {
                    requests_per_second: 1,
                    burst: project_burst,
                },
            },
            &["a".into(), "b".into(), "c".into()],
            "tenant",
            "project",
            now,
        )
        .unwrap()
    }

    #[test]
    fn same_peer_is_debited_once_and_refills_without_sleep() {
        let now = Instant::now();
        let mut quotas = state(2, 10, 10, now);
        for _ in 0..2 {
            assert_eq!(quotas.check_at("a", "a", "tenant", "project", now), Ok(()));
        }
        assert_eq!(
            quotas.check_at("a", "a", "tenant", "project", now),
            Err(QuotaRejection::Limited {
                retry_after: Duration::from_secs(1)
            })
        );
        assert_eq!(
            quotas.check_at(
                "a",
                "a",
                "tenant",
                "project",
                now + Duration::from_millis(750)
            ),
            Err(QuotaRejection::Limited {
                retry_after: Duration::from_millis(250)
            })
        );
        assert_eq!(
            quotas.check_at("a", "a", "tenant", "project", now + Duration::from_secs(1)),
            Ok(())
        );
    }

    #[test]
    fn origin_limit_survives_changing_forwarder_without_charging_other_buckets() {
        let now = Instant::now();
        let mut quotas = state(1, 10, 10, now);
        assert_eq!(quotas.check_at("a", "a", "tenant", "project", now), Ok(()));
        let tenant_credit = quotas.tenant.credit;
        assert!(matches!(
            quotas.check_at("b", "a", "tenant", "project", now),
            Err(QuotaRejection::Limited { .. })
        ));
        assert_eq!(quotas.tenant.credit, tenant_credit);
        assert_eq!(quotas.check_at("b", "c", "tenant", "project", now), Ok(()));
    }

    #[test]
    fn tenant_and_project_limits_are_atomic_and_independent() {
        let now = Instant::now();
        for (tenant, project) in [(1, 10), (10, 1)] {
            let mut quotas = state(10, tenant, project, now);
            assert_eq!(quotas.check_at("a", "a", "tenant", "project", now), Ok(()));
            let before = quotas.peers["b"].credit;
            assert!(matches!(
                quotas.check_at("b", "b", "tenant", "project", now),
                Err(QuotaRejection::Limited { .. })
            ));
            assert_eq!(quotas.peers["b"].credit, before);
        }
    }

    #[test]
    fn untrusted_keys_never_allocate_or_debit() {
        let now = Instant::now();
        let mut quotas = state(1, 1, 1, now);
        for i in 0..100 {
            assert_eq!(
                quotas.check_at(&format!("unknown-{i}"), "a", "tenant", "project", now),
                Err(QuotaRejection::UnknownPeer)
            );
        }
        assert_eq!(
            quotas.check_at("a", "a", "other", "project", now),
            Err(QuotaRejection::WrongScope)
        );
        assert_eq!(quotas.peers.len(), 3);
        assert_eq!(quotas.check_at("a", "a", "tenant", "project", now), Ok(()));
    }

    #[test]
    fn invalid_configuration_fails_closed() {
        let now = Instant::now();
        for count in [0, MAX_PEERS + 1] {
            let peers = (0..count).map(|i| format!("peer-{i}")).collect::<Vec<_>>();
            assert!(
                RelayQuotaState::new(RelayQuotaConfig::default(), &peers, "t", "p", now).is_err()
            );
        }
        assert!(
            RelayQuotaState::new(
                RelayQuotaConfig::default(),
                &["a".into(), "a".into()],
                "t",
                "p",
                now
            )
            .is_err()
        );
        for bad in [0, MAX_RATE + 1] {
            let mut cfg = RelayQuotaConfig::default();
            cfg.peer.requests_per_second = bad;
            assert!(cfg.validate().is_err());
            cfg = RelayQuotaConfig::default();
            cfg.project.burst = bad;
            assert!(cfg.validate().is_err());
        }
    }

    #[test]
    fn reversed_time_does_not_create_credit() {
        let now = Instant::now();
        let mut quotas = state(1, 1, 1, now);
        assert_eq!(quotas.check_at("a", "a", "tenant", "project", now), Ok(()));
        assert!(matches!(
            quotas.check_at(
                "a",
                "a",
                "tenant",
                "project",
                now.checked_sub(Duration::from_secs(1)).unwrap()
            ),
            Err(QuotaRejection::Limited { .. })
        ));
        assert!(matches!(
            quotas.check_at("a", "a", "tenant", "project", now),
            Err(QuotaRejection::Limited { .. })
        ));
    }
}
