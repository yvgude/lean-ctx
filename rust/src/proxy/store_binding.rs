// SPDX-License-Identifier: Apache-2.0
//! Whether the host of a request is served by an Engine that reads THIS
//! store.
//!
//! A recovery stub names the expand tool the host listed. The id in it
//! resolves only in an Engine that shares the proxy's data directory and
//! project root; a tool NAME shows neither. Nothing a caller sends — a
//! header, a body field, a naming convention — is proof of it either. What
//! is read here is only what this installation records about itself:
//!
//! - the address the proxy's listener is really bound to ([`note_listener`]);
//! - the agent presence registry inside the proxy's own data directory
//!   (`<data dir>/agents/registry.json`, `core::agents`), in which every MCP
//!   server of this installation registers its own process.
//!
//! The decision that uses it, its four conditions and its limits are stated
//! at [`host_store_bound`].

#[cfg(feature = "http-server")]
use std::net::IpAddr;
#[cfg(feature = "http-server")]
use std::path::Path;
#[cfg(feature = "http-server")]
use std::sync::OnceLock;
#[cfg(feature = "http-server")]
use std::sync::atomic::{AtomicU8, Ordering};

#[cfg(feature = "http-server")]
use crate::core::agents::AgentRegistry;
use crate::core::archive::delivered;

/// The agent type under which an MCP server registers its own process
/// (`AgentRegistry::register_mcp_process_as`, called from the server
/// lifecycle).
#[cfg(feature = "http-server")]
const MCP_ENGINE: &str = "mcp";

/// No listener of this process has reported its address.
#[cfg(feature = "http-server")]
const LISTENER_UNKNOWN: u8 = 0;
/// The listener is bound to a loopback address.
#[cfg(feature = "http-server")]
const LISTENER_LOOPBACK: u8 = 1;
/// A listener is bound to an address other machines can reach.
#[cfg(feature = "http-server")]
const LISTENER_REACHABLE: u8 = 2;

/// What is known about the listener(s) of this process. Only ever raised:
/// once a reachable listener was seen, a loopback one does not lower it.
#[cfg(feature = "http-server")]
static LISTENER: AtomicU8 = AtomicU8::new(LISTENER_UNKNOWN);

#[cfg(feature = "http-server")]
fn listener_state(bound: IpAddr) -> u8 {
    if bound.is_loopback() {
        LISTENER_LOOPBACK
    } else {
        LISTENER_REACHABLE
    }
}

/// The proxy's listener is bound to `bound` — the address the socket
/// reports after the bind (`TcpListener::local_addr`), not the configured
/// wish.
#[cfg(feature = "http-server")]
pub(crate) fn note_listener(bound: IpAddr) {
    LISTENER.fetch_max(listener_state(bound), Ordering::Relaxed);
}

/// Whether every listener this process reported is bound to loopback. An
/// unknown listener is not loopback.
#[cfg(feature = "http-server")]
fn serves_local_callers_only() -> bool {
    LISTENER.load(Ordering::Relaxed) == LISTENER_LOOPBACK
}

/// The executable of this process, in the spelling the presence registry
/// records for a process (`ipc::process::identity`).
#[cfg(feature = "http-server")]
fn own_executable() -> Option<&'static str> {
    static OWN: OnceLock<Option<String>> = OnceLock::new();
    OWN.get_or_init(|| {
        crate::ipc::process::identity(std::process::id()).map(|identity| identity.executable)
    })
    .as_deref()
}

/// Whether the root a presence entry records is `project` (canonical). The
/// registry stores the root as the registering server was given it, so both
/// sides are compared canonically. A relative root is no match: it would be
/// resolved against the proxy's working directory, not the Engine's.
#[cfg(feature = "http-server")]
fn same_project(recorded: &str, project: &Path) -> bool {
    let recorded = Path::new(recorded);
    recorded.is_absolute()
        && crate::core::pathutil::canonicalize_secure(recorded)
            .is_ok_and(|canonical| canonical == project)
}

/// Whether the presence registry of THIS data directory holds a live MCP
/// Engine of this installation for `project` (canonical).
///
/// Liveness is the registry's own rule (`AgentRegistry::list_active`): the
/// entry is not finished, and its pid is alive in the process generation the
/// entry recorded. On top of it: the entry is the MCP server's own presence
/// (type `mcp`, or carrying the transport only that server records — an
/// agent that declared another type through `ctx_agent register` on the same
/// process keeps it), its recorded process runs the executable this proxy
/// runs, and its root is `project`. A registry that is missing, unreadable or
/// malformed holds nothing.
#[cfg(feature = "http-server")]
fn engine_live_for(project: &Path) -> bool {
    let Some(own) = own_executable() else {
        return false;
    };
    let Some(registry) = AgentRegistry::load() else {
        return false;
    };
    registry.list_active(None).into_iter().any(|agent| {
        (agent.agent_type == MCP_ENGINE || agent.transport.is_some())
            && agent
                .process_identity
                .as_ref()
                .is_some_and(|identity| identity.executable == own)
            && same_project(&agent.project_root, project)
    })
}

/// The NECESSARY store conditions of the followability decision under
/// protection, read once per request pass. They are not sufficient: which
/// Engine served THIS host is shown per block ([`delivered_for_call`]; the
/// whole decision is stated at `sticky_tools::expand_tool_for`).
///
/// A stub may be issued under protection only if ALL four hold (and the
/// per-block correlation on top). The first two are decided elsewhere, the
/// last two here:
///
/// 1. The host's own tool list names the expand tool and the request lets
///    its model call it (`sticky_tools::HandleUse::for_request`). Proves:
///    the model can make the call. Not: which Engine answers it.
/// 2. The block has a complete lineage in this store that still admits
///    (`compress::bound_copy`, per block). Proves: a process writing THIS
///    data directory recorded exactly these bytes for the proxy's project —
///    recovery store and delivered register both resolve from
///    `data_dir::lean_ctx_data_dir()`. Not: that this host received them
///    from that process.
/// 3. A live MCP Engine of this installation is registered for the proxy's
///    project in this data directory's presence registry
///    ([`engine_live_for`]). The entry carries the pid and process identity
///    (start marker, executable) of the process that wrote it; a request to
///    the proxy cannot create one. Proves: an Engine that reads this store
///    under this project is running now and can resolve the id. Not: that
///    it is the Engine this host talks to.
/// 4. The proxy's listener is bound to loopback ([`note_listener`]).
///    Proves: every caller connects from this machine, where a standard
///    installation has one data directory per OS user. Closes: a host on
///    another machine, routed to this proxy, whose Engine uses another
///    store. Not known, or not loopback: not bound.
///
/// Any read that fails is "not bound"; the block then stays whole
/// (`recovery.handle_not_followable`) and nothing is stored.
///
/// What a tool call can and cannot write into the registry. `ctx_agent
/// action="register"` and the Engine's HTTP `agents/register` route accept a
/// caller-chosen agent type (and the HTTP route a caller-chosen root), so
/// `mcp` and the root alone are not proof. The pid and the process identity
/// are never the caller's: `AgentRegistry::register` always records the
/// process that executes the call — a `lean-ctx` process writing this data
/// directory. A caller able to make such an entry is therefore already a
/// client of an Engine on this store; what it can falsify is only which
/// project that Engine is said to serve.
///
/// Limits, stated plainly:
///
/// - The four conditions bind the INSTALLATION, not the request. Nothing
///   links one request to one Engine instance. Another local process of the
///   same OS user that reaches the loopback listener and lists an expand
///   tool belonging to a DIFFERENT Engine or store (a second data directory
///   via `LEAN_CTX_DATA_DIR`, a container with its own home, an Engine
///   started for another project) gets a stub whenever a local Engine for
///   the proxy's project is live and the block's exact bytes are on record
///   here. A standard installation — one data directory per user, the host's
///   MCP Engine started by the same `lean-ctx` — does not produce this; a
///   deliberately split one can. The consequence is a handle that host's
///   Engine cannot resolve: its model reads the shortened block and a
///   refusal. Nothing is disclosed — the copy holds bytes the request itself
///   carried, and it stays in this store.
/// - Condition 4 reads the socket, not the path to it. A local reverse proxy
///   or tunnel that forwards remote callers to the loopback listener makes
///   them local to this check.
/// - Same OS user is an assumption, not a recorded fact: the registry file
///   is written owner-only (`config_io::write_atomic`, 0600 on Unix) and an
///   entry carries no uid. A data directory other users can write into is a
///   shared installation already.
/// - The registry keeps one entry per process. An Engine that serves several
///   projects from one process is on record for the root it registered last;
///   an Engine registered with a relative root matches nothing. Both cost
///   compression, not safety.
/// - Evaluated once per request, before its first pass. An Engine that
///   exits between this read and the model's call leaves a handle that
///   resolves again as soon as an Engine for the project runs.
#[cfg(feature = "http-server")]
pub(crate) fn host_store_bound() -> bool {
    serves_local_callers_only()
        && super::request_project::current_source_project()
            .is_some_and(|project| engine_live_for(&project))
}

/// Whether THIS store has `block` — exact bytes — on record, for the proxy's
/// project, as delivered for the tool call with the id `call_id`
/// (`delivered::delivered_to_call`). The per-block half of the decision
/// under protection.
///
/// Why [`host_store_bound`] is not enough: on one machine a host can use the
/// expand tool of a SECOND Engine — another data directory, another
/// worktree or project — while an Engine of this store is live for the
/// proxy's project and the block's exact bytes are on record here (two
/// Engines reading the same file deliver the same bytes). A digest hit does
/// not show which Engine served this host.
///
/// What a hit here shows: an Engine writing this data directory answered a
/// call for which the host named exactly this id, with exactly these bytes,
/// for this project. What it does not show is stated at
/// `sticky_tools::expand_tool_for`. No project, a malformed id, an entry
/// without ids (every entry written before ids were recorded) and any read
/// that fails are "no".
pub(crate) fn delivered_for_call(block: &str, call_id: &str) -> bool {
    super::request_project::current_source_project().is_some_and(|project| {
        super::request_project::in_project(&project, || {
            delivered::delivered_to_call(&delivered::digest(block), call_id)
        })
    })
}

/// The test plays the Engine at its release point: `block` is what an MCP
/// call for `file` (holding `content`) delivered, recorded through the
/// register's own tool-delivery entry — for the call `call_id`, or without
/// an id as a host that names none, the CLI, or another Engine's call
/// would leave it. Needs protection to be active (the read is bound).
#[cfg(test)]
pub(crate) fn engine_delivers(
    project: &Path,
    file: &str,
    content: &str,
    block: &str,
    call_id: Option<&str>,
) {
    let root = project.to_str().expect("a UTF-8 project root");
    let path = project.join(file);
    std::fs::write(&path, content).expect("source written");
    let path = path.to_str().expect("a UTF-8 path");
    let recorded = super::request_project::in_project(project, || {
        let admitted = crate::tools::ctx_read::read_file_for_tool_rooted(path, root, "ctx_read")
            .expect("admitted read");
        let authority = crate::core::archive::authority::ArchiveAuthority::file(
            project, path, "ctx_read", &admitted,
        )
        .expect("protection is active: the read is bound");
        delivered::record_tool_delivery_for_call(true, false, Some(authority), &[block], call_id)
    });
    assert_eq!(recorded, 1, "the delivery is on record");
}

/// A listener state set by a test; what it replaced is back when dropped
/// (unknown, outside any other such guard). Tests that set it hold the
/// process-wide test lock (`data_dir::isolated_data_dir`) and drop the guard
/// before that lock.
#[cfg(test)]
#[must_use]
pub(crate) struct ListenerForTest {
    replaced: u8,
}

#[cfg(test)]
impl Drop for ListenerForTest {
    fn drop(&mut self) {
        LISTENER.store(self.replaced, Ordering::Relaxed);
    }
}

/// The proxy's listener as a test wants it: bound to `bound`.
#[cfg(test)]
pub(crate) fn listening_on(bound: IpAddr) -> ListenerForTest {
    ListenerForTest {
        replaced: LISTENER.swap(listener_state(bound), Ordering::Relaxed),
    }
}

/// The variable that makes [`tests::engine_process_for_test`] the Engine
/// process of a [`bound_host`] instead of returning at once.
#[cfg(test)]
const ENGINE_PROCESS_VAR: &str = "LEAN_CTX_TEST_ENGINE_PROCESS";

/// A live process of this installation's executable, standing in for the
/// MCP Engine of a bound host: this test binary, running only
/// [`tests::engine_process_for_test`], which waits until its stdin closes.
/// Killed and reaped when dropped.
#[cfg(test)]
struct EngineProcessForTest(std::process::Child);

#[cfg(test)]
impl EngineProcessForTest {
    fn start() -> Self {
        use std::process::{Command, Stdio};

        let child = Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "proxy::store_binding::tests::engine_process_for_test",
                "--test-threads=1",
                "--quiet",
            ])
            .env(ENGINE_PROCESS_VAR, "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("the Engine process starts");
        Self(child)
    }
}

#[cfg(test)]
impl Drop for EngineProcessForTest {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A bound standard host for a test, until dropped: the listener first goes
/// back to what it was, then the Engine process ends.
#[cfg(test)]
#[must_use]
pub(crate) struct BoundHostForTest {
    _listener: ListenerForTest,
    _engine: EngineProcessForTest,
}

/// A provably bound standard host for `project`: a live process of this
/// installation's executable is on record as the MCP Engine of the project
/// in the current data directory, and the listener is bound to loopback.
///
/// The Engine is a process of its own, as it is next to a real proxy. The
/// registry keeps ONE presence per process, and every `LeanCtxServer` this
/// test process constructs re-registers that presence for its own root —
/// also in tests that hold no test lock, in whichever data directory a
/// locked test isolated at that moment. With this process as the Engine, a
/// running scenario lost its binding that way (`handle_not_followable`,
/// 25.10.).
#[cfg(test)]
pub(crate) fn bound_host(project: &Path) -> BoundHostForTest {
    let engine = EngineProcessForTest::start();
    let identity = crate::ipc::process::identity(engine.0.id()).expect("Engine process identity");
    assert_eq!(
        Some(identity.executable.as_str()),
        own_executable(),
        "the Engine process runs this installation's executable"
    );
    engine_on_record(engine.0.id(), identity, project);
    BoundHostForTest {
        _listener: listening_on(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
        _engine: engine,
    }
}

/// Where the presence registry of the current data directory lives.
#[cfg(test)]
pub(crate) fn registry_file() -> std::path::PathBuf {
    crate::core::data_dir::lean_ctx_data_dir()
        .expect("data directory")
        .join("agents")
        .join("registry.json")
}

/// An active MCP Engine entry for `project` as the process `pid` of
/// generation `identity` would have left it. Written under the registry's
/// lock: MCP servers of this process may register at the same time.
#[cfg(test)]
pub(crate) fn engine_on_record(
    pid: u32,
    identity: crate::ipc::process::ProcessIdentity,
    project: &Path,
) {
    let now = chrono::Utc::now();
    let entry = crate::core::agents::AgentEntry {
        agent_id: format!("mcp-{pid}-test"),
        agent_type: MCP_ENGINE.to_owned(),
        role: Some("context-engine".to_owned()),
        project_root: project.to_str().expect("a UTF-8 project root").to_owned(),
        started_at: now,
        last_active: now,
        pid,
        process_identity: Some(identity),
        durable_identity_id: None,
        status: crate::core::agents::AgentStatus::Active,
        status_message: None,
        transport: None,
        role_source: None,
        parent: None,
    };
    AgentRegistry::mutate_locked(|registry| registry.agents.push(entry)).expect("registry saved");
}

/// An MCP Engine entry for `project`, of this installation's executable,
/// whose process has exited: a real child, recorded while it ran.
#[cfg(all(test, unix))]
pub(crate) fn exited_engine_on_record(project: &Path) {
    use std::process::{Command, Stdio};

    let mut child = Command::new("sleep")
        .arg("60")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("process starts");
    let mut identity = crate::ipc::process::identity(child.id()).expect("child identity");
    own_executable()
        .expect("own executable")
        .clone_into(&mut identity.executable);
    child.kill().expect("child killed");
    child.wait().expect("child reaped");
    engine_on_record(child.id(), identity, project);
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    use std::path::Path;
    use std::sync::atomic::Ordering;

    use super::{
        LISTENER, LISTENER_UNKNOWN, bound_host, delivered_for_call, engine_delivers,
        engine_live_for, host_store_bound, listening_on, note_listener, own_executable,
        registry_file, same_project, serves_local_callers_only,
    };
    use crate::core::agents::AgentRegistry;

    fn project() -> (tempfile::TempDir, std::path::PathBuf) {
        let directory = tempfile::tempdir().expect("project");
        // Canonical as the proxy resolves its project (`request_project`):
        // no Windows verbatim prefix.
        let root =
            crate::core::pathutil::canonicalize_secure(directory.path()).expect("canonical root");
        (directory, root)
    }

    /// Governed mode with `root` as the proxy's configured project root.
    fn governed_project(root: &Path) {
        let config = crate::core::paths::config_dir_read_only().expect("config directory");
        std::fs::create_dir_all(&config).expect("config directory");
        std::fs::write(
            config.join("config.toml"),
            format!(
                "[context_gateway]\nmode = \"governed\"\n\n[proxy]\nproject_root = '{}'\n",
                root.display()
            ),
        )
        .expect("gateway config");
    }

    /// The body of the Engine process a [`bound_host`] starts: it waits until
    /// its stdin closes. In the suite itself it returns at once.
    #[test]
    fn engine_process_for_test() {
        if std::env::var_os(super::ENGINE_PROCESS_VAR).is_none() {
            return;
        }
        let _ = std::io::copy(&mut std::io::stdin(), &mut std::io::sink());
    }

    /// A bound host stays bound while MCP servers of this process register
    /// for other projects — as every `LeanCtxServer` does, also one built by
    /// a test without the test lock in the data directory a locked test
    /// isolated (the scenario of a flaky `handle_not_followable`, 25.10.).
    #[test]
    fn a_bound_host_survives_servers_of_this_process_registering_elsewhere() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let (_directory, root) = project();
        let (_other_directory, other) = project();
        governed_project(&root);
        let _bound = bound_host(&root);
        assert!(host_store_bound());
        AgentRegistry::register_mcp_process(other.to_str().expect("utf-8"), None)
            .expect("this process registers for another project");
        assert!(engine_live_for(&other));
        assert!(
            engine_live_for(&root),
            "the bound host's Engine is untouched"
        );
        assert!(host_store_bound());
    }

    /// Only a loopback listener serves local callers only; an unknown one
    /// does not, and a reachable listener is not lowered by a later loopback
    /// one.
    #[test]
    fn only_a_loopback_listener_serves_local_callers_only() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        LISTENER.store(LISTENER_UNKNOWN, Ordering::Relaxed);
        assert!(!serves_local_callers_only(), "unknown");
        note_listener(IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert!(serves_local_callers_only());
        note_listener(IpAddr::V6(Ipv6Addr::LOCALHOST));
        assert!(serves_local_callers_only());
        note_listener(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        assert!(!serves_local_callers_only(), "all interfaces");
        note_listener(IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert!(!serves_local_callers_only(), "never lowered");
        LISTENER.store(LISTENER_UNKNOWN, Ordering::Relaxed);

        for reachable in [
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20)),
            IpAddr::V6(Ipv6Addr::UNSPECIFIED),
        ] {
            let _listener = listening_on(reachable);
            assert!(!serves_local_callers_only(), "{reachable}");
        }
        assert!(!serves_local_callers_only(), "unknown again after a test");
    }

    /// [real presence registry] A live MCP Engine of this installation,
    /// registered for the proxy's project in this data directory, binds the
    /// host — and nothing less does: no registry, an entry for another
    /// project, an entry of another agent type, an unreadable registry, a
    /// listener that is not loopback, a proxy without a project.
    #[test]
    fn a_live_engine_of_this_store_and_project_binds_the_host() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let (_directory, root) = project();
        let (_other_directory, other) = project();
        assert!(own_executable().is_some(), "this process has an identity");

        // No project configured: nothing to be bound for.
        {
            let _bound = bound_host(&root);
            assert!(engine_live_for(&root));
            assert!(!host_store_bound(), "the proxy acts for no project");
        }
        governed_project(&root);

        // No registry at all, then one that names only another project.
        std::fs::remove_file(registry_file()).expect("registry removed");
        let loopback = listening_on(IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert!(!engine_live_for(&root), "no registry");
        assert!(!host_store_bound());
        AgentRegistry::register_mcp_process(other.to_str().expect("utf-8"), None)
            .expect("registered");
        assert!(engine_live_for(&other));
        assert!(!engine_live_for(&root), "an Engine for another project");
        assert!(!host_store_bound());

        // This process under another agent type, for the right project, and
        // without the MCP server's transport presence (a `ctx_agent` worker
        // re-typing the transport's own entry keeps it the transport).
        std::fs::remove_file(registry_file()).expect("registry removed");
        AgentRegistry::mutate_locked(|registry| {
            registry.register("claude", Some("coder"), root.to_str().expect("utf-8"), None)
        })
        .expect("registry written")
        .1
        .expect("registered");
        assert!(!engine_live_for(&root), "not an MCP Engine");
        assert!(!host_store_bound());

        // The MCP server's own registration for the project.
        AgentRegistry::register_mcp_process(root.to_str().expect("utf-8"), None)
            .expect("registered");
        assert!(engine_live_for(&root));
        assert!(host_store_bound());
        // Necessary, not sufficient: a block counts only for the call this
        // store's Engine answered with exactly its bytes.
        let block = "pub fn kept() {}\n";
        assert!(!delivered_for_call(block, "toolu_X"), "nothing on record");
        engine_delivers(&root, "other.rs", block, block, None);
        assert!(!delivered_for_call(block, "toolu_X"), "a digest hit alone");
        engine_delivers(&root, "kept.rs", block, block, Some("toolu_X"));
        assert!(delivered_for_call(block, "toolu_X"));
        assert!(!delivered_for_call(block, "toolu_Y"), "another call");
        assert!(!delivered_for_call("other bytes", "toolu_X"));

        // The same Engine, a listener other machines can reach.
        drop(loopback);
        assert!(!host_store_bound(), "listener unknown");
        {
            let _reachable = listening_on(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
            assert!(engine_live_for(&root));
            assert!(!host_store_bound(), "not loopback");
        }

        // A registry that cannot be read holds nothing.
        let _loopback = listening_on(IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert!(host_store_bound());
        std::fs::write(registry_file(), "{ not a registry").expect("registry overwritten");
        assert!(!engine_live_for(&root), "malformed registry");
        assert!(!host_store_bound());
    }

    /// The registry stores the root as the server was given it: another
    /// spelling of the same directory still matches, a relative one and a
    /// missing one never do.
    #[test]
    fn the_recorded_root_is_compared_canonically() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let (directory, root) = project();
        let nested = root.join("nested");
        std::fs::create_dir(&nested).expect("nested directory");
        let roundabout = nested.join("..");
        assert!(same_project(root.to_str().expect("utf-8"), &root));
        assert!(same_project(
            directory.path().to_str().expect("utf-8"),
            &root
        ));
        assert!(same_project(roundabout.to_str().expect("utf-8"), &root));
        assert!(!same_project(nested.to_str().expect("utf-8"), &root));
        assert!(!same_project(".", &root), "relative");
        assert!(!same_project("", &root));
        assert!(!same_project(
            root.join("missing").to_str().expect("utf-8"),
            &root
        ));

        AgentRegistry::register_mcp_process(roundabout.to_str().expect("utf-8"), None)
            .expect("registered");
        assert!(engine_live_for(&root), "a non-canonical spelling binds");
    }

    /// [real processes] An entry is live only while the process it recorded
    /// is: the identity of an exited child binds nothing. A live process of
    /// another executable that shares the registry is not an Engine of this
    /// installation either.
    #[cfg(unix)]
    #[test]
    fn a_dead_or_foreign_process_binds_nothing() {
        use std::process::{Command, Stdio};

        use super::{engine_on_record, exited_engine_on_record};

        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let (_directory, root) = project();
        let mut child = Command::new("sleep")
            .arg("60")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("process starts");
        // The identity names the executable; read it once the child runs
        // `sleep`, not the test binary it was forked from (Linux reads
        // /proc/<pid>/exe, which changes at exec; under load that is later).
        let own = std::env::current_exe().expect("test binary");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let identity = loop {
            let identity = crate::ipc::process::identity(child.id()).expect("child identity");
            if std::path::Path::new(&identity.executable) != own
                || std::time::Instant::now() >= deadline
            {
                break identity;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        engine_on_record(child.id(), identity, &root);
        let live = (0..250).any(|_| {
            let live = AgentRegistry::load()
                .expect("registry")
                .list_active(None)
                .iter()
                .any(|agent| agent.pid == child.id());
            if !live {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            live
        });
        let bound_while_live = engine_live_for(&root);
        child.kill().expect("child killed");
        child.wait().expect("child reaped");
        assert!(live, "live by the registry's rule while it ran");
        assert!(!bound_while_live, "a live process of another executable");
        assert!(!engine_live_for(&root), "and nothing once it has exited");

        // A record of this installation's executable whose process has
        // exited: liveness is the recorded process generation.
        std::fs::remove_file(registry_file()).expect("registry removed");
        exited_engine_on_record(&root);
        let registry = AgentRegistry::load().expect("registry");
        assert_eq!(registry.list_all().len(), 1, "the entry is on record");
        assert!(registry.list_active(None).is_empty(), "and not live");
        assert!(!engine_live_for(&root), "an exited Engine");
    }
}
