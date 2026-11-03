// SPDX-License-Identifier: Apache-2.0
//! Typed agent messages on a C0 2.0 Hive (bus v2, E14): `ctx_agent send` and
//! `reply` become `place.publish` frames to the vault bus of the checkout.
//!
//! - A message for one agent names its worker in `body.to_worker` (`wkr_…`,
//!   as `ctx_agent list` shows the workers of other machines); the vault's
//!   present agents all receive it, the addressed one sees it is for them.
//! - A reply names the message it answers (`reply_to`, its `msg_…` id); Hive
//!   keeps the thread (`thread_id`) and its state (answered, accepted, …).
//! - A handoff offers a resource to one worker; the addressee answers it
//!   with `accept` true or false.
//!
//! Agents talk to agents here: Hive carries no person answers (owner,
//! 01.11.). No network in the tool call: the publish is queued and the
//! relay sends it ([`super::relay_v2::queue_publish`]).

use serde_json::{Map, Value};

use super::v2::{BUS_SCHEMA, MessageType, Publish, Ref, RefKind, ulid_for};
use crate::HiveStore;
use crate::knowledge::screening::Screen;
use crate::vault::VaultMap;

/// What a typed message carries beyond its text.
#[derive(Debug, Clone, Copy, Default)]
pub struct Extras<'a> {
    /// The addressed worker (`wkr_…`).
    pub to_worker: Option<&'a str>,
    /// The `msg_…` id answered.
    pub reply_to: Option<&'a str>,
    /// An answer to a handoff: take it (`true`) or decline it.
    pub accept: Option<bool>,
    /// A handoff's resource (a path, `task:<n>`, …); the text otherwise.
    pub resource: Option<&'a str>,
    /// `file:<path>`, `commit:<sha>`, `node:<id>`, `msg:<id>`, `url:<url>`,
    /// `test:<name>`: what the message rests on (a finding needs one).
    pub refs: &'a [String],
}

/// The bus v2 refs among `raw` (`kind:target`); other entries are ignored.
pub fn refs_of(raw: &[String]) -> Vec<Ref> {
    raw.iter()
        .filter_map(|r| {
            let (kind, target) = r.split_once(':')?;
            let kind = match kind {
                "file" => RefKind::File,
                "commit" => RefKind::Commit,
                "node" => RefKind::Node,
                "msg" => RefKind::Msg,
                "url" => RefKind::Url,
                "test" => RefKind::Test,
                _ => return None,
            };
            (!target.is_empty()).then(|| Ref {
                kind,
                target: target.to_owned(),
            })
        })
        .collect()
}

/// The body of a `kind` message with `text`, or why it cannot be sent.
pub fn body(
    kind: MessageType,
    text: &str,
    from_worker: &str,
    x: Extras<'_>,
) -> Result<Value, String> {
    let to_worker = x.to_worker.map(str::to_owned);
    if let Some(w) = &to_worker {
        if !w.starts_with("wkr_") {
            return Err(format!(
                "address another machine's agent by its worker id (wkr_…), as ctx_agent action=list shows it; {w:?} is not one"
            ));
        }
    }
    let mut b = Map::new();
    match kind {
        MessageType::Question => {
            b.insert("text".into(), text.into());
            if let Some(w) = to_worker {
                b.insert("to_worker".into(), w.into());
            }
        }
        MessageType::Answer => {
            if x.reply_to.is_none() {
                return Err("an answer needs reply_to (the msg_… id it answers)".into());
            }
            b.insert("text".into(), text.into());
            if let Some(accept) = x.accept {
                b.insert("accept".into(), accept.into());
            }
        }
        MessageType::Handoff => {
            // The sender is the publishing worker; Hive knows it.
            let to = to_worker.ok_or("a handoff needs to_agent (the worker that takes over)")?;
            if to == from_worker {
                return Err("a handoff goes to another worker".into());
            }
            b.insert("to_worker".into(), to.into());
            b.insert("resource".into(), x.resource.unwrap_or(text).into());
            b.insert("note".into(), text.into());
        }
        MessageType::Finding => {
            if refs_of(x.refs).is_empty() {
                return Err(
                    "a finding needs references (file:<path>, commit:<sha>, node:<id>, …)".into(),
                );
            }
            if !(24..=300).contains(&text.chars().count()) {
                return Err("a finding is one statement of 24 to 300 characters".into());
            }
            b.insert("statement".into(), text.into());
            b.insert("evidence".into(), "stated".into());
        }
        MessageType::Offer => {
            b.insert("what".into(), text.into());
            b.insert("capability".into(), "general".into());
            b.insert("eta_minutes".into(), 60.into());
        }
        MessageType::Progress => {
            return Err(
                "progress is a plain status post (ctx_agent post); send question|offer|handoff|answer|finding"
                    .into(),
            );
        }
    }
    Ok(Value::Object(b))
}

/// The worker a local agent speaks as: its host app and role, attached to
/// this instance's book (local; the relay registers it with Hive).
fn worker_of(store: &HiveStore, agent_id: &str) -> Result<String, String> {
    let (instance_id, host, role) = host_and_role(store, agent_id)?;
    crate::workers::attach::attach(store, &instance_id, &host, &role)
        .map(|a| a.worker.worker_id)
        .map_err(|e| e.to_string())
}

/// The worker `agent_id` speaks as, without adding it to the book: tool
/// admission reads which worker a Hive control names (`None` when this
/// machine is not enrolled or the agent is unknown).
pub(crate) fn known_worker_of(store: &HiveStore, agent_id: &str) -> Option<String> {
    use crate::workers::attach;
    let (instance_id, host, role) = host_and_role(store, agent_id).ok()?;
    let mut workers = attach::list(store, &instance_id).ok()?;
    Some(
        attach::attach_in(&mut workers, &instance_id, &host, &role)
            .worker
            .worker_id,
    )
}

/// The enrolled instance and the host app and role `agent_id` attaches with.
fn host_and_role(store: &HiveStore, agent_id: &str) -> Result<(String, String, String), String> {
    use crate::workers::attach;
    use lean_ctx::core::agents::AgentRegistry;
    let instance = store
        .instance()
        .map_err(|e| e.to_string())?
        .ok_or("this machine is not connected to Hive")?;
    let registry = AgentRegistry::load_or_create();
    let agent = registry
        .agents
        .iter()
        .find(|a| a.agent_id == agent_id)
        .ok_or("agent must be registered first (use action=register)")?;
    let transport = agent.transport.as_ref();
    let host = attach::detect_host_app(
        transport.map(|t| t.host.as_str()),
        Some(agent.agent_type.as_str()),
        std::iter::empty::<(&str, &str)>(),
        transport.and_then(|t| t.name.as_deref()),
    );
    let role = agent.role.clone().unwrap_or_default();
    Ok((instance.instance_id, host, role))
}

/// Queue a typed message from `agent_id` in `project_root`; its
/// `client_msg_id`.
pub fn send(
    project_root: &str,
    agent_id: &str,
    message_type: Option<&str>,
    text: &str,
    extras: Extras<'_>,
) -> Result<String, String> {
    let store = HiveStore::open().map_err(|e| e.to_string())?;
    if let Some(refusal) = super::control::admission(&store, Some(agent_id)) {
        return Err(refusal.message());
    }
    let raw = message_type.ok_or("type is required (question|offer|handoff|answer|finding)")?;
    let kind = MessageType::parse(&raw.trim().to_ascii_lowercase()).ok_or_else(|| {
        format!("unknown type {raw:?}; use question|offer|handoff|answer|finding")
    })?;
    let map = VaultMap::load(store.data_dir());
    let record = map.for_root(project_root).ok_or(
        "this repository is not linked to a vault (`lean-ctx hive link <workspace>/<vault>`)",
    )?;
    Screen::for_installation()
        .check(text)
        .map_err(|blocked| format!("not sent: the text contains a {blocked}"))?;
    let worker_id = worker_of(&store, agent_id)?;
    let body = body(kind, text, &worker_id, extras)?;
    let now_ms = chrono::Utc::now().timestamp_millis().unsigned_abs();
    let publish = Publish {
        schema: BUS_SCHEMA.into(),
        client_msg_id: ulid_for(
            now_ms,
            &format!("{agent_id}:{}", uuid::Uuid::new_v4().simple()),
        ),
        to: super::relay_v2::address_of(record),
        kind,
        worker_id,
        thread_id: None,
        reply_to: extras.reply_to.map(str::to_owned),
        refs: refs_of(extras.refs),
        body,
        ck_wrapped: None,
        epoch: None,
        about: None,
        counts: None,
        window: None,
    };
    let id = publish.client_msg_id.clone();
    // A handoff is remembered with its offering agent, so a maintainer's
    // veto of it reaches that agent ([`super::handoff`]).
    if kind == MessageType::Handoff
        && let Some(to) = extras.to_worker
    {
        super::handoff::offered(store.data_dir(), &id, agent_id, to)
            .map_err(|e| format!("not sent: {e}"))?;
    }
    super::relay_v2::queue_publish(&store, publish).map_err(|e| format!("not sent: {e}"))?;
    Ok(id)
}

/// How a delivered typed message reads on the local bus: its type and id
/// (to reply to), thread and state, who it is for, then the text.
pub fn delivered_line(message: &Value) -> Option<String> {
    let kind = message["type"].as_str()?;
    if kind == "progress" {
        return None;
    }
    let body = &message["body"];
    let mut head = format!("{kind} {}", message["id"].as_str().unwrap_or("?"));
    if let Some(thread) = message["thread_id"]
        .as_str()
        .filter(|t| Some(*t) != message["id"].as_str())
    {
        head.push_str(&format!(" in thread {thread}"));
    }
    if let Some(status) = message["thread_status"].as_str() {
        head.push_str(&format!(" [{status}]"));
    }
    if let Some(to) = body["to_worker"].as_str() {
        head.push_str(&format!(" for {to}"));
    }
    if let Some(accept) = body["accept"].as_bool() {
        head.push_str(if accept { " (accepted)" } else { " (declined)" });
    }
    let text = match kind {
        "handoff" => format!(
            "hands over {} to {}: {}",
            body["resource"].as_str().unwrap_or("?"),
            body["to_worker"].as_str().unwrap_or("?"),
            body["note"].as_str().unwrap_or_default()
        ),
        "finding" => body["statement"].as_str().unwrap_or_default().to_owned(),
        "offer" => body["what"].as_str().unwrap_or_default().to_owned(),
        _ => body["text"]
            .as_str()
            .map_or_else(|| body.to_string(), str::to_owned),
    };
    Some(format!(
        "{head} — reply with ctx_agent action=reply reply_to={}: {text}",
        message["id"].as_str().unwrap_or("?")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ME: &str = "wkr_aaaaaaaaaaaaaaaaaaaa";
    const YOU: &str = "wkr_bbbbbbbbbbbbbbbbbbbb";

    fn x<'a>() -> Extras<'a> {
        Extras::default()
    }

    #[test]
    fn bodies_follow_the_bus_v2_schema() {
        let question = body(
            MessageType::Question,
            "Retry budget?",
            ME,
            Extras {
                to_worker: Some(YOU),
                ..x()
            },
        )
        .unwrap();
        assert_eq!(question, json!({"text": "Retry budget?", "to_worker": YOU}));
        let answer = body(
            MessageType::Answer,
            "Yes.",
            ME,
            Extras {
                reply_to: Some("msg_1"),
                accept: Some(true),
                ..x()
            },
        )
        .unwrap();
        assert_eq!(answer, json!({"text": "Yes.", "accept": true}));
        let handoff = body(
            MessageType::Handoff,
            "Take the tests.",
            ME,
            Extras {
                to_worker: Some(YOU),
                resource: Some("tests/auth"),
                ..x()
            },
        )
        .unwrap();
        assert_eq!(
            handoff,
            json!({"to_worker": YOU, "resource": "tests/auth", "note": "Take the tests."})
        );
        let cites = ["file:src/retry.rs".to_owned(), "tests/auth".to_owned()];
        assert_eq!(
            refs_of(&cites).len(),
            1,
            "only kind:target entries are refs"
        );
        assert!(
            body(MessageType::Finding, "The cap is 3.", ME, x()).is_err(),
            "a finding needs a ref"
        );
        let finding = body(
            MessageType::Finding,
            "The retry cap is three attempts in retry().",
            ME,
            Extras {
                refs: &cites,
                ..x()
            },
        )
        .unwrap();
        assert_eq!(finding["evidence"], "stated");
        let offer = body(MessageType::Offer, "I can review retry().", ME, x()).unwrap();
        // Every body passes the engine's own bus v2 check before it is queued.
        for (kind, message_body, reply_to) in [
            (MessageType::Question, question, None),
            (MessageType::Answer, answer, Some("msg_00000000000000a1")),
            (MessageType::Handoff, handoff, None),
            (MessageType::Finding, finding, None),
            (MessageType::Offer, offer, None),
        ] {
            let publish = Publish {
                schema: BUS_SCHEMA.into(),
                client_msg_id: "01M5NKA2Z8B000000000000009".into(),
                to: "#acme/payments".into(),
                kind,
                worker_id: ME.into(),
                thread_id: None,
                reply_to: reply_to.map(Into::into),
                refs: if kind == MessageType::Finding {
                    refs_of(&cites)
                } else {
                    Vec::new()
                },
                body: message_body,
                ck_wrapped: None,
                epoch: None,
                about: None,
                counts: None,
                window: None,
            };
            assert!(
                publish.validate().is_ok(),
                "{kind:?}: {:?}",
                publish.validate()
            );
        }
    }

    #[test]
    fn refusals_name_what_is_missing() {
        assert!(
            body(MessageType::Answer, "x", ME, x())
                .unwrap_err()
                .contains("reply_to")
        );
        assert!(
            body(MessageType::Handoff, "x", ME, x())
                .unwrap_err()
                .contains("to_agent")
        );
        let local = body(
            MessageType::Question,
            "x",
            ME,
            Extras {
                to_worker: Some("mcp-1-abc"),
                ..x()
            },
        )
        .unwrap_err();
        assert!(local.contains("worker id"), "{local}");
        assert!(body(MessageType::Progress, "x", ME, x()).is_err());
    }

    #[test]
    fn a_delivery_shows_how_to_reply() {
        let m = json!({
            "id": "msg_2", "type": "answer", "thread_id": "msg_1", "thread_status": "answered",
            "body": {"text": "Three attempts."}
        });
        let line = delivered_line(&m).unwrap();
        assert!(
            line.starts_with("answer msg_2 in thread msg_1 [answered]"),
            "{line}"
        );
        assert!(line.contains("reply_to=msg_2") && line.ends_with("Three attempts."));
        let h = json!({"id": "msg_3", "type": "handoff", "body": {"to_worker": YOU, "resource": "tests/auth", "note": "please"}});
        assert!(
            delivered_line(&h)
                .unwrap()
                .contains("hands over tests/auth to")
        );
        assert!(delivered_line(&json!({"type": "progress", "body": {"step": "x"}})).is_none());
    }
}
