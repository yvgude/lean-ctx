// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx cloud cpao --json`: the team's cost per accepted outcome over
//! the last 7 and 30 days (`GET /v1/cpao`), for a local reader such as
//! LeanCTX Code. Asked only when the server announced the `cpao`
//! capability; otherwise, and when the cloud answers that it does not offer
//! it, the document says `available: false`. When the server does not
//! answer, the last answer of that server is returned with `live: false`
//! and its `as_of`. A refusal (`403`) is a `forbidden` error document.

use lean_ctx_time::UtcDateTime;
use serde::Serialize;

use lean_ctx_cloud::config::CloudConfig;
use lean_ctx_cloud::cpao::Refusal;
use lean_ctx_cloud::cpao::{CloudCpaoV1, CpaoRead};
use lean_ctx_cloud::{CloudError, CloudStore};

use super::Failure;

/// The local-read kind of this document.
pub(crate) const KIND: &str = "cloud.cpao";

#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct CpaoDoc {
    /// `false`: the server does not offer CPAO (not configured or older).
    pub available: bool,
    /// The cloud's answer (schema `leanctx.cloud.cpao/v1`); `null` when not available.
    pub cpao: Option<CloudCpaoV1>,
    /// When `cpao` was read (RFC 3339).
    pub as_of: Option<String>,
    /// Read just now; `false` = the last answer (the server did not answer).
    pub live: bool,
}

pub(crate) fn build(read: Option<CpaoRead>) -> CpaoDoc {
    match read {
        Some(r) => CpaoDoc {
            available: true,
            cpao: Some(r.cpao),
            as_of: r.as_of,
            live: r.live,
        },
        None => CpaoDoc {
            available: false,
            cpao: None,
            as_of: None,
            live: false,
        },
    }
}

pub(crate) fn run(config: &CloudConfig) {
    super::emit(KIND, || {
        let store = CloudStore::open().map_err(Failure::from)?;
        let read = lean_ctx_cloud::cpao::read(&store, config, UtcDateTime::now())
            .map_err(Failure::from)?;
        Ok(build(read))
    });
}

/// Which account this machine is connected to, as far as the human output
/// needs it (from the local enrolment; no request).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Account {
    NotConnected,
    Personal,
    /// A team or anything else: the cloud decides.
    Other,
}

impl Account {
    fn of(instance: Option<&lean_ctx_cloud::state::InstanceRecord>) -> Self {
        match instance {
            None => Self::NotConnected,
            Some(i) if i.org_kind.as_deref() == Some("personal") => Self::Personal,
            Some(_) => Self::Other,
        }
    }

    /// From reading the enrolment. An enrolment that cannot be read leaves
    /// the decision to the cloud read: a server that did not announce
    /// `cpao` still reads as "not offered", and only a read that needs the
    /// enrolment reports the error.
    fn of_read(read: Result<Option<lean_ctx_cloud::state::InstanceRecord>, CloudError>) -> Self {
        match read {
            Ok(instance) => Self::of(instance.as_ref()),
            Err(_) => Self::Other,
        }
    }
}

/// Printed when the cloud refuses (`403`) because the person's role in the
/// team may not see the team's cost.
pub(crate) const REFUSED: &str =
    "cost per accepted outcome: only owners and admins of the team see CPAO";

/// Printed when the cloud refuses (`403`) without a usable message.
pub(crate) const REFUSED_PLAIN: &str = "cost per accepted outcome: the cloud refused the request";

/// What the cloud said: the read, or which refusal it was.
type Answer = Result<Option<CpaoRead>, Refusal>;

/// `lean-ctx cloud cpao` for people: the same read, as lines.
pub(crate) fn run_human(config: &CloudConfig) -> Result<(), CloudError> {
    let store = CloudStore::open()?;
    let account = Account::of_read(store.instance());
    let text = human(account, || {
        lean_ctx_cloud::cpao::answer(&store, config, UtcDateTime::now())
    })?;
    print!("{text}");
    Ok(())
}

/// The lines for `account`. Only a team (or an account the enrolment does
/// not name) asks the cloud: without a connection, and for a Personal
/// account, `ask` is never called, so no request is sent.
fn human(
    account: Account,
    ask: impl FnOnce() -> Result<Answer, CloudError>,
) -> Result<String, CloudError> {
    if account != Account::Other {
        return Ok(render(&build(None), account));
    }
    let read = ask()?.map_err(human_error)?;
    Ok(render(&build(read), account))
}

/// A role refusal names the role as the reason; any other refusal keeps the
/// server's message (or the plain refusal when it sent none).
fn human_error(refusal: Refusal) -> CloudError {
    match refusal {
        Refusal::Role => CloudError::Refused(REFUSED.to_string()),
        Refusal::Other(Some(message)) => CloudError::Refused(message),
        Refusal::Other(None) => CloudError::Refused(REFUSED_PLAIN.to_string()),
    }
}

/// Millionths of a US dollar as `$12.50`.
fn usd(micros: u64) -> String {
    let cents = micros.saturating_add(5_000) / 10_000;
    format!("${}.{:02}", cents / 100, cents % 100)
}

/// A change in percent with one decimal and a sign; zero (also `-0.0`
/// after rounding) is `0.0%`.
fn percent(trend: f64) -> String {
    let text = format!("{trend:+.1}");
    match text.as_str() {
        "+0.0" | "-0.0" => "0.0%".to_string(),
        _ => format!("{text}%"),
    }
}

/// The document as lines. A period the cloud has not settled is marked
/// `(provisional)`; one without a `settled` from the cloud `(status unknown)`.
pub(crate) fn render(doc: &CpaoDoc, account: Account) -> String {
    let Some(cpao) = doc.cpao.as_ref().filter(|_| doc.available) else {
        return match account {
            Account::NotConnected => {
                "Cost per accepted outcome: not connected to LeanCTX Cloud (run `lean-ctx cloud connect`)\n"
            }
            Account::Personal => {
                "Cost per accepted outcome: shown for teams only; this machine is connected to a Personal account\n"
            }
            Account::Other => "Cost per accepted outcome: not offered by this server\n",
        }
        .to_string();
    };
    let mut out = String::from("Cost per accepted outcome");
    if !doc.live {
        out.push_str(" (last answer");
        if let Some(at) = &doc.as_of {
            out.push_str(", as of ");
            out.push_str(at);
        }
        out.push(')');
    }
    out.push('\n');
    for period in &cpao.periods {
        let per = period
            .cpao_usd_micros
            .map_or_else(|| "-".to_string(), |m| format!("{} per outcome", usd(m)));
        out.push_str(&format!(
            "  {:<4} {per}, {} accepted, {} total",
            period.period,
            period.accepted_outcomes,
            usd(period.cost_usd_micros)
        ));
        if let Some(trend) = period.trend_pct {
            out.push_str(&format!(
                ", change vs previous {}: {}",
                period.period,
                percent(trend)
            ));
        }
        match period.settled {
            Some(true) => {}
            Some(false) => out.push_str(" (provisional)"),
            None => out.push_str(" (status unknown)"),
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use lean_ctx_cloud::cpao::{CpaoAgentV1, CpaoPeriodV1};

    fn cpao() -> CloudCpaoV1 {
        CloudCpaoV1 {
            schema: lean_ctx_cloud::cpao::CPAO_SCHEMA.into(),
            periods: vec![CpaoPeriodV1 {
                period: "7d".into(),
                cost_usd_micros: 12_500_000,
                accepted_outcomes: 5,
                cpao_usd_micros: Some(2_500_000),
                trend_pct: Some(-12.5),
                settled: Some(false),
                by_agent: Some(vec![
                    CpaoAgentV1 {
                        agent: "claude".into(),
                        cost_usd_micros: Some(12_500_000),
                        accepted_outcomes: 4,
                    },
                    CpaoAgentV1 {
                        agent: "codex".into(),
                        cost_usd_micros: None,
                        accepted_outcomes: 1,
                    },
                ]),
            }],
        }
    }

    #[test]
    fn a_server_without_cpao_reads_as_not_available() {
        let doc = serde_json::to_value(build(None)).expect("json");
        assert_eq!(doc["available"], false);
        assert!(doc["cpao"].is_null());
        assert_eq!(doc["live"], false);
    }

    #[test]
    fn the_answer_is_carried_with_its_time_and_liveness() {
        let doc = serde_json::to_value(build(Some(CpaoRead {
            cpao: cpao(),
            as_of: Some("2026-11-18T12:00:00Z".into()),
            live: true,
        })))
        .expect("json");
        assert_eq!(doc["available"], true);
        assert_eq!(doc["cpao"]["schema"], "leanctx.cloud.cpao/v1");
        assert_eq!(doc["cpao"]["periods"][0]["period"], "7d");
        assert_eq!(doc["cpao"]["periods"][0]["cpao_usd_micros"], 2_500_000);
        assert_eq!(doc["cpao"]["periods"][0]["trend_pct"], -12.5);
        assert_eq!(doc["cpao"]["periods"][0]["by_agent"][0]["agent"], "claude");
        let agents = &doc["cpao"]["periods"][0]["by_agent"];
        assert_eq!(agents[0]["cost_usd_micros"], 12_500_000);
        // an agent the cloud did not price prints `null`, never `0`
        assert_eq!(agents[1]["agent"], "codex");
        assert!(agents[1]["cost_usd_micros"].is_null());
        assert!(agents[1].get("cost_usd_micros").is_some());
        assert_eq!(doc["as_of"], "2026-11-18T12:00:00Z");
        assert_eq!(doc["live"], true);
    }

    const TEAM: Account = Account::Other;

    #[test]
    fn settled_is_passed_on_and_each_state_is_marked() {
        let doc = |settled: Option<bool>| {
            let mut cpao = cpao();
            cpao.periods[0].settled = settled;
            build(Some(CpaoRead {
                cpao,
                as_of: Some("2026-11-18T12:00:00Z".into()),
                live: true,
            }))
        };
        let json =
            |d: &CpaoDoc| serde_json::to_value(d).expect("json")["cpao"]["periods"][0].clone();
        let line = "Cost per accepted outcome\n  7d   $2.50 per outcome, 5 accepted, $12.50 total, change vs previous 7d: -12.5%";

        let open = doc(Some(false));
        assert_eq!(json(&open)["settled"], false);
        assert_eq!(render(&open, TEAM), format!("{line} (provisional)\n"));

        let settled = doc(Some(true));
        assert_eq!(json(&settled)["settled"], true);
        assert_eq!(render(&settled, TEAM), format!("{line}\n"));

        // an older cloud does not say: left out of the JSON, marked unknown
        let unknown = doc(None);
        assert!(json(&unknown).get("settled").is_none());
        assert_eq!(render(&unknown, TEAM), format!("{line} (status unknown)\n"));
    }

    #[test]
    fn without_an_answer_each_account_gets_its_own_message() {
        let none = build(None);
        assert_eq!(
            render(&none, Account::NotConnected),
            "Cost per accepted outcome: not connected to LeanCTX Cloud (run `lean-ctx cloud connect`)\n"
        );
        assert_eq!(
            render(&none, Account::Personal),
            "Cost per accepted outcome: shown for teams only; this machine is connected to a Personal account\n"
        );
        assert_eq!(
            render(&none, TEAM),
            "Cost per accepted outcome: not offered by this server\n"
        );
    }

    #[test]
    fn the_account_comes_from_the_enrolment() {
        let record = |kind: Option<&str>| {
            serde_json::from_value::<lean_ctx_cloud::state::InstanceRecord>(serde_json::json!({
                "instance_id": "inst_1", "org_id": "org_1", "org_name": "Acme",
                "label": "mac", "url": "https://cloud.example", "org_salt_id": "s",
                "enrolled_at": "2026-11-18T12:00:00Z", "org_kind": kind
            }))
            .expect("record")
        };
        assert_eq!(Account::of(None), Account::NotConnected);
        assert_eq!(
            Account::of(Some(&record(Some("personal")))),
            Account::Personal
        );
        assert_eq!(Account::of(Some(&record(Some("team")))), Account::Other);
        assert_eq!(Account::of(Some(&record(None))), Account::Other);
        assert_eq!(Account::of_read(Ok(None)), Account::NotConnected);
        assert_eq!(
            Account::of_read(Ok(Some(record(Some("personal"))))),
            Account::Personal
        );
    }

    #[test]
    fn an_unreadable_enrolment_leaves_not_offered_to_the_cloud_read() {
        let broken = Err(CloudError::Refused("enrolment file: invalid JSON".into()));
        let account = Account::of_read(broken);
        assert_eq!(account, Account::Other);
        // a server that did not announce `cpao` answers before the enrolment is read
        assert_eq!(
            human(account, || Ok(Ok(None))).expect("text"),
            "Cost per accepted outcome: not offered by this server\n"
        );
    }

    #[test]
    fn human_output_names_a_last_answer() {
        let mut cpao = cpao();
        cpao.periods[0].cpao_usd_micros = None;
        cpao.periods[0].accepted_outcomes = 0;
        let text = render(
            &build(Some(CpaoRead {
                cpao,
                as_of: Some("2026-11-18T12:00:00Z".into()),
                live: false,
            })),
            TEAM,
        );
        assert!(
            text.starts_with(
                "Cost per accepted outcome (last answer, as of 2026-11-18T12:00:00Z)\n"
            )
        );
        assert!(text.contains("  7d   -, 0 accepted"));
    }

    #[test]
    fn amounts_round_to_the_nearest_cent() {
        assert_eq!(usd(0), "$0.00");
        assert_eq!(usd(4_999), "$0.00");
        assert_eq!(usd(5_000), "$0.01");
        assert_eq!(usd(2_494_999), "$2.49");
        assert_eq!(usd(2_495_000), "$2.50");
        assert_eq!(usd(1_234_000_000), "$1234.00");
        // never overflows on an absurd cloud amount
        assert_eq!(usd(u64::MAX), "$18446744073709.55");
    }

    #[test]
    fn a_trend_never_prints_negative_zero() {
        assert_eq!(percent(3.0), "+3.0%");
        assert_eq!(percent(-12.5), "-12.5%");
        assert_eq!(percent(0.0), "0.0%");
        assert_eq!(percent(-0.0), "0.0%");
        assert_eq!(percent(-0.04), "0.0%");
        assert_eq!(percent(0.04), "0.0%");
        assert_eq!(percent(-0.05), "-0.1%");
    }

    #[test]
    fn human_output_is_one_line_per_period_in_the_clouds_order() {
        let mut cpao = cpao();
        cpao.periods.push(CpaoPeriodV1 {
            period: "30d".into(),
            cost_usd_micros: 40_000_000,
            accepted_outcomes: 16,
            cpao_usd_micros: Some(2_500_000),
            trend_pct: Some(-0.0),
            settled: Some(true),
            by_agent: None,
        });
        let doc = build(Some(CpaoRead {
            cpao,
            as_of: Some("2026-11-18T12:00:00Z".into()),
            live: true,
        }));
        let text = render(&doc, TEAM);
        // live: no time in the text, so the same answer prints the same bytes
        assert_eq!(
            text,
            "Cost per accepted outcome\n\
             \x20 7d   $2.50 per outcome, 5 accepted, $12.50 total, change vs previous 7d: -12.5% (provisional)\n\
             \x20 30d  $2.50 per outcome, 16 accepted, $40.00 total, change vs previous 30d: 0.0%\n"
        );
        assert_eq!(render(&doc, TEAM), text);
        // agent rows are for the JSON reader only
        assert!(!text.contains("claude"));
    }

    #[test]
    fn a_refusal_is_a_forbidden_error() {
        let failure = Failure::from(lean_ctx_cloud::CloudError::Forbidden);
        assert_eq!(failure.code.as_str(), "forbidden");
    }

    #[test]
    fn people_see_the_role_as_the_reason_of_a_role_refusal_only() {
        let text = human_error(Refusal::Role).to_string();
        assert_eq!(text, REFUSED);
        assert!(text.contains("owners and admins"));
        assert!(!text.contains("instance"));
        assert!(!text.contains("lease"));
        // any other refusal keeps the server's own words
        let other = human_error(Refusal::Other(Some(
            "sign in with SSO in the browser".into(),
        )));
        assert_eq!(other.to_string(), "sign in with SSO in the browser");
        // without words from the server, the plain refusal
        assert_eq!(human_error(Refusal::Other(None)).to_string(), REFUSED_PLAIN);
    }

    #[test]
    fn rendered_refusals_never_name_an_instance_or_a_lease() {
        let texts = [
            human_error(Refusal::Role).to_string(),
            human_error(Refusal::Other(None)).to_string(),
            human(TEAM, || Ok(Err(Refusal::Other(None))))
                .expect_err("refused")
                .to_string(),
        ];
        for text in texts {
            let lower = text.to_lowercase();
            assert!(!lower.contains("instance"), "{text}");
            assert!(!lower.contains("lease"), "{text}");
        }
    }

    #[test]
    fn a_personal_account_or_no_connection_sends_no_request() {
        let never = || -> Result<Answer, CloudError> { panic!("no request may be sent") };
        assert_eq!(
            human(Account::Personal, never).expect("text"),
            "Cost per accepted outcome: shown for teams only; this machine is connected to a Personal account\n"
        );
        assert_eq!(
            human(Account::NotConnected, never).expect("text"),
            render(&build(None), Account::NotConnected)
        );
    }

    #[test]
    fn a_team_asks_the_cloud_and_shows_its_refusal() {
        let mut asked = false;
        let text = human(TEAM, || {
            asked = true;
            Ok(Ok(None))
        })
        .expect("text");
        assert!(asked);
        assert_eq!(
            text,
            "Cost per accepted outcome: not offered by this server\n"
        );
        let refused = human(TEAM, || Ok(Err(Refusal::Role))).expect_err("refused");
        assert_eq!(refused.to_string(), REFUSED);
        // an error of the read itself is passed on as it is
        assert!(matches!(
            human(TEAM, || Err(CloudError::NotConnected)),
            Err(CloudError::NotConnected)
        ));
    }

    #[test]
    fn cloud_cpao_is_listed_in_capabilities() {
        assert!(super::super::capabilities::KINDS.contains(&KIND));
        assert!(
            super::super::capabilities::COMMANDS
                .iter()
                .any(|(group, verbs)| *group == "cloud" && verbs.contains(&"cpao"))
        );
    }
}
