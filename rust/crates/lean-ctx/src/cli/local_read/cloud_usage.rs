// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx cloud usage --json`: how far each LeanCTX Cloud quota of the
//! connected workspace is used (`GET /v1/usage`), for a local reader such as
//! LeanCTX Code. Asked only when the server announced the `usage`
//! capability; otherwise the document says `available: false` and no
//! request leaves the machine. When the server does not answer, the last
//! answer of that server is returned with `live: false` and its `as_of`.

use lean_ctx_time::UtcDateTime;
use serde::Serialize;

use lean_ctx_cloud::CloudStore;
use lean_ctx_cloud::config::CloudConfig;
use lean_ctx_cloud::usage::UsageRead;
use lean_ctx_protocol::cloud::usage::CloudUsageV1;

use super::Failure;

/// The local-read kind of this document.
pub(crate) const KIND: &str = "cloud.usage";

#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct UsageDoc {
    /// `false`: the server does not offer usage (older LeanCTX Cloud).
    pub available: bool,
    /// The cloud's answer (schema `leanctx.cloud.usage/v1`); `null` when not available.
    pub usage: Option<CloudUsageV1>,
    /// When `usage` was read (RFC 3339).
    pub as_of: Option<String>,
    /// Read just now; `false` = the last answer (the server did not answer).
    pub live: bool,
}

pub(crate) fn build(read: Option<UsageRead>) -> UsageDoc {
    match read {
        Some(r) => UsageDoc {
            available: true,
            usage: Some(r.usage),
            as_of: r.as_of,
            live: r.live,
        },
        None => UsageDoc {
            available: false,
            usage: None,
            as_of: None,
            live: false,
        },
    }
}

pub(crate) fn run(config: &CloudConfig) {
    super::emit(KIND, || {
        let store = CloudStore::open().map_err(Failure::from)?;
        let read = lean_ctx_cloud::usage::read(&store, config, UtcDateTime::now())
            .map_err(Failure::from)?;
        Ok(build(read))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage() -> CloudUsageV1 {
        serde_json::from_str(
            r#"{"schema":"leanctx.cloud.usage/v1","plan":"personal","read_only":false,"quotas":{"storage_bytes":{"limit":1073741824,"used":966367641,"reset_at":null,"state":"warn","next_step":"upgrade_team"}},"notices":[{"kind":"quota_warn"}],"model_credit":{"top_up_usd_micros":5000000},"suggest_team":true}"#,
        )
        .expect("usage fixture")
    }

    #[test]
    fn a_server_without_usage_reads_as_not_available() {
        let doc = serde_json::to_value(build(None)).expect("json");
        assert_eq!(doc["available"], false);
        assert!(doc["usage"].is_null());
        assert_eq!(doc["live"], false);
    }

    #[test]
    fn the_answer_is_carried_with_its_time_and_liveness() {
        let doc = serde_json::to_value(build(Some(UsageRead {
            usage: usage(),
            as_of: Some("2026-11-17T12:00:00Z".into()),
            live: false,
        })))
        .expect("json");
        assert_eq!(doc["available"], true);
        assert_eq!(doc["usage"]["schema"], "leanctx.cloud.usage/v1");
        assert_eq!(doc["usage"]["quotas"]["storage_bytes"]["state"], "warn");
        assert_eq!(doc["as_of"], "2026-11-17T12:00:00Z");
        assert_eq!(doc["live"], false);
        // the parts the engine does not read reach the app as the cloud sent them
        assert_eq!(doc["usage"]["notices"][0]["kind"], "quota_warn");
        assert_eq!(doc["usage"]["model_credit"]["top_up_usd_micros"], 5_000_000);
        assert_eq!(doc["usage"]["suggest_team"], true);
    }

    #[test]
    fn cloud_usage_is_listed_in_capabilities() {
        assert!(super::super::capabilities::KINDS.contains(&KIND));
        assert!(
            super::super::capabilities::COMMANDS
                .iter()
                .any(|(group, verbs)| *group == "cloud" && verbs.contains(&"usage"))
        );
    }
}
