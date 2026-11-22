// SPDX-License-Identifier: Apache-2.0
//! The team stop (kill switch) as the Cloud reports it to an engine
//! (`GET /v1/stop`, acting as the person the instance is linked to).
//!
//! A stopped team's engines get `423 team_stopped` from the Cloud on its
//! writes, landings and Code sessions. Nothing here gates anything on this
//! machine: local agent tools keep working (owner decision 19.11.). This
//! module only reads the state for `cloud status` and the `cloud stop`
//! commands, and keeps the last answer of the enrolled server in
//! `<data>/team-stop.json` with the time it was read. `cloud status` reads
//! only that file (never the Cloud): the scheduler tick and the `cloud stop`
//! commands refresh it, and status names its age (`age_s`) and whether it is
//! older than two pull intervals (`stale`).
//!
//! The Cloud answers `200 {team, stopped, since, by: {id, name}, reason,
//! generation, role, may_stop, may_release, company?: {admin}, step_up_url?}`;
//! the instance credential names the team (no org in the route). Before any
//! call of `/v1/stop` the engine reads `GET /v1/capabilities`: a Cloud that
//! does not announce `team_stop` is "not offered" and hears nothing of it;
//! a `404` from the route reads the same (`Ok(None)`).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::CloudError;
use crate::config::CloudConfig;
use crate::state::{CloudStore, write_private};
use crate::transport::CloudClient;

/// The Cloud route (under `/v1`): `GET` the state, `POST {reason}` stops,
/// `DELETE {confirm, note?}` releases.
pub const PATH: &str = "/stop";
/// The `features` entry of a Cloud that offers the team stop.
pub const FEATURE: &str = "team_stop";
/// Largest answer read.
pub const MAX_ANSWER_BYTES: usize = 64 * 1024;
/// Longest reason the Cloud keeps (and this engine sends).
pub const MAX_REASON_CHARS: usize = 500;
/// The last answer.
const FILE: &str = "team-stop.json";

/// The team stop as one answer of the Cloud, checked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamStop {
    /// The team's slug (what `--confirm` must name).
    pub team: String,
    pub stopped: bool,
    /// When the stop began, as the Cloud wrote it (RFC 3339).
    pub since: Option<String>,
    /// Who stopped it (display name).
    pub by: Option<String>,
    pub reason: Option<String>,
    /// Counts stops and releases (the Cloud's; `0` when absent).
    pub generation: u64,
    /// The linked person's acting role in the team.
    pub role: Option<String>,
    pub may_stop: bool,
    pub may_release: bool,
    /// Whether the person administers the team's company; `None` when the
    /// Cloud does not say (or the team has no company).
    pub company_admin: Option<bool>,
    /// The browser page that confirms a release (two-factor), when the
    /// Cloud named one.
    pub step_up_url: Option<String>,
}

impl TeamStop {
    /// The page that confirms a release: the Cloud's when it lies below the
    /// web app's origin, else the web app's stop page of the team when this
    /// person may release an active stop.
    #[must_use]
    pub fn release_link(&self, web_origin: &str) -> Option<String> {
        if let Some(url) = self
            .step_up_url
            .as_ref()
            .filter(|u| below_origin(u, web_origin))
        {
            return Some(url.clone());
        }
        (self.stopped && self.may_release)
            .then(|| format!("{}/o/{}/stop/", web_origin.trim_end_matches('/'), self.team))
    }
}

/// Whether `slug` has the Cloud's slug shape (`[a-z0-9-]`, 1–64).
#[must_use]
pub fn is_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 64
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// The Cloud's words, safe for a terminal: control characters become
/// spaces, at most `max` characters; `None` when nothing is left.
#[must_use]
pub fn safe_text(text: &str, max: usize) -> Option<String> {
    let text: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(max)
        .collect();
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// Whether `url` is a plain link below `origin` (same scheme, host and
/// port; no whitespace or control characters).
#[must_use]
pub fn below_origin(url: &str, origin: &str) -> bool {
    let origin = origin.trim_end_matches('/');
    url.len() <= 2048
        && !origin.is_empty()
        && !url.chars().any(|c| c.is_control() || c.is_whitespace())
        && url
            .strip_prefix(origin)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// The checked state of a `200` answer. `team` and `stopped` are required;
/// other members that are malformed are left out, never repaired.
pub fn decode(body: &Value) -> Result<TeamStop, CloudError> {
    let bad = |why: &str| CloudError::Protocol(format!("team stop: {why}"));
    let team = body["team"]
        .as_str()
        .filter(|t| is_slug(t))
        .ok_or_else(|| bad("team is not a slug"))?;
    let stopped = body["stopped"]
        .as_bool()
        .ok_or_else(|| bad("stopped is not a boolean"))?;
    let since = body["since"]
        .as_str()
        .filter(|t| t.len() <= 64 && t.chars().all(|c| c.is_ascii_graphic()))
        .map(str::to_owned);
    let role = body["role"]
        .as_str()
        .filter(|r| {
            !r.is_empty() && r.len() <= 32 && r.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
        })
        .map(str::to_owned);
    Ok(TeamStop {
        team: team.to_owned(),
        stopped,
        since: since.filter(|_| stopped),
        // The display name, else the email when the person set no name.
        by: ["name", "email"]
            .iter()
            .find_map(|key| body["by"][*key].as_str().and_then(|n| safe_text(n, 200)))
            .filter(|_| stopped),
        reason: body["reason"]
            .as_str()
            .and_then(|r| safe_text(r, MAX_REASON_CHARS))
            .filter(|_| stopped),
        generation: body["generation"].as_u64().unwrap_or(0),
        role,
        may_stop: body["may_stop"].as_bool() == Some(true),
        may_release: body["may_release"].as_bool() == Some(true),
        company_admin: body["company"]["admin"].as_bool(),
        step_up_url: body["step_up_url"]
            .as_str()
            .filter(|u| {
                let host = u
                    .strip_prefix("https://")
                    .or_else(|| u.strip_prefix("http://"));
                host.is_some_and(|rest| !rest.starts_with('/'))
                    && u.len() <= 2048
                    && !u.chars().any(|c| c.is_control() || c.is_whitespace())
            })
            .map(str::to_owned),
    })
}

/// `GET /v1/stop` with `client`. A `401` is an error, so the session mints a
/// new token once and then reports it.
pub fn fetch(client: &CloudClient) -> Result<(u16, Value), CloudError> {
    let (status, body) = client.send_answer("GET", PATH, None, MAX_ANSWER_BYTES)?;
    if status == 401 {
        return Err(CloudError::Unauthorized);
    }
    Ok((status, body))
}

/// Whether the Cloud `client` talks to offers the team stop: asks
/// `GET /v1/capabilities` and records the answer for `server` (the enrolled
/// origin), as every feature check does.
pub fn offered(store: &CloudStore, server: &str, client: &CloudClient) -> Result<bool, CloudError> {
    crate::session_ref::refresh(store, server, client)?;
    Ok(crate::session_ref::announced(store, FEATURE))
}

/// A refusal of `/v1/stop` the team stop does not name itself, in the
/// shared mapping of every cloud call.
#[must_use]
pub fn refused(status: u16, body: &Value) -> CloudError {
    CloudError::from_response(status, body)
}

/// The state of a `GET /v1/stop` answer; `None` from a Cloud without the
/// route.
pub fn classify(status: u16, body: &Value) -> Result<Option<TeamStop>, CloudError> {
    let code = body["error"]["code"].as_str().unwrap_or_default();
    match status {
        200 => decode(body).map(Some),
        404 if code.is_empty() || code == "not_found" => Ok(None),
        403 => Err(CloudError::Forbidden),
        _ => Err(CloudError::from_response(status, body)),
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Cache {
    /// The origin that answered; another enrolled server reads none of it.
    server: String,
    /// The team and the enrolment it was read for: after a new `cloud
    /// connect` (another team on the same server, `--keep-local`) none of
    /// it is read.
    org_id: String,
    instance_id: String,
    /// `None`: this Cloud does not offer the team stop (it answered `404`).
    stop: Option<TeamStop>,
    /// When it answered (seconds since the Unix epoch, this machine's clock).
    read_at: Option<u64>,
}

/// A kept answer of the Cloud and how old it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamStopRead {
    pub stop: TeamStop,
    /// Seconds since the Cloud answered it; `None` when unknown (an older
    /// file, or a clock that went back).
    pub age_s: Option<u64>,
    /// Older than two pull intervals, or of unknown age: the tick did not
    /// refresh it as it should.
    pub stale: bool,
}

/// What `cloud status` knows of the team stop, from the kept answer only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cached {
    /// Connected, but no answer kept yet (no tick or `cloud stop` ran).
    Missing,
    /// This Cloud does not offer the team stop.
    NotOffered,
    Kept(TeamStopRead),
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The team stop for `cloud status`: the kept answer of the enrolled
/// server, never a call of the Cloud. `None` when not enrolled.
pub fn read(store: &CloudStore, config: &CloudConfig) -> Option<Cached> {
    let instance = store.instance().ok()??;
    let Some(cache) = load(store).filter(|c| c.is_for(&instance)) else {
        return Some(Cached::Missing);
    };
    let Some(stop) = cache.stop else {
        return Some(Cached::NotOffered);
    };
    let age_s = cache.read_at.and_then(|at| now_unix().checked_sub(at));
    let fresh_for = u64::from(config.pull_interval()) * 60 * 2;
    Some(Cached::Kept(TeamStopRead {
        stop,
        age_s,
        stale: age_s.is_none_or(|age| age > fresh_for),
    }))
}

/// Ask the Cloud with `session` and keep its answer (the tick, and the
/// `cloud stop` commands through [`keep`]). A failure keeps the last answer;
/// a Cloud that does not announce [`FEATURE`] is asked nothing else.
pub fn refresh_with(
    store: &CloudStore,
    session: &crate::auth::Session,
) -> Result<Option<TeamStop>, CloudError> {
    let server = session.instance.url.clone();
    if !session.with_client(|client| offered(store, &server, client))? {
        keep(store, None);
        return Ok(None);
    }
    let (status, body) = session.with_client(fetch)?;
    let stop = classify(status, &body)?;
    keep(store, stop.as_ref());
    Ok(stop)
}

/// The Cloud session of this machine for the team stop: none when
/// `[cloud] enabled = false`, not connected, or the lease lapsed (as every
/// other cloud call off the tick).
pub fn session(
    store: &CloudStore,
    config: &CloudConfig,
) -> Result<crate::auth::Session, CloudError> {
    crate::scheduler::connected_session(store, config)
}

/// Fill the team-stop members of `cloud status --json` from `cached`
/// (`web_origin`: the web app's origin, for the release link). Nothing for
/// a Cloud that does not offer the stop.
pub fn apply(
    status: &mut lean_ctx_protocol::cloud::status::CloudCliStatusV1,
    cached: &Cached,
    web_origin: &str,
) {
    use lean_ctx_protocol::cloud::status::{CloudStatusCompanyV1, CloudStatusTeamStopV1};
    let read = match cached {
        Cached::NotOffered => return,
        // No answer kept yet: `team_stop: null`, never defaults that read
        // as a running team nobody may stop.
        Cached::Missing => {
            status.team_stop = Some(None);
            return;
        }
        Cached::Kept(read) => read,
    };
    let stop = &read.stop;
    status.team_stop = Some(Some(CloudStatusTeamStopV1 {
        active: stop.stopped,
        reason: stop.reason.clone(),
        since: stop.since.clone(),
        by: stop.by.clone(),
        generation: stop.generation,
        may_stop: stop.may_stop,
        may_release: stop.may_release,
        cache: if read.stale { "stale" } else { "fresh" }.into(),
        age_s: read.age_s,
    }));
    status.role.clone_from(&stop.role);
    status.org_slug = Some(stop.team.clone());
    status.company = stop
        .company_admin
        .map(|admin| CloudStatusCompanyV1 { admin });
    status.step_up_url = stop.release_link(web_origin).filter(|_| stop.stopped);
}

/// Keep `stop` as the answer of the enrolled server just read (`None`: it
/// does not offer the team stop). Fail-safe: a Cloud that stops offering it
/// does not clear a kept stop of the team; that stop stays, of unknown age
/// (so `stale`), until the Cloud answers the state again.
pub fn keep(store: &CloudStore, stop: Option<&TeamStop>) {
    let Ok(Some(instance)) = store.instance() else {
        return;
    };
    let (stop, read_at) = match stop {
        Some(stop) => (Some(stop.clone()), Some(now_unix())),
        None => match load(store).filter(|c| c.is_for(&instance)) {
            Some(Cache {
                stop: Some(kept), ..
            }) if kept.stopped => (Some(kept), None),
            _ => (None, Some(now_unix())),
        },
    };
    let cache = Cache {
        server: instance.url,
        org_id: instance.org_id,
        instance_id: instance.instance_id,
        stop,
        read_at,
    };
    if let Ok(bytes) = serde_json::to_vec_pretty(&cache) {
        let _ = write_private(store.data_dir(), FILE, &bytes);
    }
}

impl Cache {
    /// Read for this enrolment: its server, its team and its instance.
    fn is_for(&self, instance: &crate::state::InstanceRecord) -> bool {
        self.server == instance.url
            && self.org_id == instance.org_id
            && self.instance_id == instance.instance_id
    }
}

fn load(store: &CloudStore) -> Option<Cache> {
    std::fs::read(store.data_dir().join(FILE))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn stopped() -> Value {
        json!({
            "team": "acme-platform",
            "stopped": true,
            "since": "2026-11-19T10:00:00Z",
            "by": {"id": "8d0c", "name": "Ada\u{1b}[2J Lovelace"},
            "reason": "runaway agent\nspending",
            "generation": 3,
            "team_stop": {"since": "2026-11-19T10:00:00Z", "generation": 3},
            "role": "admin",
            "may_stop": true,
            "may_release": true,
            "signed": {"v": 1, "signature": "sig"}
        })
    }

    #[test]
    fn a_stop_is_checked_and_its_words_made_safe() {
        let stop = decode(&stopped()).unwrap();
        assert_eq!(
            stop,
            TeamStop {
                team: "acme-platform".into(),
                stopped: true,
                since: Some("2026-11-19T10:00:00Z".into()),
                by: Some("Ada [2J Lovelace".into()),
                reason: Some("runaway agent spending".into()),
                generation: 3,
                role: Some("admin".into()),
                may_stop: true,
                may_release: true,
                company_admin: None,
                step_up_url: None,
            }
        );
        assert_eq!(
            stop.release_link("https://cloud.leanctx.com/").as_deref(),
            Some("https://cloud.leanctx.com/o/acme-platform/stop/")
        );
    }

    #[test]
    fn a_running_team_names_no_stopper_and_no_release_link() {
        let body = json!({"team": "acme", "stopped": false, "since": null, "by": null,
            "reason": null, "generation": 4, "role": "contributor", "may_stop": true,
            "may_release": false, "company": {"admin": true}});
        let stop = decode(&body).unwrap();
        assert!(!stop.stopped && stop.by.is_none() && stop.reason.is_none());
        assert_eq!(stop.company_admin, Some(true));
        assert_eq!(stop.release_link("https://cloud.leanctx.com"), None);
    }

    #[test]
    fn a_stopper_without_a_name_is_named_by_email() {
        let mut body = stopped();
        body["by"] = json!({"id": "8d0c", "name": null, "email": "ada@acme.test"});
        assert_eq!(decode(&body).unwrap().by.as_deref(), Some("ada@acme.test"));
        body["by"] = json!({"id": "8d0c", "name": "  ", "email": "ada@acme.test"});
        assert_eq!(decode(&body).unwrap().by.as_deref(), Some("ada@acme.test"));
    }

    #[test]
    fn a_broken_answer_is_refused_and_odd_members_left_out() {
        for bad in [
            json!({"stopped": true}),
            json!({"team": "Acme Co", "stopped": true}),
            json!({"team": "acme", "stopped": "yes"}),
        ] {
            assert!(
                matches!(decode(&bad), Err(CloudError::Protocol(_))),
                "{bad}"
            );
        }
        let mut body = stopped();
        body["role"] = json!("Admin<b>");
        body["since"] = json!("2026-11-19 10:00");
        body["may_release"] = json!("true");
        body["step_up_url"] = json!("javascript:alert(1)");
        let stop = decode(&body).unwrap();
        assert_eq!(
            (stop.role, stop.since, stop.may_release, stop.step_up_url),
            (None, None, false, None)
        );
        body["step_up_url"] = json!("https://cloud.leanctx.com/o/acme-platform/stop/");
        assert!(decode(&body).unwrap().step_up_url.is_some());
    }

    #[test]
    fn links_stay_below_their_origin() {
        let origin = "https://cloud.leanctx.com";
        assert!(below_origin("https://cloud.leanctx.com/o/a/stop/", origin));
        for bad in [
            "https://cloud.leanctx.com.evil.example/o/a/stop/",
            "https://evil.example/o/a/stop/",
            "https://cloud.leanctx.com/o/a b/",
            "https://cloud.leanctx.com",
        ] {
            assert!(!below_origin(bad, origin), "{bad}");
        }
    }

    #[test]
    fn an_older_cloud_offers_no_stop_and_a_refusal_is_an_error() {
        assert_eq!(classify(404, &json!({})).unwrap(), None);
        assert_eq!(
            classify(403, &json!({"error": {"code": "forbidden"}})).unwrap_err(),
            CloudError::Forbidden
        );
        assert!(classify(200, &stopped()).unwrap().unwrap().stopped);
    }
}
