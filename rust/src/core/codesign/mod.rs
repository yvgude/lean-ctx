//! macOS code-signing with a stable, self-signed identity (#356).
//!
//! Ad-hoc signing (`codesign -s -`) re-keys the binary's cdhash on every build.
//! macOS TCC anchors an ad-hoc binary's privacy grant to that cdhash, so each
//! update looks like a brand-new program and re-pops the
//! "lean-ctx wants to access your Documents folder" prompt — forever.
//!
//! Signing every build with ONE persistent self-signed identity instead gives
//! TCC a stable Designated Requirement
//! (`identifier "com.leanctx.cli" and certificate leaf = H"…"`), so a single
//! "Allow" survives all future updates.
//!
//! The identity lives in a dedicated keychain so we never touch the login
//! keychain or need the user's login password. Creating it requires a one-time
//! GUI trust confirmation — see [`setup_identity`], invoked by
//! `lean-ctx codesign-setup`. [`sign_binary`] is otherwise fully automatic and
//! attempts an ad-hoc signature when the stable one is not available — the
//! attempt that is meant to keep the binary launchable; it reports the
//! attempt, not that a signature was made.

//!
//! Every tool this module starts is one bounded child process: no input
//! (the unlock alone reads its command, password included, from a pipe), a
//! capped output, a time budget of its own and the one deadline of the whole
//! signing, which keeps a slice in reserve for the ad-hoc fallback. A tool
//! that does not answer in time is ended by the shared capture, with its
//! process group, and reaped before the next step starts. The budgets bound
//! how long an answer is waited for and accepted; they are no promise about
//! a descendant that deliberately detached itself, about a cleanup the
//! capture reports as failed, or about what the operating system needs to
//! finish a child that was started.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

mod setup;
pub(crate) use setup::{SetupOutcome, setup_identity};
#[cfg(test)]
use test_scope::{QueryReason, RunReason, Stage, capture_reason, query_reason};

/// Common Name of the signing certificate (stable across builds).
pub(crate) const IDENTITY_CN: &str = "lean-ctx-codesign";
/// Designated-requirement identifier baked into every signature.
pub(crate) const CODESIGN_IDENTIFIER: &str = "com.leanctx.cli";

/// Which signing path [`sign_binary`] took.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SignKind {
    /// Signed with the persistent identity — TCC grant survives updates.
    Stable,
    /// The stable path was not taken or did not succeed and the ad-hoc
    /// fallback was attempted. This says which path was tried last, not that
    /// an ad-hoc signature exists: that attempt is best effort and bounded.
    AdHoc,
}

/// How long each step may take, and all of them together.
#[derive(Debug, Clone, Copy)]
struct Budgets {
    /// `security find-identity`.
    query: Duration,
    /// `security unlock-keychain`.
    unlock: Duration,
    /// `codesign` with the persistent identity.
    stable: Duration,
    /// `codesign` ad hoc. Reserved: the steps before it must be over this
    /// long before the whole signing ends.
    adhoc: Duration,
    /// The whole signing, fallback included.
    total: Duration,
}

impl Budgets {
    const PRODUCTION: Self = Self {
        query: Duration::from_secs(5),
        unlock: Duration::from_secs(5),
        stable: Duration::from_secs(20),
        adhoc: Duration::from_secs(15),
        total: Duration::from_secs(40),
    };
}

/// Captured output of one tool. `find-identity` lists a handful of lines;
/// anything near these sizes is not an answer and ends the tool.
const STDOUT_LIMIT: usize = 64 * 1024;
const STDERR_LIMIT: usize = 16 * 1024;

/// What signing takes from its surroundings: which keychain, where its
/// password is kept, which tools, how long. Production has exactly one,
/// [`SigningContext::production`]; nothing in it is read from the request
/// that asks for a signature.
#[derive(Debug, Clone)]
struct SigningContext {
    keychain: Option<PathBuf>,
    password_file: Option<PathBuf>,
    security: OsString,
    codesign: OsString,
    budgets: Budgets,
}

impl SigningContext {
    fn production() -> Self {
        Self {
            keychain: keychain_path(),
            password_file: password_path(),
            security: "security".into(),
            codesign: "codesign".into(),
            budgets: Budgets::PRODUCTION,
        }
    }

    fn current() -> Self {
        #[cfg(test)]
        if let Some(scoped) = test_scope::current() {
            return scoped;
        }
        Self::production()
    }

    /// The keychain password, if set up. Passed to `security` only; never
    /// logged, returned or put into an error.
    fn password(&self) -> Option<String> {
        let password = std::fs::read_to_string(self.password_file.as_ref()?).ok()?;
        let password = password.trim().to_string();
        (!password.is_empty()).then_some(password)
    }
}

/// Run one tool, bounded: no input, capped output, and an answer that counts
/// only if it is back by the earlier of `budget` from now and `deadline` —
/// one instant, fixed before the tool is started and checked again on a
/// fresh clock after the capture returns, because the capture's own wait
/// begins only once the child exists.
///
/// `Ok(None)` is "no answer to act on, and the step is over": the instant had
/// already passed (nothing is started), or the capture returned normally for
/// a tool that ran out of time — having ended its process group and reaped
/// it — or whose answer came back too late.
///
/// `Err` is every error of the capture itself: a tool that could not be
/// started, an output over its limit, a wait that failed, a cleanup or reap
/// the capture could not confirm. They are not told apart — no error text is
/// read — and all of them stop the signing: after an unconfirmed cleanup a
/// process of this step may still be writing, so nothing further is started
/// and the caller must not go on to use the file.
///
/// Nothing the tool printed reaches the caller except through `Ok(Some(_))`.
fn run(
    command: &mut Command,
    budget: Duration,
    deadline: Instant,
) -> Result<Option<std::process::Output>, SigningStopped> {
    command.stdin(Stdio::null());
    capture(command, budget, deadline)
}

/// As [`run`], with `input` as the tool's whole standard input. A secret
/// given this way is in neither the tool's arguments nor its environment,
/// which other processes can read. A pipe that cannot be prepared counts as
/// a tool that could not be started.
fn run_with_input(
    command: &mut Command,
    input: &[u8],
    budget: Duration,
    deadline: Instant,
) -> Result<Option<std::process::Output>, SigningStopped> {
    let stdin = piped_input(input).map_err(|_| {
        #[cfg(test)]
        test_scope::note_run(RunReason::CaptureRejected);
        SigningStopped
    })?;
    command.stdin(stdin);
    capture(command, budget, deadline)
}

/// The largest input [`piped_input`] takes: one page, which the pipe buffer
/// of every supported platform holds, so writing it before the reader exists
/// cannot block.
const PIPED_INPUT_LIMIT: usize = 4096;

/// A pipe holding `input`, its writing end already closed: the reader sees
/// `input` and then the end of input. Nothing is written to disk.
fn piped_input(input: &[u8]) -> std::io::Result<std::io::PipeReader> {
    use std::io::Write;
    if input.len() > PIPED_INPUT_LIMIT {
        return Err(std::io::Error::other("input does not fit the pipe buffer"));
    }
    let (reader, mut writer) = std::io::pipe()?;
    writer.write_all(input)?;
    Ok(reader)
}

/// One command line for `security -i`, which reads its commands from
/// standard input: every word in double quotes, `"` and `\` escaped. `None`
/// when a word holds a line break or NUL, which no quoting carries.
fn security_command(words: &[&str]) -> Option<String> {
    let mut line = String::new();
    for word in words {
        if word.contains(['\n', '\r', '\0']) {
            return None;
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push('"');
        line.push_str(&word.replace('\\', "\\\\").replace('"', "\\\""));
        line.push('"');
    }
    line.push('\n');
    Some(line)
}

fn capture(
    command: &mut Command,
    budget: Duration,
    deadline: Instant,
) -> Result<Option<std::process::Output>, SigningStopped> {
    let now = Instant::now();
    let accept_by = deadline.min(now + budget);
    let left = accept_by.saturating_duration_since(now);
    if left.is_zero() {
        #[cfg(test)]
        test_scope::note_run(RunReason::DeadlineElapsed);
        return Ok(None);
    }
    let captured = crate::core::process_capture::run_with_output_limits(
        command,
        Some(left),
        STDOUT_LIMIT,
        STDERR_LIMIT,
    )
    .map_err(|_| {
        #[cfg(test)]
        test_scope::note_run(RunReason::CaptureRejected);
        SigningStopped
    })?;
    // One fresh instant decides acceptance; the test note reads the same one.
    let completed = Instant::now();
    #[cfg(test)]
    test_scope::note_run(capture_reason(
        captured.timed_out,
        captured.cancelled,
        completed >= accept_by,
    ));
    Ok(accepted(captured, accept_by, completed))
}

/// The answer of a capture that returned normally, if it is one: the tool
/// ended by itself, and `now` — read after the capture returned — is still
/// before the instant fixed when the step began.
fn accepted(
    captured: crate::core::process_capture::CapturedOutput,
    accept_by: Instant,
    now: Instant,
) -> Option<std::process::Output> {
    (!captured.timed_out && !captured.cancelled && now < accept_by).then_some(captured.output)
}

/// Signing was stopped because a signing tool's process could not be run and
/// confirmed ended. The file must not be used as signed, sealed or installed
/// on the strength of this call. Carries no detail by design: neither
/// arguments nor anything a tool printed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SigningStopped;

impl std::fmt::Display for SigningStopped {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("code signing stopped: a signing tool could not be confirmed ended")
    }
}

/// Full path to the dedicated lean-ctx signing keychain.
pub(crate) fn keychain_path() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join("Library/Keychains/lean-ctx-codesign.keychain-db"))
}

/// File holding the dedicated keychain's password (0600, in the data dir).
pub(crate) fn password_path() -> Option<PathBuf> {
    Some(
        crate::core::paths::data_dir()
            .ok()?
            .join("codesign-keychain.pw"),
    )
}

/// Is the persistent identity present AND trusted for code signing?
/// Pure query — reads only the certificate, never prompts. Bounded: a
/// `security` that does not answer in time counts as "not ready".
pub(crate) fn is_ready() -> bool {
    let context = SigningContext::current();
    ready(&context, Instant::now() + context.budgets.query).unwrap_or(false)
}

fn ready(context: &SigningContext, deadline: Instant) -> Result<bool, SigningStopped> {
    #[cfg(test)]
    let _phase = test_scope::phase(Stage::Query);
    let Some(keychain) = &context.keychain else {
        #[cfg(test)]
        test_scope::note_query(QueryReason::KeychainAbsent, false);
        return Ok(false);
    };
    if !keychain.exists() {
        #[cfg(test)]
        test_scope::note_query(QueryReason::KeychainAbsent, false);
        return Ok(false);
    }
    let Some(out) = run(
        Command::new(&context.security)
            .args(["find-identity", "-v", "-p", "codesigning"])
            .arg(keychain),
        context.budgets.query,
        deadline,
    )?
    else {
        return Ok(false);
    };
    // `find-identity` annotates broken/untrusted identities in parentheses
    // (e.g. `"lean-ctx-codesign" (Invalid Key Usage for policy)` or
    // `(CSSMERR_TP_NOT_TRUSTED)`). Treat the identity as ready only when its
    // line carries the CN with no such annotation. As before, the tool's exit
    // status is not consulted: the listing alone decides.
    let has_cn = String::from_utf8_lossy(&out.stdout)
        .lines()
        .any(|line| line.contains(IDENTITY_CN) && !line.contains('('));
    #[cfg(test)]
    test_scope::note_query(
        query_reason(out.status.success(), out.stdout.is_empty(), has_cn),
        has_cn,
    );
    Ok(has_cn)
}

/// Sign `binary`, preferring the stable identity; otherwise the ad-hoc
/// fallback is attempted, which is what keeps a binary launchable before
/// [`setup_identity`] has been run. `SignKind::AdHoc` reports that attempt,
/// not its success.
///
/// One deadline covers the answers of all steps. Query, unlock and the
/// stable signature are accepted only while the fallback's reserved slice is
/// still left; no step resets the clock for the next.
///
/// `Err` when a tool's process could not be run and confirmed ended: then no
/// further tool is started — not the fallback either — and the caller must
/// stop instead of reading, sealing or installing `binary`.
pub(crate) fn sign_binary(binary: &Path) -> Result<SignKind, SigningStopped> {
    let context = SigningContext::current();
    let started = Instant::now();
    let budgets = context.budgets;
    let stable_by = started + budgets.total.saturating_sub(budgets.adhoc);
    if ready(&context, stable_by)? && sign_with_identity(&context, binary, stable_by)? {
        return Ok(SignKind::Stable);
    }
    adhoc_sign(&context, binary, started + budgets.total)?;
    Ok(SignKind::AdHoc)
}

/// Whether `codesign` signed with the persistent identity, in time. Nothing
/// the tools print is kept: a failed signature is reported by the fallback
/// being taken, not by forwarding diagnostics.
fn sign_with_identity(
    context: &SigningContext,
    binary: &Path,
    deadline: Instant,
) -> Result<bool, SigningStopped> {
    let Some(keychain) = &context.keychain else {
        return Ok(false);
    };
    let unlock = context.password().and_then(|password| {
        security_command(&["unlock-keychain", "-p", &password, keychain.to_str()?])
    });
    if let Some(unlock) = unlock {
        // Best-effort unlock: whether it unlocked is not consulted, signing
        // fails below if the keychain is still locked. That its process is
        // over is not best effort. The password goes in on standard input
        // (`security -i`), never in the arguments.
        #[cfg(test)]
        let _phase = test_scope::phase(Stage::Unlock);
        run_with_input(
            Command::new(&context.security).arg("-i"),
            unlock.as_bytes(),
            context.budgets.unlock,
            deadline,
        )?;
    }
    #[cfg(test)]
    let _phase = test_scope::phase(Stage::Stable);
    Ok(run(
        Command::new(&context.codesign)
            .args([
                "--force",
                "--timestamp=none",
                "--identifier",
                CODESIGN_IDENTIFIER,
                "--keychain",
            ])
            .arg(keychain)
            .args(["--sign", IDENTITY_CN])
            .arg(binary),
        context.budgets.stable,
        deadline,
    )?
    .is_some_and(|out| out.status.success()))
}

/// Ad-hoc signature — keeps the binary launchable, but offers TCC no stable
/// anchor (the historical behaviour that caused #356). Whether it signed is
/// best effort and not reported; that its process is over is not.
fn adhoc_sign(
    context: &SigningContext,
    binary: &Path,
    deadline: Instant,
) -> Result<(), SigningStopped> {
    #[cfg(test)]
    let _phase = test_scope::phase(Stage::AdHoc);
    run(
        Command::new(&context.codesign)
            .args(["--force", "-s", "-"])
            .arg(binary),
        context.budgets.adhoc,
        deadline,
    )
    .map(|_| ())
}

/// A signing context for the tests of the thread that installs it, put back
/// when its guard goes, also when the test unwinds. Tests that sign — the
/// updater's among them — thereby never start the real tools, never open the
/// user's keychain and never read its password.
#[cfg(test)]
pub(crate) mod test_scope {
    use super::{Budgets, OsString, SigningContext};
    use std::cell::{Cell, RefCell};

    thread_local! {
        static SCOPED: RefCell<Option<SigningContext>> = const { RefCell::new(None) };
        static NOTES: Cell<Notes> = const { Cell::new(Notes::EMPTY) };
    }

    pub(super) fn current() -> Option<SigningContext> {
        SCOPED.with(|scoped| scoped.borrow().clone())
    }

    /// Restores what was scoped before it, and that scope's notes; not
    /// `Send`: the scope is this thread's.
    pub(crate) struct Scoped {
        previous: Option<SigningContext>,
        previous_notes: Notes,
        _this_thread: std::marker::PhantomData<*const ()>,
    }

    impl Drop for Scoped {
        fn drop(&mut self) {
            SCOPED.with(|scoped| *scoped.borrow_mut() = self.previous.take());
            NOTES.set(self.previous_notes);
        }
    }

    pub(super) fn install(context: SigningContext) -> Scoped {
        Scoped {
            previous: SCOPED.with(|scoped| scoped.borrow_mut().replace(context)),
            previous_notes: NOTES.replace(Notes::EMPTY),
            _this_thread: std::marker::PhantomData,
        }
    }

    /// The step whose tool runs; its index is its slot in [`slots`].
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) enum Stage {
        Query,
        Unlock,
        Stable,
        AdHoc,
    }

    /// Why a step's run gave what it gave. A fixed word: no path, argument,
    /// output, exit code or error text.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) enum RunReason {
        DeadlineElapsed,
        CaptureRejected,
        TimedOut,
        Cancelled,
        LateAccepted,
        Accepted,
    }

    /// What the readiness query saw. Observation only: `Nonzero` with the CN
    /// listed is still ready, as the listing alone decides.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) enum QueryReason {
        KeychainAbsent,
        Nonzero,
        Empty,
        NoCN,
        Ready,
    }

    /// The last note of one step in the current scope.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) struct Slot {
        pub(super) run: Option<RunReason>,
        pub(super) query: Option<QueryReason>,
        pub(super) has_cn: bool,
    }

    impl Slot {
        pub(super) const EMPTY: Self = Self {
            run: None,
            query: None,
            has_cn: false,
        };
    }

    #[derive(Clone, Copy)]
    struct Notes {
        phase: Option<Stage>,
        slots: [Slot; 4],
    }

    impl Notes {
        const EMPTY: Self = Self {
            phase: None,
            slots: [Slot::EMPTY; 4],
        };
    }

    pub(super) fn capture_reason(timed_out: bool, cancelled: bool, late: bool) -> RunReason {
        if timed_out {
            RunReason::TimedOut
        } else if cancelled {
            RunReason::Cancelled
        } else if late {
            RunReason::LateAccepted
        } else {
            RunReason::Accepted
        }
    }

    pub(super) fn query_reason(success: bool, empty: bool, has_cn: bool) -> QueryReason {
        if !success {
            QueryReason::Nonzero
        } else if empty {
            QueryReason::Empty
        } else if !has_cn {
            QueryReason::NoCN
        } else {
            QueryReason::Ready
        }
    }

    /// Names the step that runs next on this thread; puts the previous one
    /// back when it goes, also when the step unwinds.
    pub(super) struct Phase(Option<Stage>);

    impl Drop for Phase {
        fn drop(&mut self) {
            let mut notes = NOTES.get();
            notes.phase = self.0;
            NOTES.set(notes);
        }
    }

    pub(super) fn phase(stage: Stage) -> Phase {
        let mut notes = NOTES.get();
        let previous = notes.phase.replace(stage);
        NOTES.set(notes);
        Phase(previous)
    }

    /// Notes only inside an installed scope; never read by signing itself.
    fn note(stage: Stage, write: impl FnOnce(&mut Slot)) {
        if SCOPED.with(|scoped| scoped.borrow().is_none()) {
            return;
        }
        let mut notes = NOTES.get();
        write(&mut notes.slots[stage as usize]);
        NOTES.set(notes);
    }

    pub(super) fn note_run(reason: RunReason) {
        if let Some(stage) = NOTES.get().phase {
            note(stage, |slot| slot.run = Some(reason));
        }
    }

    pub(super) fn note_query(reason: QueryReason, has_cn: bool) {
        note(Stage::Query, |slot| {
            slot.query = Some(reason);
            slot.has_cn = has_cn;
        });
    }

    /// The notes of this thread's scope, in [`Stage`] order.
    pub(super) fn slots() -> [Slot; 4] {
        NOTES.get().slots
    }

    /// Tools owned by the caller in `directory`: executable stand-ins named
    /// `security` and `codesign`, a keychain file and a password file there.
    /// Production budgets unless `budgets` shortens them.
    pub(super) fn owned(directory: &std::path::Path, budgets: Option<Budgets>) -> SigningContext {
        SigningContext {
            keychain: Some(directory.join("test.keychain-db")),
            password_file: Some(directory.join("keychain.pw")),
            security: OsString::from(directory.join("security")),
            codesign: OsString::from(directory.join("codesign")),
            budgets: budgets.unwrap_or(Budgets::PRODUCTION),
        }
    }

    /// For tests of other modules that sign as a side effect: stand-in tools
    /// in `directory` that know no identity and whose ad-hoc `codesign`
    /// leaves the file as it is and succeeds. This shows the caller's control
    /// flow around signing; it is no signature and proves none.
    #[cfg(unix)]
    pub(crate) fn without_identity(directory: &std::path::Path) -> Scoped {
        let script = |name: &str, body: &str| {
            use std::os::unix::fs::PermissionsExt;
            let path = directory.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        };
        script("security", "exit 0");
        script("codesign", "exit 0");
        std::fs::write(directory.join("test.keychain-db"), b"").unwrap();
        install(owned(directory, None))
    }

    /// For tests of where a caller signs: stand-in tools in `directory` that
    /// know no identity and whose `codesign` appends `+signed` to the file it
    /// is given and notes that file's path in `directory/signed-paths`.
    #[cfg(unix)]
    pub(crate) fn marking(directory: &std::path::Path) -> Scoped {
        use std::os::unix::fs::PermissionsExt;
        let script = |name: &str, body: String| {
            let path = directory.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        };
        script("security", "exit 0".into());
        script(
            "codesign",
            format!(
                "for last in \"$@\"; do :; done\n\
                 printf '%s\\n' \"$last\" >> '{}'\nprintf '+signed' >> \"$last\"",
                directory.join("signed-paths").display()
            ),
        );
        std::fs::write(directory.join("test.keychain-db"), b"").unwrap();
        install(owned(directory, None))
    }

    /// For tests of what a caller does when signing stops: a keychain is
    /// there, the tools in `directory` are not, so the first step cannot be
    /// started and the capture reports an error.
    pub(crate) fn unconfirmable(directory: &std::path::Path) -> Scoped {
        std::fs::write(directory.join("test.keychain-db"), b"").unwrap();
        install(owned(directory, None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keychain_path_is_dedicated_not_login() {
        let kc = keychain_path().expect("home dir");
        assert!(kc.ends_with("lean-ctx-codesign.keychain-db"));
        // Must never be the login keychain — we manage our own.
        assert!(!kc.to_string_lossy().contains("login.keychain"));
    }

    #[test]
    fn password_lives_in_data_dir() {
        let p = password_path().expect("data dir");
        assert!(p.ends_with("codesign-keychain.pw"));
    }

    #[test]
    fn identifier_is_stable_and_reverse_dns() {
        // The TCC Designated Requirement is keyed to this — it must not drift.
        assert_eq!(CODESIGN_IDENTIFIER, "com.leanctx.cli");
        assert_eq!(IDENTITY_CN, "lean-ctx-codesign");
    }

    #[test]
    fn sign_kind_is_comparable() {
        assert_ne!(SignKind::Stable, SignKind::AdHoc);
    }

    #[test]
    fn readiness_notes_keep_a_fixed_reason_per_step_and_scope() {
        use test_scope::Slot;
        for (timed_out, cancelled, late, expected) in [
            (true, true, true, RunReason::TimedOut),
            (false, true, true, RunReason::Cancelled),
            (false, false, true, RunReason::LateAccepted),
            (false, false, false, RunReason::Accepted),
        ] {
            assert_eq!(capture_reason(timed_out, cancelled, late), expected);
        }
        // A nonzero exit is noted, never decisive: with the CN listed the
        // listing still says ready.
        for (success, empty, has_cn, expected) in [
            (false, false, true, QueryReason::Nonzero),
            (false, true, false, QueryReason::Nonzero),
            (true, true, false, QueryReason::Empty),
            (true, false, false, QueryReason::NoCN),
            (true, false, true, QueryReason::Ready),
        ] {
            assert_eq!(query_reason(success, empty, has_cn), expected);
        }

        // No keychain and tools that do not exist. Not pure: signing reads the
        // clock, and the inner signing below attempts to capture a tool at a
        // path that does not exist; that start fails, so no tool runs.
        let context = |budgets| SigningContext {
            keychain: None,
            password_file: None,
            security: OsString::from("/nonexistent/leanctx-security"),
            codesign: OsString::from("/nonexistent/leanctx-codesign"),
            budgets,
        };
        let binary = Path::new("/nonexistent/leanctx-binary");
        let at = |stage: Stage| test_scope::slots()[stage as usize];
        let unscoped = test_scope::slots();
        let outer_scope = test_scope::install(context(Budgets {
            query: Duration::ZERO,
            unlock: Duration::ZERO,
            stable: Duration::ZERO,
            adhoc: Duration::ZERO,
            total: Duration::ZERO,
        }));
        // Absent before any query; the fallback's instant has passed, so it
        // starts nothing.
        assert_eq!(sign_binary(binary), Ok(SignKind::AdHoc));
        assert_eq!(at(Stage::Query).query, Some(QueryReason::KeychainAbsent));
        assert_eq!(at(Stage::AdHoc).run, Some(RunReason::DeadlineElapsed));
        let outer = test_scope::slots();
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _inner = test_scope::install(context(Budgets::PRODUCTION));
            assert_eq!(test_scope::slots(), [Slot::EMPTY; 4]);
            // A tool that cannot be started is an error of the capture.
            assert_eq!(sign_binary(binary), Err(SigningStopped));
            assert_eq!(at(Stage::AdHoc).run, Some(RunReason::CaptureRejected));
            assert_eq!(at(Stage::Unlock), Slot::EMPTY);
            let _step = test_scope::phase(Stage::Stable);
            panic!("unwinding inside a step");
        }));
        assert!(unwound.is_err());
        assert_eq!(test_scope::slots(), outer);
        // The step was put back: a note outside any step lands nowhere.
        test_scope::note_run(RunReason::Accepted);
        assert_eq!(test_scope::slots(), outer);
        drop(outer_scope);
        assert_eq!(test_scope::slots(), unscoped);
        test_scope::note_query(QueryReason::Ready, true);
        assert_eq!(test_scope::slots(), unscoped);
    }
}

/// Signing against stand-in tools this test owns. A stand-in that exits 0
/// shows the control flow around the real tools — order, arguments, bounds,
/// cleanup — and nothing about a macOS signature or a TCC grant.
#[cfg(all(test, unix))]
mod bounded_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    const PASSWORD: &str = "sentinel-keychain-password";
    const IDENTITY: &str = "  1) 0123456789ABCDEF \"lean-ctx-codesign\"\n";

    /// The stand-ins and what they recorded: one line per start in `calls`.
    struct Tools {
        directory: tempfile::TempDir,
        _scope: test_scope::Scoped,
    }

    impl Tools {
        /// `security` prints the file `listing` on `find-identity` and records
        /// every start; `codesign` records its start and runs `codesign_body`
        /// with `$mode` set to `stable` or `adhoc`.
        fn new(listing: &[u8], codesign_body: &str, budgets: Budgets) -> Self {
            Self::with_slow_query("", listing, codesign_body, budgets)
        }

        /// As [`Self::new`]; `security` runs `before_answer` after recording
        /// its start and before it answers.
        fn with_slow_query(
            before_answer: &str,
            listing: &[u8],
            codesign_body: &str,
            budgets: Budgets,
        ) -> Self {
            let directory = tempfile::tempdir().unwrap();
            let at = |name: &str| directory.path().join(name);
            let script = |name: &str, body: String| {
                std::fs::write(at(name), format!("#!/bin/sh\n{body}\n")).unwrap();
                std::fs::set_permissions(at(name), std::fs::Permissions::from_mode(0o700)).unwrap();
            };
            std::fs::write(at("listing"), listing).unwrap();
            std::fs::write(at("test.keychain-db"), b"").unwrap();
            std::fs::write(at("keychain.pw"), format!("{PASSWORD}\n")).unwrap();
            let calls = at("calls");
            script(
                "security",
                // `security -i` reads its command from standard input: that
                // line is recorded as `stdin …`, its first word is `$cmd`.
                format!(
                    "echo \"security $*\" >> '{calls}'\ncmd=$1\n\
                     if [ \"$1\" = -i ]; then input=$(cat); echo \"stdin $input\" >> '{calls}'; \
                     cmd=${{input%% *}}; cmd=${{cmd#\\\"}}; cmd=${{cmd%\\\"}}; fi\n{before_answer}\n\
                     [ \"$cmd\" = find-identity ] && cat '{listing}'\nexit ${{status:-0}}",
                    calls = calls.display(),
                    listing = at("listing").display()
                ),
            );
            script(
                "codesign",
                format!(
                    "mode=adhoc; for a in \"$@\"; do [ \"$a\" = --sign ] && mode=stable; done\n\
                     echo \"codesign $mode $*\" >> '{calls}'\n{codesign_body}",
                    calls = calls.display()
                ),
            );
            let scope = test_scope::install(test_scope::owned(directory.path(), Some(budgets)));
            Self {
                directory,
                _scope: scope,
            }
        }

        fn at(&self, name: &str) -> PathBuf {
            self.directory.path().join(name)
        }

        fn calls(&self) -> Vec<String> {
            std::fs::read_to_string(self.at("calls"))
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect()
        }
    }

    fn alive(pid: &str) -> bool {
        Command::new("kill")
            .args(["-0", pid.trim()])
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    /// A process of this test that no signing step started. Ended and reaped
    /// when the guard goes, also when the test unwinds.
    struct Bystander(std::process::Child);

    impl Bystander {
        fn start() -> Self {
            Self(Command::new("sleep").arg("600").spawn().unwrap())
        }

        fn still_running(&mut self) -> bool {
            self.0.try_wait().unwrap().is_none()
        }
    }

    impl Drop for Bystander {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn the_stable_path_runs_query_unlock_and_sign_in_order_with_the_fixed_arguments() {
        let tools = Tools::new(IDENTITY.as_bytes(), "exit 0", Budgets::PRODUCTION);
        let binary = tools.at("binary");
        std::fs::write(&binary, b"bytes").unwrap();
        assert!(is_ready(), "{:?}", test_scope::slots());
        assert_eq!(sign_binary(&binary), Ok(SignKind::Stable));
        let keychain = tools.at("test.keychain-db");
        let calls = tools.calls();
        assert_eq!(
            calls,
            [
                // `is_ready` above, then the three steps of one signing.
                format!(
                    "security find-identity -v -p codesigning {}",
                    keychain.display()
                ),
                format!(
                    "security find-identity -v -p codesigning {}",
                    keychain.display()
                ),
                "security -i".to_string(),
                format!(
                    "stdin \"unlock-keychain\" \"-p\" \"{PASSWORD}\" \"{}\"",
                    keychain.display()
                ),
                format!(
                    "codesign stable --force --timestamp=none --identifier com.leanctx.cli \
                     --keychain {} --sign lean-ctx-codesign {}",
                    keychain.display(),
                    binary.display()
                ),
            ]
        );
        // The password goes to the unlock's standard input and nowhere else:
        // no tool's arguments carry it.
        assert_eq!(
            calls.iter().filter(|call| call.contains(PASSWORD)).count(),
            1
        );
        assert!(
            calls
                .iter()
                .filter(|call| !call.starts_with("stdin "))
                .all(|call| !call.contains(PASSWORD))
        );
    }

    #[test]
    fn security_commands_quote_every_word_and_refuse_line_breaks() {
        assert_eq!(
            security_command(&["unlock-keychain", "-p", "a\"b\\c d", "/k c/x"]).as_deref(),
            Some("\"unlock-keychain\" \"-p\" \"a\\\"b\\\\c d\" \"/k c/x\"\n")
        );
        assert_eq!(security_command(&["unlock-keychain", "-p", "a\nb"]), None);
        assert_eq!(security_command(&["unlock-keychain", "-p", "a\0b"]), None);
        assert!(piped_input(&[b'x'; PIPED_INPUT_LIMIT + 1]).is_err());
    }

    #[test]
    fn a_failed_or_annotated_identity_falls_back_to_one_ad_hoc_attempt() {
        // The identity is listed but signing with it fails.
        let tools = Tools::new(
            IDENTITY.as_bytes(),
            "[ $mode = stable ] && exit 1; exit 0",
            Budgets::PRODUCTION,
        );
        let binary = tools.at("binary");
        std::fs::write(&binary, b"bytes").unwrap();
        assert_eq!(sign_binary(&binary), Ok(SignKind::AdHoc));
        let codesign: Vec<_> = tools
            .calls()
            .into_iter()
            .filter(|call| call.starts_with("codesign"))
            .collect();
        assert_eq!(
            codesign,
            [
                codesign[0].clone(),
                format!("codesign adhoc --force -s - {}", binary.display())
            ]
        );
        assert!(codesign[0].starts_with("codesign stable "));
        drop(tools);

        // An identity the listing annotates is not ready: no stable attempt.
        let listing = "  1) 0123 \"lean-ctx-codesign\" (CSSMERR_TP_NOT_TRUSTED)\n";
        let tools = Tools::new(listing.as_bytes(), "exit 0", Budgets::PRODUCTION);
        assert!(!is_ready());
        assert_eq!(sign_binary(&tools.at("binary")), Ok(SignKind::AdHoc));
        assert!(
            !tools
                .calls()
                .iter()
                .any(|call| call.starts_with("codesign stable") || call.contains("unlock"))
        );
    }

    #[test]
    fn a_tool_that_does_not_answer_is_ended_before_the_fallback_and_nothing_else_is_touched() {
        // Not this signing's child: it must still be there afterwards.
        let mut bystander = Bystander::start();
        let tools = Tools::new(
            IDENTITY.as_bytes(),
            // The stable signature hangs; the fallback notes whether the hung
            // process is still there when it starts.
            "if [ $mode = stable ]; then echo $$ > \"$0.hung\"; exec sleep 600; fi\n\
             kill -0 \"$(cat \"$0.hung\")\" 2>/dev/null && echo alive > \"$0.seen\" \
             || echo gone > \"$0.seen\"\nexit 0",
            Budgets {
                // Long enough for the stand-in to start and note its pid on a
                // busy test host; it would otherwise sleep for ten minutes.
                stable: Duration::from_secs(3),
                ..Budgets::PRODUCTION
            },
        );
        let started = Instant::now();
        assert_eq!(
            sign_binary(&tools.at("binary")),
            Ok(SignKind::AdHoc),
            "{:?}",
            test_scope::slots()
        );
        // Bounded by the step's own budget, not by the tool: far below the
        // ten minutes it would have taken, and below the whole signing budget.
        assert!(started.elapsed() < Budgets::PRODUCTION.total);
        let hung = std::fs::read_to_string(tools.at("codesign.hung"))
            .unwrap_or_else(|error| panic!("{error}; {:?}", test_scope::slots()));
        assert_eq!(
            std::fs::read_to_string(tools.at("codesign.seen")).unwrap(),
            "gone\n",
            "the hung signature was not ended before the fallback started"
        );
        assert!(!alive(&hung));
        assert!(bystander.still_running(), "a foreign process was ended");
    }

    #[test]
    fn a_hanging_unlock_or_fallback_is_bounded_too() {
        // The unlock never answers: it is cut at its own budget and the
        // stable signature is still attempted with the time that is left.
        let tools = Tools::with_slow_query(
            "[ \"$cmd\" = unlock-keychain ] && exec sleep 600",
            IDENTITY.as_bytes(),
            "exit 0",
            Budgets {
                unlock: Duration::from_secs(1),
                ..Budgets::PRODUCTION
            },
        );
        assert_eq!(sign_binary(&tools.at("binary")), Ok(SignKind::Stable));
        drop(tools);

        // The stable signature fails and the fallback itself hangs: the
        // attempt is reported, in bounded time, and the hung tool is gone.
        let tools = Tools::new(
            IDENTITY.as_bytes(),
            "[ $mode = stable ] && exit 1\necho $$ > \"$0.hung\"; exec sleep 600",
            Budgets {
                adhoc: Duration::from_secs(3),
                ..Budgets::PRODUCTION
            },
        );
        let started = Instant::now();
        assert_eq!(sign_binary(&tools.at("binary")), Ok(SignKind::AdHoc));
        assert!(started.elapsed() < Budgets::PRODUCTION.total);
        assert!(!alive(
            &std::fs::read_to_string(tools.at("codesign.hung")).unwrap()
        ));
    }

    #[test]
    fn readiness_is_the_listing_alone_and_an_answer_past_its_instant_is_none() {
        // As before this change: `security` may exit non-zero and still list
        // the identity; the listing decides.
        let tools = Tools::with_slow_query(
            "status=3",
            IDENTITY.as_bytes(),
            "exit 0",
            Budgets::PRODUCTION,
        );
        assert!(is_ready());
        drop(tools);

        // Diagnostics have their own limit: at it the answer counts, one
        // byte over it there is no answer.
        for (bytes, answered) in [(STDERR_LIMIT, true), (STDERR_LIMIT + 1, false)] {
            let tools = Tools::with_slow_query(
                &format!("head -c {bytes} /dev/zero | tr '\\0' e >&2"),
                IDENTITY.as_bytes(),
                "exit 0",
                Budgets::PRODUCTION,
            );
            assert_eq!(is_ready(), answered, "{bytes} bytes of diagnostics");
            drop(tools);
        }

        // An instant that has passed starts nothing at all.
        let tools = Tools::new(IDENTITY.as_bytes(), "exit 0", Budgets::PRODUCTION);
        let context = SigningContext::current();
        assert_eq!(ready(&context, Instant::now()), Ok(false));
        assert!(tools.calls().is_empty());

        // An answer is one only while its instant has not passed, whatever
        // the tool's exit: the capture returning normally is not enough.
        let returned = || crate::core::process_capture::CapturedOutput {
            output: std::process::Output {
                status: std::os::unix::process::ExitStatusExt::from_raw(0),
                stdout: IDENTITY.as_bytes().to_vec(),
                stderr: Vec::new(),
            },
            timed_out: false,
            cancelled: false,
        };
        let in_time = Instant::now();
        let accept_by = in_time + Duration::from_secs(30);
        assert!(accepted(returned(), accept_by, in_time).is_some_and(|out| out.status.success()));
        assert!(accepted(returned(), accept_by, accept_by).is_none());
        assert!(accepted(returned(), accept_by, accept_by + Duration::from_secs(1)).is_none());
    }

    #[test]
    fn a_step_that_cannot_be_confirmed_over_stops_the_signing_before_any_further_tool() {
        // The capture reports an error for a tool that cannot be started and
        // for one that floods its output; no error text is read to tell them
        // from a failed cleanup, so each stops the whole signing.
        let tools = Tools::new(IDENTITY.as_bytes(), "exit 0", Budgets::PRODUCTION);
        std::fs::remove_file(tools.at("security")).unwrap();
        assert!(!is_ready());
        assert_eq!(sign_binary(&tools.at("binary")), Err(SigningStopped));
        assert!(tools.calls().is_empty(), "a tool ran after the stop");
        drop(tools);

        let flood = "head -c 20000 /dev/zero | tr '\\0' e >&2";
        // The stable signature floods: no fallback follows it.
        let tools = Tools::new(
            IDENTITY.as_bytes(),
            &format!("[ $mode = stable ] && {flood}\nexit 0"),
            Budgets::PRODUCTION,
        );
        assert_eq!(sign_binary(&tools.at("binary")), Err(SigningStopped));
        let calls = tools.calls();
        assert!(
            calls
                .last()
                .is_some_and(|call| call.starts_with("codesign stable"))
        );
        assert!(!calls.iter().any(|call| call.starts_with("codesign adhoc")));
        drop(tools);

        // The fallback itself floods: reported, not passed off as an attempt.
        let tools = Tools::new(
            IDENTITY.as_bytes(),
            &format!("[ $mode = stable ] && exit 1\n{flood}\nexit 0"),
            Budgets::PRODUCTION,
        );
        assert_eq!(sign_binary(&tools.at("binary")), Err(SigningStopped));
        // Nothing of what a tool printed, and no argument, is in the error.
        assert_eq!(
            SigningStopped.to_string(),
            "code signing stopped: a signing tool could not be confirmed ended"
        );
    }

    #[test]
    fn the_bystander_guard_ends_exactly_its_own_process_when_a_test_unwinds() {
        let mut other = Bystander::start();
        let pid = std::cell::Cell::new(0);
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let bystander = Bystander::start();
            pid.set(bystander.0.id());
            panic!("unwinding with a bystander running");
        }));
        assert!(unwound.is_err());
        assert!(
            !alive(&pid.get().to_string()),
            "the unwound guard left its process"
        );
        assert!(other.still_running(), "another guard's process was ended");
    }

    #[test]
    fn the_steps_share_one_deadline_and_the_fallback_keeps_its_reserve() {
        // The query uses up the time before the fallback's reserve; the
        // stable signature, though its own budget is ten seconds, gets none.
        let budgets = Budgets {
            query: Duration::from_secs(5),
            unlock: Duration::from_secs(5),
            stable: Duration::from_secs(10),
            adhoc: Duration::from_secs(5),
            // Three seconds before the reserve begins — room for a stand-in
            // to start on a busy test host; the query outlasts them.
            total: Duration::from_secs(8),
        };
        let tools = Tools::with_slow_query("sleep 30", IDENTITY.as_bytes(), "exit 0", budgets);
        let started = Instant::now();
        assert_eq!(sign_binary(&tools.at("binary")), Ok(SignKind::AdHoc));
        // Cut at the shared deadline: long before the query's thirty seconds
        // and before the whole signing's eight.
        assert!(started.elapsed() < budgets.total);
        let calls = tools.calls();
        // The query was cut at the shared deadline, so nothing was unlocked
        // or signed with the identity; the fallback still ran.
        assert_eq!(calls.len(), 2, "{calls:?} {:?}", test_scope::slots());
        assert!(calls[0].starts_with("security find-identity"));
        assert!(calls[1].starts_with("codesign adhoc"));
    }

    #[test]
    fn an_answer_is_read_up_to_its_limit_and_one_byte_more_is_no_answer() {
        let listing = |bytes: usize| {
            let mut listing = IDENTITY.as_bytes().to_vec();
            listing.resize(bytes, b'\n');
            listing
        };
        let tools = Tools::new(&listing(STDOUT_LIMIT), "exit 0", Budgets::PRODUCTION);
        assert!(is_ready());
        drop(tools);
        let tools = Tools::new(&listing(STDOUT_LIMIT + 1), "exit 0", Budgets::PRODUCTION);
        assert!(!is_ready());
        // A flood is an error of the capture: the signing stops, and neither
        // signature is attempted after it.
        assert_eq!(sign_binary(&tools.at("binary")), Err(SigningStopped));
        assert!(
            !tools
                .calls()
                .iter()
                .any(|call| call.starts_with("codesign"))
        );
    }

    #[test]
    fn the_scope_is_this_threads_and_is_put_back_also_when_a_test_unwinds() {
        let production = SigningContext::production();
        let same = |context: &SigningContext| {
            context.keychain == production.keychain && context.security == production.security
        };
        assert!(same(&SigningContext::current()));
        let directory = tempfile::tempdir().unwrap();
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _scope = test_scope::install(test_scope::owned(directory.path(), None));
            assert!(!same(&SigningContext::current()));
            // Another thread does not see this thread's scope.
            assert!(
                std::thread::spawn(|| SigningContext::current().security == *"security")
                    .join()
                    .unwrap()
            );
            panic!("unwinding with the scope installed");
        }));
        assert!(unwound.is_err());
        assert!(same(&SigningContext::current()));
    }
}
