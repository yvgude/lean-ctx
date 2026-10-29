//! `lean-ctx eval` — deterministic with/without output-quality eval CLI (#238).
//!
//! Subcommands:
//! * `eval init <dir>`     — scaffold a runnable starter suite (one QA + one code task).
//! * `eval ab --suite P`   — run the A/B comparison and write a signed, reproducible artifact.
//! * `eval verify <file>`  — verify an artifact's signature + determinism digest offline.

use std::path::{Path, PathBuf};

use crate::core::eval_ab::arm_study::{self, Arm, ProviderCondition};
use crate::core::eval_ab::artifact::{self, SignedAbReportV1};
use crate::core::eval_ab::conditions::Condition;
use crate::core::eval_ab::footprint::{
    Footprint, FootprintConfig, FootprintReport, run_footprint_ab,
};
use crate::core::eval_ab::frontier::{self, FrontierReport};
use crate::core::eval_ab::model::{ModelRunner, OpenAiRunner, RecordedRunner, RecordingRunner};
use crate::core::eval_ab::report::ReportConfig;
use crate::core::eval_ab::suite::EvalSuite;
use crate::core::eval_ab::testbench::lockfile::TestbenchLock;
use crate::core::eval_ab::testbench::{self, TestbenchConfig, TestbenchReport, findings};
use crate::core::eval_ab::{AbRunConfig, run_ab};

/// Entry point dispatched from `cli::dispatch`.
pub fn cmd_eval(args: &[String]) {
    // `eval --delta [opts]` is sugar for the footprint subcommand.
    if args.iter().any(|a| a == "--delta") {
        let rest: Vec<String> = args.iter().filter(|a| *a != "--delta").cloned().collect();
        return cmd_footprint(&rest);
    }
    match args.first().map(String::as_str) {
        Some("ab") => cmd_ab(&args[1..]),
        Some("frontier") => cmd_frontier(&args[1..]),
        Some("footprint" | "delta") => cmd_footprint(&args[1..]),
        Some("testbench") => cmd_testbench(&args[1..]),
        Some("verify") => cmd_verify(&args[1..]),
        Some("arm-study") => cmd_arm_study(&args[1..]),
        Some("init") => cmd_init(&args[1..]),
        Some("-h" | "--help") | None => print_help(),
        Some(other) => {
            eprintln!("eval: unknown subcommand '{other}'\n");
            print_help();
            std::process::exit(2);
        }
    }
}

fn print_help() {
    println!(
        "lean-ctx eval — deterministic with/without output-quality proof\n\n\
USAGE:\n\
  lean-ctx eval init <dir>                 Scaffold a runnable starter suite\n\
  lean-ctx eval ab --suite <file> [opts]   Run the A/B quality comparison\n\
  lean-ctx eval footprint --suite <f> [o]  Ablate lean-ctx's OWN injected context (#959)\n\
  lean-ctx eval testbench --lock <f> [o]   Off-vs-on across pinned real repos (#611)\n\
  lean-ctx eval verify <artifact.json>     Verify signature + determinism digest\n\n\
ab OPTIONS:\n\
  --suite <file>     NDJSON suite (required)\n\
  --budget <n>       Token budget per condition (default 4000)\n\
  --margin <f>       Non-inferiority margin for the gate (default 0.0)\n\
  --out <file>       Artifact path (default: data dir)\n\
  --replay <file>    Replay a recording instead of calling a live model (deterministic CI)\n\
  --record <file>    Call the live model and save responses to a recording\n\
  --gate             Exit non-zero if the verdict is a regression\n\n\
footprint OPTIONS (also: `eval --delta`):\n\
  --suite <file>     Footprint-sensitive NDJSON suite (required)\n\
  --margin <f>       Non-inferiority margin for the per-element gate (default 0.0)\n\
  --floor <n>        Min marginal tokens before flagging an element to prune (default 50)\n\
  --replay <file>    Replay a recording (deterministic); --record to capture live\n\
  --json             Emit the full JSON report instead of the side-by-side table\n\
  --gate             Exit non-zero if any injected element is actively harmful\n\
  --export <file>    Write this build's footprint (rules, tool schemas, wakeup) as JSON\n\
  --compare <file>   Paired run: exported baseline footprint vs. this build's footprint\n\n\
testbench OPTIONS:\n\
  --lock <file>      Pinned-repo lockfile (default eval/testbench/testbench.lock.json)\n\
  --out <dir>        Output dir for FINDINGS.md + regressions.json (default testbench-out)\n\
  --cache <dir>      Clone cache for remote repos (default <out>/cache)\n\
  --budget <n>       Token budget per condition (default 4000)\n\
  --margin <f>       Non-inferiority margin for the per-repo gate (default 0.0)\n\
  --replay <file>    Replay a recording (deterministic CI); --record to capture live\n\
  --gate             Exit non-zero if any repo regressed\n\n\
GATES: --gate fails on REGRESSED and on UNDERPOWERED (too few pairs to show quality).\n\
  --mechanism        With --gate: wiring check for tiny fixture suites; only REGRESSED fails\n\n\
LIVE MODEL (when not replaying) is read from the environment:\n\
  LEAN_CTX_EVAL_MODEL_URL   OpenAI-compatible base URL (e.g. https://api.openai.com/v1)\n\
  LEAN_CTX_EVAL_MODEL       Model id (e.g. gpt-4o-mini)\n\
  LEAN_CTX_EVAL_MODEL_KEY   API key (optional for local servers)\n\
  LEAN_CTX_EVAL_SEED        Decoding seed (default 7)"
    );
    println!(
        "  lean-ctx eval frontier --suite <file> --strategies <csv> [opts]\n\
frontier strategies: lean_ctx, json_crush, tabular_crush, yaml_crush, read_full, read_map, read_signatures\n\
frontier options: --limit <n>, --budget <n>, --replay <file>, --record <file>, --json, --gate, --save-evidence"
    );
    println!(
        "Model revision provenance: LEAN_CTX_EVAL_MODEL_VERSION (defaults to a colon-tagged model revision when available)."
    );
    println!(
        "  lean-ctx eval arm-study --protocol <file> --observations <file> [--json]\n\
arm-study: medians, 95% intervals and p95 latency over SUPPLIED four-arm observations of a\n\
registered task list. It runs nothing and measures nothing, and what it prints is no proof\n\
that the observations are authentic, blind or preregistered in time."
    );
    println!(
        "Optional provider metadata: --provider-report <file> --provider-arm <unchanged_agent|community|team|company> --provider-condition <baseline|lean_ctx>"
    );
}

/// A regular file that is not a link, read up to `limit` bytes; one byte more
/// is a refusal, never a truncation.
///
/// The open itself decides: it does not follow a link in the last component
/// and does not wait on a FIFO, so nothing swapped in after an earlier look can
/// be read or block. What was opened must be a regular file, and the path must
/// still name that very file. Where the platform has no such open here, the
/// read is refused instead of done the unsafe way.
#[cfg(unix)]
fn bounded_regular_file(path: &Path, limit: usize) -> Result<Vec<u8>, &'static str> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| "cannot be opened as a file that is not a link")?;
    let opened = file.metadata().map_err(|_| "cannot be read")?;
    if !opened.is_file() {
        return Err("is not a regular file");
    }
    let named = std::fs::symlink_metadata(path).map_err(|_| "cannot be read")?;
    if !named.file_type().is_file() || (named.dev(), named.ino()) != (opened.dev(), opened.ino()) {
        return Err("was replaced while it was opened");
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "cannot be read")?;
    if bytes.len() > limit {
        return Err("exceeds its size bound");
    }
    Ok(bytes)
}

/// Windows: the parent directory is opened and held as the object it is (a
/// reparse point is refused, not followed), and the file is opened by its one
/// name RELATIVE to that held handle with the shared opener
/// (`leanctx_native_storage::windows_file::open_relative`: `RootDirectory` +
/// `OBJ_DONT_REPARSE`), read-only, as a non-directory, without following a final
/// reparse point — the flags `agent_identity::read_key_file` opens a key with.
/// The admitted length is checked before a byte is read; after the read the
/// same name is opened once more against the same held directory and must be
/// the same file (volume serial and file index) of the same length.
///
/// The protection is of the parent and the final component. A reparse point in
/// an EARLIER component of the path is not looked for, and the handles share
/// write and delete like the opener's other callers: a change is detected by
/// the checks above, it is not prevented.
#[cfg(windows)]
fn bounded_regular_file(path: &Path, limit: usize) -> Result<Vec<u8>, &'static str> {
    use leanctx_native_storage::windows_file::open_relative;
    use std::fs::File;
    use std::io::Read;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::MetadataExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use windows_sys::Wdk::Storage::FileSystem::{
        FILE_NON_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT,
    };
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_LIST_DIRECTORY,
        FILE_READ_ATTRIBUTES, FILE_READ_DATA, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        GetFileInformationByHandle, OPEN_EXISTING, SYNCHRONIZE,
    };

    const NOT_OPENED: &str = "cannot be opened as a file that is not a link";
    const NOT_READ: &str = "cannot be read";
    const NOT_REGULAR: &str = "is not a regular file";
    const TOO_LARGE: &str = "exceeds its size bound";

    // A relative path is taken against the working directory as it is now,
    // lexically: nothing is canonicalized and then opened the ordinary way.
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map_err(|_| NOT_READ)?.join(path)
    };
    let parent = absolute.parent().ok_or(NOT_OPENED)?;
    let name: Vec<u16> = absolute
        .file_name()
        .ok_or(NOT_OPENED)?
        .encode_wide()
        .collect();
    // The parent as it is named, first WITHOUT a terminator: a NUL inside it would
    // make the call open a shorter path than the one that was checked. Refused;
    // then exactly one terminator is appended.
    let mut wide_parent: Vec<u16> = parent.as_os_str().encode_wide().collect();
    if wide_parent.contains(&0) {
        return Err(NOT_OPENED);
    }
    wide_parent.push(0);
    // SAFETY: `wide_parent` is NUL-terminated and lives through the call;
    // security attributes and template handle are null.
    let handle = unsafe {
        CreateFileW(
            wide_parent.as_ptr(),
            FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        return Err(NOT_OPENED);
    }
    // SAFETY: a successful `CreateFileW` returned this owned handle.
    let directory = unsafe { File::from_raw_handle(handle) };
    let held = directory.metadata().map_err(|_| NOT_READ)?;
    if held.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 || !held.is_dir() {
        return Err(NOT_OPENED);
    }

    let open = || {
        open_relative(
            &directory,
            &name,
            FILE_READ_DATA | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            FILE_OPEN,
            FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
        )
        .map_err(|_| NOT_OPENED)
    };
    // The length of what was opened, if it is a regular file and no reparse point.
    let regular_length = |file: &File| -> Result<u64, &'static str> {
        let about = file.metadata().map_err(|_| NOT_READ)?;
        if about.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 || !about.is_file() {
            return Err(NOT_REGULAR);
        }
        Ok(about.len())
    };
    let identity = |file: &File| -> Result<(u32, u32, u32), &'static str> {
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        // SAFETY: a live handle and exactly the writable storage the call fills.
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &raw mut info) } == 0 {
            return Err(NOT_READ);
        }
        Ok((
            info.dwVolumeSerialNumber,
            info.nFileIndexHigh,
            info.nFileIndexLow,
        ))
    };

    let file = open()?;
    if regular_length(&file)? > limit as u64 {
        return Err(TOO_LARGE);
    }
    let opened = identity(&file)?;
    let mut bytes = Vec::new();
    (&file)
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| NOT_READ)?;
    if bytes.len() > limit {
        return Err(TOO_LARGE);
    }
    let again = open()?;
    if regular_length(&again)? != bytes.len() as u64
        || regular_length(&file)? != bytes.len() as u64
        || identity(&again)? != opened
    {
        return Err("was replaced while it was opened");
    }
    Ok(bytes)
}

#[cfg(not(any(unix, windows)))]
fn bounded_regular_file(path: &Path, limit: usize) -> Result<Vec<u8>, &'static str> {
    let _ = (path, limit);
    Err("cannot be read: this command has no link-safe file open on this platform")
}

/// The optional provider source is one explicit all-or-none tuple. Admission of
/// these arguments precedes every file read, including the unchanged old inputs.
fn arm_study_provider_args(
    args: &[String],
) -> Result<Option<(&str, Arm, ProviderCondition)>, &'static str> {
    let mut values = [None; 3];
    for (index, arg) in args.iter().enumerate() {
        let slot = match arg.as_str() {
            "--provider-report" => 0,
            "--provider-arm" => 1,
            "--provider-condition" => 2,
            _ if arg.split_once('=').is_some_and(|(flag, _)| {
                matches!(
                    flag,
                    "--provider-report" | "--provider-arm" | "--provider-condition"
                )
            }) =>
            {
                return Err("provider flags require separate values");
            }
            _ => continue,
        };
        if values[slot].is_some() {
            return Err("provider flags must occur exactly once");
        }
        values[slot] = Some(
            args.get(index + 1)
                .map(String::as_str)
                .filter(|value| !value.is_empty() && !value.starts_with('-'))
                .ok_or("provider flags require a value")?,
        );
    }
    match values {
        [None, None, None] => Ok(None),
        [Some(path), Some(arm), Some(condition)] => {
            let arm = match arm {
                "unchanged_agent" => Arm::UnchangedAgent,
                "community" => Arm::Community,
                "team" => Arm::Team,
                "company" => Arm::Company,
                _ => {
                    return Err("provider arm must be unchanged_agent, community, team or company");
                }
            };
            let condition = match condition {
                "baseline" => ProviderCondition::Baseline,
                "lean_ctx" => ProviderCondition::LeanCtx,
                _ => return Err("provider condition must be baseline or lean_ctx"),
            };
            Ok(Some((path, arm, condition)))
        }
        _ => Err("provider-report, provider-arm and provider-condition are required together"),
    }
}

/// `eval arm-study`: inputs are admitted by size before they are
/// parsed, the protocol's own work bounds before anything is analysed
/// (`arm_study::parse_protocol`), and a refusal exits 1 with the rule it broke.
fn cmd_arm_study(args: &[String]) {
    let provider = arm_study_provider_args(args).unwrap_or_else(|error| {
        eprintln!("eval arm-study: {error}");
        std::process::exit(2);
    });
    let (Some(protocol_path), Some(observations_path)) = (
        flag_value(args, "--protocol"),
        flag_value(args, "--observations"),
    ) else {
        eprintln!("eval arm-study: --protocol <file> and --observations <file> are required");
        std::process::exit(2);
    };
    let read = |path: &str, limit: usize| {
        bounded_regular_file(Path::new(path), limit).unwrap_or_else(|error| {
            eprintln!("eval arm-study: {path} {error}");
            std::process::exit(1);
        })
    };
    let protocol_bytes = read(protocol_path, arm_study::MAX_PROTOCOL_BYTES);
    let observation_bytes = read(observations_path, arm_study::MAX_OBSERVATIONS_BYTES);
    let provider_bytes = provider.map(|(path, arm, condition)| {
        (
            read(path, arm_study::MAX_PROVIDER_REPORT_BYTES),
            arm,
            condition,
        )
    });
    let report = arm_study::parse_protocol(&protocol_bytes).and_then(|(protocol, digest)| {
        let mut observations = arm_study::parse_observations(&observation_bytes)?;
        if let Some((bytes, arm, condition)) = &provider_bytes {
            let provider_report = arm_study::parse_provider_report(bytes)?;
            arm_study::attach_provider_observations(
                &mut observations,
                &provider_report,
                *arm,
                *condition,
            )?;
        }
        arm_study::analyze(&protocol, &digest, &observations)
    });
    match report {
        Ok(report) if has_flag(args, "--json") => println!("{}", report.to_json()),
        Ok(report) => print!("{}", report.render()),
        Err(refusal) => {
            eprintln!("eval arm-study: refused: {refusal}");
            std::process::exit(1);
        }
    }
}

/// Returns the value following `flag` in `args`, if present.
fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

fn cmd_ab(args: &[String]) {
    let Some(suite_path) = flag_value(args, "--suite") else {
        eprintln!("eval ab: --suite <file> is required");
        std::process::exit(2);
    };
    let suite_path = PathBuf::from(suite_path);
    let suite = match EvalSuite::load(&suite_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("eval ab: {e:#}");
            std::process::exit(1);
        }
    };
    let suite_name = suite_path
        .file_name()
        .map_or_else(|| "suite".to_string(), |s| s.to_string_lossy().into_owned());

    let mut cfg = AbRunConfig::default();
    if let Some(b) = flag_value(args, "--budget").and_then(|v| v.parse().ok()) {
        cfg.budget_tokens = b;
    }
    cfg.report = ReportConfig {
        noninferiority_margin: flag_value(args, "--margin")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.0),
        ..ReportConfig::default()
    };

    // Runner selection: replay (deterministic) > live + record > live.
    let report = if let Some(replay) = flag_value(args, "--replay") {
        let runner = match RecordedRunner::from_file(Path::new(replay)) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("eval ab: {e:#}");
                std::process::exit(1);
            }
        };
        run_or_exit(&suite, &suite_name, &runner, &cfg)
    } else {
        let live = match OpenAiRunner::from_env() {
            Ok(r) => r,
            Err(e) => {
                eprintln!(
                    "eval ab: no live model configured: {e:#}\n(use --replay <file> for an offline run)"
                );
                std::process::exit(1);
            }
        };
        cfg.report.live_model = true;
        if let Some(record_path) = flag_value(args, "--record") {
            let recorder = RecordingRunner::new(live);
            let report = run_or_exit(&suite, &suite_name, &recorder, &cfg);
            if let Err(e) = recorder.into_recording().save(Path::new(record_path)) {
                eprintln!("eval ab: failed to save recording: {e:#}");
                std::process::exit(1);
            }
            println!("Recording saved → {record_path}");
            report
        } else {
            run_or_exit(&suite, &suite_name, &live, &cfg)
        }
    };

    // Sign + persist the artifact.
    let agent_id = crate::core::agent_identity::current_agent_id().to_string();
    let mut signed = SignedAbReportV1::from_report(report, &agent_id);
    if let Err(e) = signed.sign(&agent_id) {
        eprintln!("eval ab: signing failed: {e}");
        std::process::exit(1);
    }
    let out = match flag_value(args, "--out") {
        Some(p) => PathBuf::from(p),
        None => match artifact::default_artifact_path() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("eval ab: {e}");
                std::process::exit(1);
            }
        },
    };
    if let Err(e) = artifact::write_artifact(&signed, &out) {
        eprintln!("eval ab: {e}");
        std::process::exit(1);
    }

    println!("{}", signed.report.render());
    println!("determinism digest: {}", signed.determinism_digest);
    println!("artifact:           {}", out.display());

    if has_flag(args, "--gate") && !signed.verdict.passes_gate(has_flag(args, "--mechanism")) {
        eprintln!("\nquality gate FAILED: {}", signed.verdict.label());
        std::process::exit(1);
    }
}

fn cmd_frontier(args: &[String]) {
    let Some(suite_path) = flag_value(args, "--suite") else {
        eprintln!("eval frontier: --suite <file> is required");
        std::process::exit(2);
    };
    let Some(strategy_names) = flag_value(args, "--strategies") else {
        eprintln!("eval frontier: --strategies <csv> is required");
        std::process::exit(2);
    };
    let strategies = match frontier::parse_strategies(strategy_names) {
        Ok(strategies) => strategies,
        Err(error) => {
            eprintln!("eval frontier: {error:#}");
            std::process::exit(2);
        }
    };

    let suite_path = PathBuf::from(suite_path);
    let mut suite = match EvalSuite::load(&suite_path) {
        Ok(suite) => suite,
        Err(error) => {
            eprintln!("eval frontier: {error:#}");
            std::process::exit(1);
        }
    };
    if let Some(limit) = flag_value(args, "--limit") {
        let Ok(limit) = limit.parse::<usize>() else {
            eprintln!("eval frontier: --limit must be a positive integer");
            std::process::exit(2);
        };
        if limit == 0 {
            eprintln!("eval frontier: --limit must be a positive integer");
            std::process::exit(2);
        }
        suite.tasks.truncate(limit);
    }
    let suite_name = suite_path.file_name().map_or_else(
        || "suite".to_string(),
        |name| name.to_string_lossy().into_owned(),
    );

    let mut cfg = AbRunConfig::default();
    if let Some(budget) = flag_value(args, "--budget").and_then(|value| value.parse().ok()) {
        cfg.budget_tokens = budget;
    }
    cfg.report = ReportConfig {
        noninferiority_margin: flag_value(args, "--margin")
            .and_then(|value| value.parse().ok())
            .unwrap_or(0.0),
        ..ReportConfig::default()
    };

    let report = if let Some(replay) = flag_value(args, "--replay") {
        let runner = match RecordedRunner::from_file(Path::new(replay)) {
            Ok(runner) => runner,
            Err(error) => {
                eprintln!("eval frontier: {error:#}");
                std::process::exit(1);
            }
        };
        run_frontier_or_exit(&suite, &suite_name, &runner, &cfg, &strategies)
    } else {
        let live = match OpenAiRunner::from_env() {
            Ok(runner) => runner,
            Err(error) => {
                eprintln!(
                    "eval frontier: no live model configured: {error:#}\n(use --replay <file> for an offline run)"
                );
                std::process::exit(1);
            }
        };
        cfg.report.live_model = true;
        if let Some(record_path) = flag_value(args, "--record") {
            let recorder = RecordingRunner::new(live);
            let report = run_frontier_or_exit(&suite, &suite_name, &recorder, &cfg, &strategies);
            if let Err(error) = recorder.into_recording().save(Path::new(record_path)) {
                eprintln!("eval frontier: failed to save recording: {error:#}");
                std::process::exit(1);
            }
            eprintln!("Recording saved → {record_path}");
            report
        } else {
            run_frontier_or_exit(&suite, &suite_name, &live, &cfg, &strategies)
        }
    };

    if has_flag(args, "--json") {
        println!("{}", report.to_json());
    } else {
        print!("{}", report.render_table());
    }
    if has_flag(args, "--save-evidence") {
        match crate::core::eval_ab::strategy_evaluations::save(&report) {
            Ok(0) => eprintln!(
                "No read-strategy evaluation to save (use read_full, read_map, read_signatures)."
            ),
            Ok(saved) => {
                eprintln!("Saved {saved} read-strategy evaluation(s) as context-policy evidence.");
            }
            Err(error) => {
                eprintln!("eval frontier: failed to save evidence: {error}");
                std::process::exit(1);
            }
        }
    }
    if has_flag(args, "--gate") && !report.gate_passes(has_flag(args, "--mechanism")) {
        eprintln!("\nquality gate FAILED: one or more frontier strategies regressed");
        std::process::exit(1);
    }
}

fn run_frontier_or_exit(
    suite: &EvalSuite,
    suite_name: &str,
    runner: &dyn ModelRunner,
    cfg: &AbRunConfig,
    strategies: &[Condition],
) -> FrontierReport {
    match frontier::run_frontier(suite, suite_name, runner, cfg, strategies) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("eval frontier: run failed: {error:#}");
            std::process::exit(1);
        }
    }
}

fn run_or_exit(
    suite: &EvalSuite,
    suite_name: &str,
    runner: &dyn crate::core::eval_ab::model::ModelRunner,
    cfg: &AbRunConfig,
) -> crate::core::eval_ab::report::AbReport {
    match run_ab(suite, suite_name, runner, cfg) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("eval ab: run failed: {e:#}");
            std::process::exit(1);
        }
    }
}

/// `eval footprint` (alias `eval --delta`): ablate each element of lean-ctx's own
/// injected context (rules / tool schemas / wakeup) and report per-element
/// pass-rate Δ + token Δ with a prune recommendation (#959).
/// `eval footprint --compare <baseline.json>`: the exported footprint of another
/// build against this build's live footprint, paired on the same tasks.
fn cmd_footprint_compare(
    args: &[String],
    suite: &EvalSuite,
    suite_name: &str,
    baseline_path: &str,
    candidate: &Footprint,
    mut report_cfg: ReportConfig,
) {
    let baseline: Footprint = match std::fs::read_to_string(baseline_path)
        .map_err(|e| e.to_string())
        .and_then(|raw| serde_json::from_str(&raw).map_err(|e| e.to_string()))
    {
        Ok(fp) => fp,
        Err(e) => {
            eprintln!("eval footprint: cannot load baseline footprint {baseline_path}: {e}");
            std::process::exit(1);
        }
    };
    let run = |runner: &dyn crate::core::eval_ab::model::ModelRunner, cfg: ReportConfig| {
        match crate::core::eval_ab::footprint::run_footprint_compare(
            suite, suite_name, &baseline, candidate, runner, cfg,
        ) {
            Ok(report) => report,
            Err(e) => {
                eprintln!("eval footprint: {e:#}");
                std::process::exit(1);
            }
        }
    };
    let report = if let Some(replay) = flag_value(args, "--replay") {
        match RecordedRunner::from_file(Path::new(replay)) {
            Ok(runner) => run(&runner, report_cfg),
            Err(e) => {
                eprintln!("eval footprint: {e:#}");
                std::process::exit(1);
            }
        }
    } else {
        let live = match OpenAiRunner::from_env() {
            Ok(r) => r,
            Err(e) => {
                eprintln!(
                    "eval footprint: no live model configured: {e:#}\n(use --replay <file> for an offline run)"
                );
                std::process::exit(1);
            }
        };
        report_cfg.live_model = true;
        if let Some(record_path) = flag_value(args, "--record") {
            let recorder = RecordingRunner::new(live);
            let report = run(&recorder, report_cfg);
            if let Err(e) = recorder.into_recording().save(Path::new(record_path)) {
                eprintln!("eval footprint: failed to save recording: {e:#}");
                std::process::exit(1);
            }
            println!("Recording saved → {record_path}");
            report
        } else {
            run(&live, report_cfg)
        }
    };
    let saved = report
        .records
        .first()
        .map_or(0, |r| r.baseline_tokens as i64 - r.lean_ctx_tokens as i64);
    println!("Footprint compare: baseline {baseline_path} vs. this build");
    println!(
        "Fixed footprint delta: {saved:+} tokens per request (positive = candidate smaller)\n"
    );
    println!("{}", report.render());
    if has_flag(args, "--gate") && !report.verdict.passes_gate(has_flag(args, "--mechanism")) {
        eprintln!(
            "\nfootprint compare gate FAILED: {}",
            report.verdict.label()
        );
        std::process::exit(1);
    }
}

fn cmd_footprint(args: &[String]) {
    let Some(suite_path) = flag_value(args, "--suite") else {
        eprintln!("eval footprint: --suite <file> is required");
        std::process::exit(2);
    };
    let suite_path = PathBuf::from(suite_path);
    let suite = match EvalSuite::load(&suite_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("eval footprint: {e:#}");
            std::process::exit(1);
        }
    };
    let suite_name = suite_path.file_name().map_or_else(
        || "footprint".to_string(),
        |s| s.to_string_lossy().into_owned(),
    );

    let margin = flag_value(args, "--margin")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.0);
    let token_floor = flag_value(args, "--floor")
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| FootprintConfig::default().token_floor);
    let mut cfg = FootprintConfig {
        report: ReportConfig {
            noninferiority_margin: margin,
            ..ReportConfig::default()
        },
        token_floor,
    };

    // The footprint under test is what this install actually injects.
    let project_root = std::env::current_dir()
        .map_or_else(|_| ".".to_string(), |p| p.to_string_lossy().into_owned());
    let footprint = Footprint::live(&project_root);

    if let Some(path) = flag_value(args, "--export") {
        let json = serde_json::to_string_pretty(&footprint).unwrap_or_default();
        if let Err(e) = std::fs::write(path, json) {
            eprintln!("eval footprint: cannot write {path}: {e}");
            std::process::exit(1);
        }
        println!("Footprint exported → {path}");
        return;
    }
    if let Some(path) = flag_value(args, "--compare") {
        cmd_footprint_compare(args, &suite, &suite_name, path, &footprint, cfg.report);
        return;
    }

    let mut report = if let Some(replay) = flag_value(args, "--replay") {
        let runner = match RecordedRunner::from_file(Path::new(replay)) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("eval footprint: {e:#}");
                std::process::exit(1);
            }
        };
        run_footprint_or_exit(&suite, &suite_name, &footprint, &runner, &cfg)
    } else {
        let live = match OpenAiRunner::from_env() {
            Ok(r) => r,
            Err(e) => {
                eprintln!(
                    "eval footprint: no live model configured: {e:#}\n(use --replay <file> for an offline run)"
                );
                std::process::exit(1);
            }
        };
        cfg.report.live_model = true;
        if let Some(record_path) = flag_value(args, "--record") {
            let recorder = RecordingRunner::new(live);
            let report = run_footprint_or_exit(&suite, &suite_name, &footprint, &recorder, &cfg);
            if let Err(e) = recorder.into_recording().save(Path::new(record_path)) {
                eprintln!("eval footprint: failed to save recording: {e:#}");
                std::process::exit(1);
            }
            println!("Recording saved → {record_path}");
            report
        } else {
            run_footprint_or_exit(&suite, &suite_name, &footprint, &live, &cfg)
        }
    };

    let agent_id = crate::core::agent_identity::current_agent_id().to_string();
    if let Err(e) = report.sign(&agent_id) {
        eprintln!("eval footprint: signing failed: {e}");
        std::process::exit(1);
    }

    let out = match flag_value(args, "--out") {
        Some(p) => PathBuf::from(p),
        None => match default_footprint_path() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("eval footprint: {e}");
                std::process::exit(1);
            }
        },
    };
    if let Some(parent) = out.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::write(&out, report.to_json()) {
        eprintln!("eval footprint: write {}: {e}", out.display());
        std::process::exit(1);
    }

    if has_flag(args, "--json") {
        println!("{}", report.to_json());
    } else {
        println!("{}", report.render());
        println!("artifact:           {}", out.display());
    }

    if has_flag(args, "--gate") && !report.gate_passes(has_flag(args, "--mechanism")) {
        eprintln!("\nfootprint gate FAILED: a harmful injected element is present");
        std::process::exit(1);
    }
}

fn run_footprint_or_exit(
    suite: &EvalSuite,
    suite_name: &str,
    footprint: &Footprint,
    runner: &dyn ModelRunner,
    cfg: &FootprintConfig,
) -> FootprintReport {
    match run_footprint_ab(suite, suite_name, footprint, runner, cfg) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("eval footprint: run failed: {e:#}");
            std::process::exit(1);
        }
    }
}

/// Default footprint artifact location: `<data_dir>/eval/footprint-report-v1_<utc>.json`.
fn default_footprint_path() -> Result<PathBuf, String> {
    let dir = crate::core::data_dir::lean_ctx_data_dir()?.join("eval");
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir eval: {e}"))?;
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    Ok(dir.join(format!("footprint-report-v1_{stamp}.json")))
}

/// `eval testbench`: run the off-vs-on answer-quality benchmark across every pinned
/// repo in a lockfile, write `FINDINGS.md` + `regressions.json`, and (with `--gate`)
/// fail the build if any repo regressed (#611).
fn cmd_testbench(args: &[String]) {
    let lock_path = flag_value(args, "--lock").map_or_else(
        || PathBuf::from("eval/testbench/testbench.lock.json"),
        PathBuf::from,
    );
    let lock = match TestbenchLock::load(&lock_path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("eval testbench: {e:#}\n(use --lock <file> to point at a lockfile)");
            std::process::exit(1);
        }
    };

    let out_dir =
        flag_value(args, "--out").map_or_else(|| PathBuf::from("testbench-out"), PathBuf::from);
    let cache_dir =
        flag_value(args, "--cache").map_or_else(|| out_dir.join("cache"), PathBuf::from);

    let mut cfg = TestbenchConfig::default();
    if let Some(b) = flag_value(args, "--budget").and_then(|v| v.parse().ok()) {
        cfg.run.budget_tokens = b;
    }
    cfg.run.report = ReportConfig {
        noninferiority_margin: flag_value(args, "--margin")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.0),
        ..ReportConfig::default()
    };

    let report = if let Some(replay) = flag_value(args, "--replay") {
        let runner = match RecordedRunner::from_file(Path::new(replay)) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("eval testbench: {e:#}");
                std::process::exit(1);
            }
        };
        run_testbench_or_exit(&lock, &cache_dir, &runner, &cfg)
    } else {
        let live = match OpenAiRunner::from_env() {
            Ok(r) => r,
            Err(e) => {
                eprintln!(
                    "eval testbench: no live model configured: {e:#}\n(use --replay <file> for an offline run)"
                );
                std::process::exit(1);
            }
        };
        cfg.run.report.live_model = true;
        if let Some(record_path) = flag_value(args, "--record") {
            let recorder = RecordingRunner::new(live);
            let report = run_testbench_or_exit(&lock, &cache_dir, &recorder, &cfg);
            if let Err(e) = recorder.into_recording().save(Path::new(record_path)) {
                eprintln!("eval testbench: failed to save recording: {e:#}");
                std::process::exit(1);
            }
            println!("Recording saved → {record_path}");
            report
        } else {
            run_testbench_or_exit(&lock, &cache_dir, &live, &cfg)
        }
    };

    let (findings_path, regressions_path) = match findings::write(&report, &out_dir) {
        Ok(paths) => paths,
        Err(e) => {
            eprintln!("eval testbench: {e:#}");
            std::process::exit(1);
        }
    };

    print!("{}", findings::render_findings(&report));
    println!("\nFINDINGS:     {}", findings_path.display());
    println!("regressions:  {}", regressions_path.display());

    if has_flag(args, "--gate") && !report.gate_passes(has_flag(args, "--mechanism")) {
        eprintln!("\ntestbench gate FAILED: {}", report.verdict.label());
        std::process::exit(1);
    }
}

fn run_testbench_or_exit(
    lock: &TestbenchLock,
    cache_dir: &Path,
    runner: &dyn ModelRunner,
    cfg: &TestbenchConfig,
) -> TestbenchReport {
    match testbench::run_testbench(lock, cache_dir, runner, cfg) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("eval testbench: run failed: {e:#}");
            std::process::exit(1);
        }
    }
}

fn cmd_verify(args: &[String]) {
    let Some(path) = args.first() else {
        eprintln!("eval verify: <artifact.json> is required");
        std::process::exit(2);
    };
    let artifact = match artifact::load_artifact(Path::new(path)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("eval verify: {e}");
            std::process::exit(1);
        }
    };
    let result = artifact.verify();
    println!("Artifact:           {path}");
    println!("Verdict:            {}", artifact.verdict.label());
    println!(
        "Evidence tier:      {}",
        artifact.report.evidence_tier.map_or_else(
            || "UNSPECIFIED (v1 report)".to_string(),
            |t| format!("{} ({})", t.code(), t.label())
        )
    );
    println!(
        "Quality claim:      {}",
        if artifact.report.supports_quality_claim() {
            "supported (powered, model-backed)"
        } else {
            "none"
        }
    );
    println!("Determinism digest: {}", artifact.determinism_digest);
    println!(
        "Digest matches:     {}",
        if result.digest_matches { "yes" } else { "NO" }
    );
    println!(
        "Signature valid:    {}",
        if result.signature_valid { "yes" } else { "NO" }
    );
    if let Some(pk) = &result.signer_public_key {
        println!("Signer public key:  {pk}");
    }
    if let Some(err) = &result.error {
        println!("Error:              {err}");
    }
    if result.ok() {
        println!("\nOK — artifact is authentic and internally consistent.");
    } else {
        eprintln!("\nFAILED — artifact could not be verified.");
        std::process::exit(1);
    }
}

fn cmd_init(args: &[String]) {
    let dir = PathBuf::from(args.first().map_or("eval-suite", |s| s.as_str()));
    match write_starter_suite(&dir) {
        Ok(suite) => {
            println!("Starter suite written to {}", dir.display());
            println!("Suite file: {}", suite.display());
            println!("\nNext:");
            println!("  # 1) record real model answers once (needs a live model in env)");
            println!(
                "  lean-ctx eval ab --suite {} --record {}/recording.json",
                suite.display(),
                dir.display()
            );
            println!("  # 2) replay deterministically anywhere (CI)");
            println!(
                "  lean-ctx eval ab --suite {} --replay {}/recording.json --gate",
                suite.display(),
                dir.display()
            );
        }
        Err(e) => {
            eprintln!("eval init: {e:#}");
            std::process::exit(1);
        }
    }
}

/// Materializes a small, runnable starter suite: one RAG/QA task whose answer lives in the
/// corpus, and one POSIX-shell code task with a failing stub + unit test.
fn write_starter_suite(dir: &Path) -> anyhow::Result<PathBuf> {
    use anyhow::Context;
    let corpus = dir.join("corpus");
    let code = dir.join("code");
    std::fs::create_dir_all(&corpus).context("creating corpus dir")?;
    std::fs::create_dir_all(&code).context("creating code dir")?;

    std::fs::write(
        corpus.join("architecture.md"),
        "# Consolidation pipeline\n\n\
Provider data flows through one consolidation pipeline. Artifacts are persisted to four \
stores: the BM25 index, the Graph index, ProjectKnowledge, and the Session cache. This is \
what lets semantic search, knowledge recall, and cross-source hints share one source of truth.\n",
    )
    .context("writing corpus/architecture.md")?;
    std::fs::write(
        corpus.join("overview.md"),
        "# Overview\n\nlean-ctx is a context runtime for AI agents. This file is general \
background and intentionally does not list the consolidation stores.\n",
    )
    .context("writing corpus/overview.md")?;

    std::fs::write(
        code.join("test.sh"),
        "#!/bin/sh\n. ./solution.sh\n[ \"$(add 2 3)\" = \"5\" ] || exit 1\n[ \"$(add 10 20)\" = \"30\" ] || exit 1\n",
    )
    .context("writing code/test.sh")?;
    std::fs::write(
        code.join("solution.sh"),
        "# TODO: implement add() so that `add a b` prints a+b\nadd() { echo 0; }\n",
    )
    .context("writing code/solution.sh")?;

    let suite = dir.join("suite.ndjson");
    let lines = [
        r#"{"id":"qa-consolidation-stores","domain":"qa","prompt":"Which four stores does the consolidation pipeline persist artifacts to?","workspace":"corpus","answers":["bm25 index, graph index, projectknowledge, session cache","bm25, graph, knowledge, session"]}"#,
        r#"{"id":"code-add","domain":"code","prompt":"Implement the POSIX shell function add in solution.sh so that `add a b` prints the sum a+b. Output only the file contents.","workspace":"code","target_file":"solution.sh","test_cmd":"sh test.sh"}"#,
    ];
    std::fs::write(
        &suite,
        format!("# lean-ctx eval starter suite\n{}\n", lines.join("\n")),
    )
    .context("writing suite.ndjson")?;
    Ok(suite)
}

#[cfg(test)]
mod recording_guard_tests {
    use super::*;

    #[test]
    fn arm_study_provider_flags_are_explicit_complete_and_unique() {
        let strings = |values: &[&str]| values.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            arm_study_provider_args(&strings(&["--protocol", "p", "--observations", "o"])),
            Ok(None)
        );
        let pairs = [
            ["--provider-report", "report.json"],
            ["--provider-arm", "team"],
            ["--provider-condition", "lean_ctx"],
        ];
        let valid = pairs
            .iter()
            .flatten()
            .map(|s| (*s).to_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            arm_study_provider_args(&valid),
            Ok(Some(("report.json", Arm::Team, ProviderCondition::LeanCtx)))
        );
        for mask in 1..7 {
            let mut args = Vec::new();
            for (index, pair) in pairs.iter().enumerate() {
                if mask & (1 << index) != 0 {
                    args.extend(pair.iter().map(|s| (*s).to_owned()));
                }
            }
            assert!(arm_study_provider_args(&args).is_err());
        }
        for pair in pairs {
            let mut duplicated = valid.clone();
            duplicated.extend(pair.map(str::to_owned));
            assert!(arm_study_provider_args(&duplicated).is_err());
        }
        for bad in [
            vec!["--provider-report"],
            vec!["--provider-report", "--provider-arm", "team"],
            vec!["--provider-report", ""],
            vec!["--provider-arm=team"],
            vec![
                "--provider-report",
                "r",
                "--provider-arm",
                "baseline",
                "--provider-condition",
                "lean_ctx",
            ],
            vec![
                "--provider-report",
                "r",
                "--provider-arm",
                "team",
                "--provider-condition",
                "company",
            ],
        ] {
            assert!(arm_study_provider_args(&strings(&bad)).is_err());
        }
        for (name, arm) in [
            ("unchanged_agent", Arm::UnchangedAgent),
            ("community", Arm::Community),
            ("team", Arm::Team),
            ("company", Arm::Company),
        ] {
            assert_eq!(
                arm_study_provider_args(&strings(&[
                    "--provider-report",
                    "r",
                    "--provider-arm",
                    name,
                    "--provider-condition",
                    "baseline",
                ])),
                Ok(Some(("r", arm, ProviderCondition::Baseline)))
            );
        }
    }

    /// The arm-study inputs: the exact bytes of a regular file at its bound, and
    /// a refusal — never a truncation, a followed link or a wait — for anything else.
    #[cfg(unix)]
    #[test]
    fn arm_study_input_is_a_bounded_regular_file_and_nothing_else() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir().unwrap();
        let regular = dir.path().join("exact");
        std::fs::write(&regular, b"12345678").unwrap();
        assert_eq!(bounded_regular_file(&regular, 8).unwrap(), b"12345678");
        assert_eq!(
            bounded_regular_file(&regular, 7),
            Err("exceeds its size bound")
        );
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&regular, &link).unwrap();
        let fifo = dir.path().join("fifo");
        let name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: a NUL-terminated path inside this test's own directory.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let not_opened = "cannot be opened as a file that is not a link";
        for (path, refusal) in [
            (link, not_opened),
            // Opened without waiting for a writer, then refused for what it is.
            (fifo, "is not a regular file"),
            (dir.path().to_path_buf(), "is not a regular file"),
            (dir.path().join("absent"), not_opened),
        ] {
            assert_eq!(
                bounded_regular_file(&path, 8),
                Err(refusal),
                "{}",
                path.display()
            );
        }
    }

    /// The committed recording (`rust/eval/recording.json`) is what flips the CI
    /// quality-gate from "skipped" to "enforced" (#361 Phase 3). Guard it
    /// **in-process** so a suite/corpus/prompt change that invalidates the
    /// recording (a replay key miss) or a captured regression fails here in
    /// `cargo test` — i.e. during `dev-install` — not only in CI.
    #[test]
    fn committed_recording_replays_and_passes_gate() {
        let dir = tempfile::tempdir().unwrap();
        let suite_path = write_starter_suite(dir.path()).expect("scaffold starter suite");
        let suite = EvalSuite::load(&suite_path).expect("load starter suite");

        let rec_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("eval/recording.json");
        assert!(
            rec_path.exists(),
            "committed recording missing at {} — CI quality-gate would silently skip",
            rec_path.display()
        );
        let runner = RecordedRunner::from_file(&rec_path).expect("load committed recording");

        // Every (task × condition) request must hit a recorded key, else the
        // recording drifted from the suite/corpus/prompt and must be re-captured.
        let report = run_ab(&suite, "suite.ndjson", &runner, &AbRunConfig::default())
            .expect("committed recording must cover every replay key");
        assert!(
            report.verdict.mechanism_gate_passes(),
            "committed recording must not encode a regression, got: {}",
            report.verdict.label()
        );
    }
}
