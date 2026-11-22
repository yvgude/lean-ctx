// SPDX-License-Identifier: Apache-2.0
//! `cloud.plan.updates` (contract §37.6): the plan versions this instance
//! applied from LeanCTX Cloud, with each local agent's answer, for a local
//! reader such as LeanCTX Code. Read from local files only; nothing is
//! sent to Cloud.
//!
//! - `GET /v1/local/cloud/plan/updates[?project_root=P]`, CLI
//!   `lean-ctx cloud plan updates --json [--project-root P]`. With a
//!   project, only the plans of its vault. Only the CLI outside an agent
//!   (LeanCTX Code's read) shows an offered directive's `release_id` and
//!   the plan text of the verified body: the socket route has no caller
//!   authentication and always gives `null` for both.
//! - `lean-ctx cloud plan release` with the release id and the confirmed
//!   body on stdin (never in argv, which other processes of the user see):
//!   the person's "Send to agents" in LeanCTX Code, for one directive and
//!   one agent. CLI only: no route, no MCP tool, no `ctx_agent` action, no
//!   bus or Cloud frame reaches it.
//! - `lean-ctx cloud plan publish` with the request on stdin: LeanCTX
//!   Code publishes a plan version (§37.10); the engine serializes the body,
//!   digests it and posts it to the vault the working directory is bound to.
//!
//! All three commands refuse when an agent marker is set
//! (`lean_ctx::shell_hook::agent_env_markers`): `updates` with `forbidden`,
//! `publish` and `release` with `{"ok":false,"code":"agent_refused"}`; the
//! agent shell policy refuses them too. The ids
//! themselves are kept in a secret path no agent read tool opens
//! (`core::agents::RELEASE_IDS_FILE`).
//!
//! Wired as one `local_read::Part`: its routes, kinds and commands are
//! chained into the route table and the capabilities builder.

use std::collections::BTreeMap;
use std::io::{IsTerminal as _, Read as _};
use std::path::Path;

use serde::Serialize;

use lean_ctx::core::agents::{DirectiveStore, ReleaseBody, is_release_id};
use lean_ctx_cloud::CloudStore;
use lean_ctx_cloud::bus::plan_body;
use lean_ctx_cloud::bus::plan_view::{self, PlanUpdate};

use super::routes::{Method, Param, Project, Route, Shape};
use super::{Answer, Code, Failure, Via};

/// The kind of the plan updates' document.
pub(crate) const KIND: &str = "cloud.plan.updates";

/// This part's routes (Route as in `routes.rs`).
pub(crate) const ROUTES: &[Route] = &[Route {
    kind: KIND,
    method: Method::Get,
    path: "/v1/local/cloud/plan/updates",
    cli: &["cloud", "plan", "updates", "--json"],
    action: &["updates"],
    params: &[Param {
        name: "project_root",
        flag: "--project-root",
        shape: Shape::Value,
    }],
    project: Project::Optional,
    names: None,
}];

/// This part's document kinds.
pub(crate) const KINDS: &[&str] = &[KIND];

/// Command path → CLI verbs, as `capabilities::COMMANDS`.
pub(crate) const COMMANDS: &[(&str, &[&str])] =
    &[("cloud.plan", &["updates", "publish", "release"])];

/// The applied plan versions.
#[derive(Debug, Serialize)]
pub(crate) struct PlanUpdates {
    /// The project named, `None` for every vault of this instance.
    pub project_root: Option<String>,
    pub plans: Vec<PlanUpdate>,
    /// Every offered directive, one per agent and version: what Code
    /// confirms and releases (§37.6).
    pub offers: Vec<plan_view::Offer>,
}

/// The project's enrolled vault id when one is named.
fn vault_of(data_dir: &Path, root: Option<&str>) -> Result<Option<String>, Failure> {
    let Some(root) = root else {
        return Ok(None);
    };
    let record = lean_ctx_cloud::resolution::vault_for_root(data_dir, Path::new(root))?
        .ok_or_else(|| {
            Failure::new(
                Code::NoVault,
                "this project is not linked to a LeanCTX Cloud vault".to_owned(),
            )
        })?;
    Ok(Some(record.vault_id))
}

/// The agent markers are the engine's one list
/// (`lean_ctx::shell_hook::agent_env_markers`): with one set, the plan
/// commands refuse; release ids are for LeanCTX Code, never for an agent.
fn env_agent_marker() -> Option<&'static str> {
    lean_ctx::shell_hook::env_agent_marker()
}

fn agent_refusal(marker: &str) -> String {
    format!(
        "release ids are for LeanCTX Code only; this command runs inside an agent ({marker} is set)"
    )
}

/// The answer of `cloud plan publish` and `cloud plan release` inside an
/// agent: no detail on which marker matched (§37.9, §37.10).
fn agent_refused() -> (serde_json::Value, i32) {
    (serde_json::json!({"ok": false, "code": "agent_refused"}), 1)
}

/// Who reads: the app (the CLI outside an agent) gets the release ids,
/// every other reader the same document with `release_id: null`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reader {
    App,
    Other,
}

/// The reader of an answer `via`: the CLI outside an agent is the app.
fn reader_of(via: Via) -> Result<Reader, Failure> {
    match via {
        Via::Cli => match env_agent_marker() {
            Some(marker) => Err(Failure::new(Code::Forbidden, agent_refusal(marker))),
            None => Ok(Reader::App),
        },
        // The daemon socket authenticates no caller: an agent of the same
        // user reaches it, so it never shows a release id.
        Via::Socket => Ok(Reader::Other),
    }
}

fn read(root: Option<String>, reader: Reader) -> Result<PlanUpdates, Failure> {
    let store = CloudStore::open()?;
    let only = vault_of(store.data_dir(), root.as_deref())?;
    let addresses: BTreeMap<String, String> =
        lean_ctx_cloud::resolution::current_view(store.data_dir())
            .vaults
            .values()
            .map(|r| {
                (
                    r.vault_id.clone(),
                    lean_ctx_cloud::bus::relay_v2::address_of(r),
                )
            })
            .collect();
    let docs = plan_view::kept_plans(store.data_dir());
    let store = match reader {
        Reader::App => DirectiveStore::load_for_app(),
        Reader::Other => DirectiveStore::load(),
    };
    let plans = plan_view::updates(
        &docs,
        &store,
        &addresses,
        only.as_deref(),
        reader == Reader::App,
    );
    let offers = plan_view::offers(&plans);
    Ok(PlanUpdates {
        project_root: root,
        plans,
        offers,
    })
}

/// The reader's optional `--project-root`, checked.
fn root_arg(args: &[String]) -> Result<Option<String>, Failure> {
    super::flag(args, "--project-root")?
        .filter(|raw| !raw.is_empty())
        .map(super::routes::project_dir)
        .transpose()
}

/// The released document of `kind`, for the CLI or the socket.
pub(crate) fn answer(kind: &str, args: &[String], via: Via) -> Answer {
    if kind != KIND {
        return Answer::failure(&Failure::invalid(format!("unknown kind {kind}")));
    }
    let root = match root_arg(args) {
        Ok(root) => root,
        Err(failure) => return Answer::failure(&failure),
    };
    let reader = match reader_of(via) {
        Ok(reader) => reader,
        Err(failure) => return Answer::failure(&failure),
    };
    let tool_root = root.clone().unwrap_or_default();
    super::answer_released("ctx_agent", &tool_root, kind, via, || read(root, reader))
}

/// Bytes of the release request on stdin.
const MAX_BODY_BYTES: u64 = 64 * 1024;
/// Bytes of the publish request on stdin: a plan body may have 256 KiB
/// (37.10), and Cloud takes a publish of up to 400 KiB.
const MAX_PUBLISH_BYTES: u64 = 400 * 1024;

/// The release request on stdin; empty when stdin is a terminal or empty.
fn stdin_body() -> Result<String, String> {
    stdin_text(MAX_BODY_BYTES)
}

/// The publish request on stdin; empty when stdin is a terminal or empty.
fn stdin_publish() -> Result<String, String> {
    stdin_text(MAX_PUBLISH_BYTES)
}

fn stdin_text(max: u64) -> Result<String, String> {
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        return Ok(String::new());
    }
    capped_text(stdin.lock(), max)
}

/// At most `max` bytes of UTF-8 text from `reader`; more is refused.
fn capped_text(reader: impl std::io::Read, max: u64) -> Result<String, String> {
    let mut text = String::new();
    reader
        .take(max + 1)
        .read_to_string(&mut text)
        .map_err(|e| format!("stdin: {e}"))?;
    if text.len() as u64 > max {
        return Err(format!("stdin is larger than {max} bytes"));
    }
    Ok(text)
}

/// The release id and the confirmed body of a stdin request
/// `{"release_id", "version", "goal", "brief"}` (a decision release:
/// `{"release_id"}` only). The id is checked first.
fn release_request(text: &str) -> Result<(String, Option<ReleaseBody>), String> {
    const EXPECTED: &str =
        "expected {\"release_id\",\"version\",\"goal\",\"brief\"} or {\"release_id\"}";
    if text.trim().is_empty() {
        return Err(format!(
            "the release request is read from stdin ({EXPECTED})"
        ));
    }
    let mut fields = match serde_json::from_str::<serde_json::Value>(text) {
        Ok(serde_json::Value::Object(fields)) => fields,
        Ok(_) => {
            return Err(format!(
                "the release request is not a JSON object ({EXPECTED})"
            ));
        }
        Err(e) => return Err(format!("release request: {e} ({EXPECTED})")),
    };
    let Some(serde_json::Value::String(rid)) = fields.remove("release_id") else {
        return Err(format!("release_id is required ({EXPECTED})"));
    };
    // A directive id (`<plan>:<v>:<slot>`) or any other guess is no release.
    if !is_release_id(&rid) {
        return Err(
            "release_id takes the release_id of `cloud plan updates --json` (64 lowercase hex)"
                .to_owned(),
        );
    }
    if fields.is_empty() {
        return Ok((rid, None));
    }
    serde_json::from_value::<ReleaseBody>(serde_json::Value::Object(fields))
        .map(|body| (rid, Some(body)))
        .map_err(|e| format!("release body: {e} ({EXPECTED})"))
}

/// The answer of `cloud plan release`: the JSON line and the exit status.
/// `marker` is the agent marker set, `stdin` the request (release id and
/// confirmed body).
fn release(
    args: &[String],
    marker: Option<&str>,
    stdin: impl FnOnce() -> Result<String, String>,
) -> (serde_json::Value, i32) {
    let refused = |code: &str, message: String| {
        (
            serde_json::json!({
                "ok": false,
                "released": false,
                "error": {"code": code, "message": message},
            }),
            if code == "invalid_argument" { 2 } else { 1 },
        )
    };
    if marker.is_some() {
        return agent_refused();
    }
    // No argument but `--json`: the id is never in argv (visible to other
    // processes of the user).
    if let Some(arg) = args.iter().find(|a| a.as_str() != "--json") {
        let message = if arg.starts_with("--release-id") {
            "--release-id moved to stdin: pass {\"release_id\",\"version\",\"goal\",\"brief\"} as one JSON object"
                .to_owned()
        } else {
            format!(
                "cloud plan release takes no argument but --json (got {arg}); the request is read from stdin"
            )
        };
        return refused("invalid_argument", message);
    }
    let (rid, body) = match stdin().and_then(|text| release_request(&text)) {
        Ok(request) => request,
        Err(e) => return refused("invalid_argument", e),
    };
    // Delivered is reported as delivered: a record write failing after the
    // bus entry is written is a warning, not an error (§37.6).
    match DirectiveStore::mutate_release(&rid, body.as_ref()) {
        Ok((done, warning)) => {
            let mut value = serde_json::json!({
                "ok": true,
                "released": done.released,
                "directive_id": done.directive_id,
                "agent_id": done.agent_id,
                "version": done.version,
                "released_at": done.released_at,
            });
            if let Some(warning) = warning {
                value["warning"] = serde_json::Value::String(warning);
            }
            (value, 0)
        }
        Err(error) => refused(error.code(), error.message()),
    }
}

/// The answer of `cloud plan publish`: the JSON line and the exit status.
/// `marker` is the agent marker set, `stdin` the request, `root` the
/// project of the working directory, `post` the Cloud call.
fn publish(
    args: &[String],
    marker: Option<&str>,
    stdin: impl FnOnce() -> Result<String, String>,
    root: impl FnOnce() -> Result<String, Failure>,
    post: impl FnOnce(
        &Path,
        &plan_body::Publish,
    ) -> Result<plan_body::Published, plan_body::PublishRefused>,
) -> (serde_json::Value, i32) {
    let refused = |code: &str, message: String| {
        (
            serde_json::json!({"ok": false, "code": code, "message": message}),
            if code == "invalid_argument" { 2 } else { 1 },
        )
    };
    if marker.is_some() {
        return agent_refused();
    }
    // The plan text is never in argv (visible to other processes).
    if let Some(arg) = args.iter().find(|a| a.as_str() != "--json") {
        return refused(
            "invalid_argument",
            format!(
                "cloud plan publish takes no argument but --json (got {}); the request is read from stdin",
                if arg.starts_with('-') {
                    arg.split('=').next().unwrap_or("an option")
                } else {
                    "a value"
                }
            ),
        );
    }
    let request = match stdin().and_then(|text| {
        if text.trim().is_empty() {
            return Err(
                "the publish request is read from stdin ({\"idempotency_key\",\"plan_id\"?,\"supersedes_version\"?,\"body\"})"
                    .to_owned(),
            );
        }
        plan_body::publish_request(&text)
    }) {
        Ok(request) => request,
        Err(e) => return refused("invalid_argument", e),
    };
    // The project of the working directory; without one there is no
    // binding either.
    let root = match root() {
        Ok(root) => root,
        Err(failure) if failure.code == Code::NoProject => {
            return (
                serde_json::json!({"ok": false, "code": plan_body::NOT_BOUND}),
                1,
            );
        }
        Err(failure) => return refused(failure.code.as_str(), failure.message),
    };
    match post(Path::new(&root), &request) {
        Ok(done) => (
            serde_json::json!({
                "ok": true,
                "plan_id": done.plan_id,
                "version": done.version,
                "content_sha256": done.content_sha256,
            }),
            0,
        ),
        Err(no) => match (no.code.as_str(), no.current_version) {
            ("stale_version", Some(current)) => (
                serde_json::json!({"ok": false, "code": "stale_version", "current_version": current}),
                1,
            ),
            // The key named another publish (Cloud 409): exactly the code,
            // so the app makes a new key. A replay of the same request is
            // answered like the first publish.
            (code @ (plan_body::NOT_BOUND | "idempotency_key_reused"), _) => {
                (serde_json::json!({"ok": false, "code": code}), 1)
            }
            _ => (refused(&no.code, no.message).0, 1),
        },
    }
}

/// The project of the working directory.
fn cwd_root() -> Result<String, Failure> {
    let cwd = std::env::current_dir()
        .map_err(|e| Failure::new(Code::NoProject, format!("working directory: {e}")))?;
    super::routes::project_dir(&cwd.to_string_lossy())
}

/// `lean-ctx cloud plan updates --json …` (the Local Read document),
/// `lean-ctx cloud plan release` and `lean-ctx cloud plan publish`
/// (requests on stdin); `false` for every other command line.
pub(crate) fn try_run(command: &str, rest: &[String]) -> bool {
    let words: Vec<&str> = rest.iter().map(String::as_str).take(2).collect();
    if command != "cloud" {
        return false;
    }
    if words == ["plan", "publish"] {
        let (value, status) = publish(
            &rest[2..],
            env_agent_marker(),
            stdin_publish,
            cwd_root,
            plan_body::publish,
        );
        println!("{value}");
        if status != 0 {
            std::process::exit(status);
        }
        return true;
    }
    if words == ["plan", "release"] {
        let (value, status) = release(&rest[2..], env_agent_marker(), stdin_body);
        println!("{value}");
        if status != 0 {
            std::process::exit(status);
        }
        return true;
    }
    if words != ["plan", "updates"] || !super::wants_json(rest) {
        return false;
    }
    super::print(&answer(KIND, &rest[1..], Via::Cli));
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(w: &[&str]) -> Vec<String> {
        w.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn plan_updates_are_dispatched_and_routed_with_an_optional_project() {
        assert!(super::super::part_of(KIND).is_some(), "{KIND} has no part");
        let route = super::super::routes::all()
            .find(|r| r.kind == KIND)
            .expect("routed");
        assert!(matches!(route.method, Method::Get));
        assert!(matches!(route.project, Project::Optional));
        let text = super::super::routes::table_json().to_string();
        assert!(text.contains("/v1/local/cloud/plan/updates"), "{text}");
        assert!(
            !try_run("cloud", &words(&["plan", "updates"])),
            "needs --json"
        );
        assert!(!try_run("plan", &words(&["updates", "--json"])));
    }

    #[test]
    fn a_release_needs_a_drawn_release_id_on_stdin() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let zeros = "0".repeat(64);
        let request = |rid: &str| {
            serde_json::json!({"release_id": rid, "version": 3, "goal": "g", "brief": null})
                .to_string()
        };
        for (text, status, code) in [
            (String::new(), 2, "invalid_argument"),
            (
                r#"{"version":3,"goal":"g","brief":null}"#.to_owned(),
                2,
                "invalid_argument",
            ),
            (r#"{"release_id":3}"#.to_owned(), 2, "invalid_argument"),
            ("[]".to_owned(), 2, "invalid_argument"),
            // A reconstructed directive id is refused before any lookup.
            (request("plan_AbCdEf12:3:api"), 2, "invalid_argument"),
            // A body with an unknown field (a title, another agent's part)
            // is refused before any lookup.
            (
                serde_json::json!({"release_id": zeros, "version": 3, "goal": "g", "title": "t"})
                    .to_string(),
                2,
                "invalid_argument",
            ),
            (request(&zeros), 1, "not_found"),
            // A decision release: the id only.
            (
                serde_json::json!({"release_id": zeros}).to_string(),
                1,
                "not_found",
            ),
        ] {
            let (value, exit) = release(&[], None, || Ok(text.clone()));
            assert_eq!(exit, status, "{text}: {value}");
            assert_eq!(value["ok"], false);
            assert_eq!(value["released"], false);
            assert_eq!(value["error"]["code"], code, "{value}");
        }
        assert_eq!(
            release(&words(&["--json"]), None, || Ok(request(&zeros))).1,
            1
        );
    }

    /// N4: the release id is never an argument (argv is visible to other
    /// processes of the same user); an old `--release-id` says where it went.
    #[test]
    fn the_release_id_is_refused_in_argv() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let zeros = "0".repeat(64);
        for args in [
            words(&["--release-id", &zeros]),
            words(&[&format!("--release-id={zeros}")]),
            words(&[&zeros]),
        ] {
            let (value, exit) = release(&args, None, || panic!("stdin is not read"));
            assert_eq!(exit, 2, "{args:?}: {value}");
            assert_eq!(value["error"]["code"], "invalid_argument");
        }
        let (value, _) = release(&words(&["--release-id", &zeros]), None, || unreachable!());
        let message = value["error"]["message"].as_str().unwrap();
        assert!(message.contains("moved to stdin"), "{message}");
        assert!(!message.contains(&zeros), "{message}");
    }

    /// Route "agent-callable CLI": inside an agent (a marker set) both the
    /// release and the read that shows release ids refuse.
    #[test]
    fn inside_an_agent_the_cli_neither_releases_nor_shows_release_ids() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        use lean_ctx::shell_hook::{KNOWN_AGENT_ENV_VARS, agent_env_marker, agent_env_markers};
        // One shared list: the shell hook's markers and the further ones.
        let markers: Vec<&str> = agent_env_markers().collect();
        for name in KNOWN_AGENT_ENV_VARS.iter().chain(&[
            "CURSOR_AGENT",
            "CURSOR_TASK_ID",
            "CODEX_SANDBOX",
            "CODEX_THREAD_ID",
            "LEAN_CTX_AGENT_ID",
            "LCTX_AGENT_ID",
            "AI_AGENT",
        ]) {
            assert!(markers.contains(name), "{name}");
        }
        for marker in markers {
            assert_eq!(agent_env_marker(|name| name == marker), Some(marker));
            let (value, exit) = release(&[], Some(marker), || {
                panic!("the request is not read inside an agent")
            });
            // Exactly this: no detail on which marker matched.
            assert_eq!(exit, 1, "{value}");
            assert_eq!(
                value,
                serde_json::json!({"ok": false, "code": "agent_refused"})
            );
        }
        assert_eq!(agent_env_marker(|_| false), None);
        assert_eq!(agent_env_marker(|name| name == "LEAN_CTX_ACTIVE"), None);
    }

    /// Route "local service without the app's auth": the socket answer is
    /// read with [`Reader::Other`], which never fills a release id, even
    /// when the id file holds one.
    #[test]
    fn the_socket_route_never_shows_a_release_id() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let issue = lean_ctx::core::agents::DirectiveIssue {
            kind: lean_ctx::core::agents::DirectiveKind::Plan,
            directive_id: "plan_AbCdEf12:3:all".to_owned(),
            subject: "plan_AbCdEf12".to_owned(),
            version: 3,
            slot: Some("all".to_owned()),
            worker_id: None,
            task_id: None,
            vault_id: "a".repeat(32),
            digest: format!("sha256:{}", "a".repeat(64)),
            brief_sha256: None,
            goal_sha256: None,
            text: String::new(),
        };
        lean_ctx::core::agents::DirectiveStore::mutate(|_, store| {
            store
                .issue("/tmp/plan-project", "agent-x", &issue)
                .map(|_| ())
        })
        .expect("offered");
        let app = DirectiveStore::load_for_app();
        assert!(app.directives[0].release_id.is_some(), "the app sees it");
        let other = DirectiveStore::load();
        assert_eq!(other.directives[0].release_id, None);
        assert!(other.directives[0].release_sha256.is_some());
        assert!(matches!(reader_of(Via::Socket), Ok(Reader::Other)));
        let text = super::super::document(KIND, &read(None, Reader::Other).expect("local"));
        assert!(!text.contains(app.directives[0].release_id.as_deref().unwrap()));
    }

    /// The release is reachable only from this CLI: no bus handler, Cloud
    /// frame, MCP tool or `ctx_agent` action calls it.
    #[test]
    fn only_the_cli_releases_a_directive() {
        let rust = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let allowed = [
            "src/core/agents/directives.rs",
            "src/core/agents/directives_tests.rs",
            "crates/lean-ctx/src/cli/local_read/plan_part.rs",
        ];
        let mut found = Vec::new();
        let mut stack = vec![rust.join("src"), rust.join("crates")];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if path.file_name().is_some_and(|n| n != "target") {
                        stack.push(path);
                    }
                } else if path.extension().is_some_and(|x| x == "rs")
                    && std::fs::read_to_string(&path)
                        .is_ok_and(|text| text.contains("release_offered("))
                {
                    let rel = path.strip_prefix(&rust).unwrap_or(&path);
                    found.push(rel.to_string_lossy().replace('\\', "/"));
                }
            }
        }
        found.sort();
        for file in &found {
            assert!(
                allowed.contains(&file.as_str()),
                "release_offered called from {file}"
            );
        }
        assert!(
            found.iter().any(|f| f.ends_with("plan_part.rs")),
            "{found:?}"
        );
    }

    fn publish_input() -> String {
        serde_json::json!({
            "idempotency_key": "6f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c01",
            "plan_id": "plan_K7mQ2xVb9Z",
            "supersedes_version": 2,
            "body": {"title": "Refunds", "goal": "Ship refunds.",
                     "sessions": [{"slot": "api", "role": "backend", "brief": "Build POST /refunds."}]}
        })
        .to_string()
    }

    fn no_root() -> Result<String, Failure> {
        panic!("the project is not resolved")
    }

    fn no_post(
        _: &Path,
        _: &plan_body::Publish,
    ) -> Result<plan_body::Published, plan_body::PublishRefused> {
        panic!("nothing is posted")
    }

    /// A body just under the 256 KiB a plan may have passes the publish
    /// stdin limit and the request check; the release limit stays 64 KiB.
    #[test]
    fn a_publish_on_stdin_carries_a_body_of_almost_256_kib() {
        let sessions = |brief_len: usize| -> Vec<serde_json::Value> {
            (0..32)
                .map(|i| {
                    let len = if i == 31 { brief_len } else { 8_000 };
                    serde_json::json!({"slot": format!("s{i:02}"), "role": "dev",
                        "brief": "b".repeat(len)})
                })
                .collect()
        };
        let body = |brief_len: usize| {
            serde_json::json!({"title": "Big", "goal": "g".repeat(4_000),
                "sessions": sessions(brief_len)})
        };
        let size = |b: &serde_json::Value| serde_json::to_vec(b).map_or(0, |v| v.len());
        // Fill the last brief up to 256 KiB less 512 bytes of the body.
        let target = plan_body::MAX_BODY_BYTES - 512;
        let fill = 8_000 - (size(&body(8_000)).saturating_sub(target)).min(8_000);
        let big = body(fill);
        let text = serde_json::json!({
            "idempotency_key": "6f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c01",
            "body": big,
        })
        .to_string();
        assert!(
            size(&big) > plan_body::MAX_BODY_BYTES - 1_024,
            "{}",
            size(&big)
        );
        assert!(size(&big) < plan_body::MAX_BODY_BYTES, "{}", size(&big));
        let read = capped_text(text.as_bytes(), MAX_PUBLISH_BYTES).expect("fits the publish limit");
        assert!(capped_text(text.as_bytes(), MAX_BODY_BYTES).is_err());
        let publish = plan_body::publish_request(&read).expect("a body under 256 KiB is published");
        assert!(
            publish.request["body"]
                .as_str()
                .is_some_and(|b| b.len() > 4 * 255 * 1_024 / 3)
        );
        let over = "x".repeat(usize::try_from(MAX_PUBLISH_BYTES).unwrap_or(0) + 1);
        assert!(capped_text(over.as_bytes(), MAX_PUBLISH_BYTES).is_err());
    }

    /// Agents may not publish: with a marker set, nothing is read or sent.
    #[test]
    fn inside_an_agent_the_cli_does_not_publish() {
        for marker in lean_ctx::shell_hook::agent_env_markers() {
            let (value, exit) = publish(
                &[],
                Some(marker),
                || panic!("the request is not read inside an agent"),
                no_root,
                no_post,
            );
            assert_eq!(exit, 1, "{value}");
            assert_eq!(
                value,
                serde_json::json!({"ok": false, "code": "agent_refused"})
            );
        }
    }

    /// The plan text is never an argument, and a request that is no
    /// publish is refused before anything is sent.
    #[test]
    fn a_publish_takes_its_request_from_stdin_only() {
        for args in [
            words(&["--body", "x"]),
            words(&["Ship refunds."]),
            words(&["--body=Ship refunds."]),
        ] {
            let (value, exit) = publish(&args, None, || panic!("stdin"), no_root, no_post);
            assert_eq!(exit, 2, "{value}");
            assert_eq!(value["code"], "invalid_argument");
            assert!(!value.to_string().contains("Ship refunds."), "{value}");
        }
        for text in [String::new(), "[]".to_owned(), r#"{"body":{}}"#.to_owned()] {
            let (value, exit) = publish(&[], None, || Ok(text.clone()), no_root, no_post);
            assert_eq!(exit, 2, "{text}: {value}");
            assert_eq!(value["code"], "invalid_argument");
        }
    }

    #[test]
    fn a_publish_answers_the_version_or_the_current_one() {
        let root = || Ok("/work/app".to_owned());
        let (value, exit) = publish(
            &words(&["--json"]),
            None,
            || Ok(publish_input()),
            root,
            |path, request| {
                assert_eq!(path, Path::new("/work/app"));
                Ok(plan_body::Published {
                    plan_id: "plan_K7mQ2xVb9Z".to_owned(),
                    version: 3,
                    content_sha256: request.content_sha256.clone(),
                })
            },
        );
        assert_eq!(exit, 0, "{value}");
        assert_eq!(value["ok"], true);
        assert_eq!(value["plan_id"], "plan_K7mQ2xVb9Z");
        assert_eq!(value["version"], 3);
        assert!(
            value["content_sha256"]
                .as_str()
                .unwrap()
                .starts_with("sha256:")
        );
        let (value, exit) = publish(
            &[],
            None,
            || Ok(publish_input()),
            root,
            |_, _| {
                Err(plan_body::PublishRefused {
                    code: "stale_version".to_owned(),
                    message: "m".to_owned(),
                    current_version: Some(5),
                })
            },
        );
        assert_eq!(
            (value, exit),
            (
                serde_json::json!({"ok": false, "code": "stale_version", "current_version": 5}),
                1
            )
        );
        // Without a binding of the working directory nothing is posted.
        let unbound = || {
            Err(Failure::new(
                Code::NoProject,
                "/work is not a project".to_owned(),
            ))
        };
        let (value, exit) = publish(&[], None, || Ok(publish_input()), unbound, no_post);
        assert_eq!(
            (value, exit),
            (serde_json::json!({"ok": false, "code": "not_bound"}), 1)
        );
        let (value, exit) = publish(
            &[],
            None,
            || Ok(publish_input()),
            root,
            |_, _| {
                Err(plan_body::PublishRefused {
                    code: "not_bound".to_owned(),
                    message: "m".to_owned(),
                    current_version: None,
                })
            },
        );
        assert_eq!(
            (value, exit),
            (serde_json::json!({"ok": false, "code": "not_bound"}), 1)
        );
        let (value, exit) = publish(
            &[],
            None,
            || Ok(publish_input()),
            root,
            |_, _| {
                Err(plan_body::PublishRefused {
                    code: "idempotency_key_reused".to_owned(),
                    message: "m".to_owned(),
                    current_version: None,
                })
            },
        );
        assert_eq!(
            (value, exit),
            (
                serde_json::json!({"ok": false, "code": "idempotency_key_reused"}),
                1
            )
        );
    }

    #[test]
    fn without_kept_plans_the_document_is_empty() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let doc = read(None, Reader::App).expect("local files only");
        let value: serde_json::Value =
            serde_json::from_str(&super::super::document(KIND, &doc)).unwrap();
        assert_eq!(value["contract"], "local-read/1");
        assert_eq!(value["kind"], KIND);
        assert_eq!(value["plans"], serde_json::json!([]));
    }
}
