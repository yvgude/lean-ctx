// SPDX-License-Identifier: Apache-2.0
//! `client fs-read` and `client fs-write`: ACP `fs/read_text_file` and
//! `fs/write_text_file` through the boundary of `ctx_read` and `ctx_edit`.
//!
//! A read returns the file's text verbatim — never a compressed view; only
//! the context policy (redaction, gateway) may change what is delivered,
//! exactly as for any other agent read. A write replaces the whole file
//! atomically with the text on stdin.

use std::io::Read;
use std::path::Path;

use serde::Serialize;

use super::{Caller, Code, Failure, Outcome, audit, audit_failure, emit_released};
use crate::cli::local_read::flag;

const READ_TOOL: &str = "client.fs_read";
const WRITE_TOOL: &str = "client.fs_write";

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct FsRead {
    pub path: String,
    pub content: String,
    /// The 1-based first line asked for, or `null`.
    pub line: Option<usize>,
    /// The number of lines asked for, or `null`.
    pub limit: Option<usize>,
    /// Lines of the whole (delivered) file.
    pub total_lines: usize,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct FsWrite {
    pub path: String,
    pub bytes: usize,
    pub created: bool,
}

/// A failure the audit trail records with `outcome`.
fn refused(caller: &Caller, tool: &str, path: &str, outcome: Outcome, failure: Failure) -> Failure {
    audit(tool, caller, path, outcome);
    failure.recorded()
}

/// The `--path` a refusal names in the record, as given (or empty).
fn named_path(args: &[String]) -> String {
    flag(args, "--path")
        .ok()
        .flatten()
        .unwrap_or_default()
        .to_owned()
}

fn wrap(
    value: Result<Option<&str>, crate::cli::local_read::Failure>,
) -> Result<Option<&str>, Failure> {
    value.map_err(|f| Failure::invalid(f.message))
}

/// `--path`: required and absolute (ACP sends absolute paths only).
fn required_path(args: &[String]) -> Result<String, Failure> {
    let raw = wrap(flag(args, "--path"))?.ok_or_else(|| Failure::invalid("--path is required"))?;
    if !Path::new(raw).is_absolute() {
        return Err(Failure::invalid("--path must be absolute"));
    }
    Ok(raw.to_owned())
}

/// A positive integer flag, or `None`.
fn positive(args: &[String], name: &str) -> Result<Option<usize>, Failure> {
    match wrap(flag(args, name))? {
        None => Ok(None),
        Some(raw) => raw
            .parse::<usize>()
            .ok()
            .filter(|n| *n >= 1)
            .map(Some)
            .ok_or_else(|| Failure::invalid(format!("{name} must be a positive integer"))),
    }
}

/// The PathJail (project root plus the configured extra roots — the boundary
/// of `ctx_read`, #1903) and the secret-path policy for `raw`.
fn jailed(raw: &str, root: &str, caller: &Caller, tool: &str) -> Result<String, Failure> {
    let path = lean_ctx::core::command_root::jail_cli_read_path(raw, root).map_err(|message| {
        refused(
            caller,
            tool,
            raw,
            Outcome::Jailed,
            Failure::new(Code::Forbidden, message),
        )
    })?;
    match lean_ctx::core::io_boundary::check_secret_path_for_tool(tool, Path::new(&path)) {
        Ok(Some(warning)) => eprintln!("{warning}"),
        Ok(None) => {}
        Err(message) => {
            return Err(refused(
                caller,
                tool,
                &path,
                Outcome::Denied,
                Failure::new(Code::Forbidden, message),
            ));
        }
    }
    Ok(path)
}

/// The lines `line..line+limit` (1-based) of `text`, each with its own line
/// ending, so the joined slice is byte-identical to that part of the file.
pub(crate) fn slice_lines(text: &str, line: Option<usize>, limit: Option<usize>) -> String {
    if line.is_none() && limit.is_none() {
        return text.to_owned();
    }
    let skip = line.map_or(0, |n| n - 1);
    let take = limit.unwrap_or(usize::MAX);
    text.split_inclusive('\n').skip(skip).take(take).collect()
}

/// `lean-ctx client fs-read --json … --path ABS [--line N] [--limit N]`.
pub(super) fn cmd_fs_read(args: &[String], caller: &Caller) -> ! {
    let root = &caller.project_root;
    emit_released("ctx_read", root, "client.fs_read", || {
        read(args, root, caller)
    })
}

/// [`read_checked`], every refusal recorded.
pub(crate) fn read(args: &[String], root: &str, caller: &Caller) -> Result<FsRead, Failure> {
    read_checked(args, root, caller)
        .map_err(|f| audit_failure(READ_TOOL, caller, &named_path(args), f))
}

fn read_checked(args: &[String], root: &str, caller: &Caller) -> Result<FsRead, Failure> {
    let raw = required_path(args)?;
    let line = positive(args, "--line")?;
    let limit = positive(args, "--limit")?;
    let path = jailed(&raw, root, caller, READ_TOOL)?;
    let failed = |code: Code, message: String| {
        refused(
            caller,
            READ_TOOL,
            &path,
            Outcome::Failed,
            Failure::new(code, message),
        )
    };

    let meta = match std::fs::metadata(&path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(failed(Code::NotFound, format!("no file at {path}")));
        }
        Err(error) => return Err(failed(Code::Internal, format!("{path}: {error}"))),
    };
    if !meta.is_file() {
        return Err(failed(
            Code::InvalidArgument,
            format!("{path} is not a file"),
        ));
    }
    let cap = lean_ctx::core::limits::max_read_bytes();
    if meta.len() > cap as u64 {
        return Err(failed(
            Code::TooLarge,
            format!("{path} is {} bytes; the read limit is {cap}", meta.len()),
        ));
    }
    // A text read only: bytes that are not UTF-8 would come back lossy, and
    // an agent writing that view back would corrupt the file.
    let bytes = std::fs::read(&path).map_err(|e| failed(Code::Internal, format!("{path}: {e}")))?;
    if std::str::from_utf8(&bytes).is_err() {
        return Err(failed(
            Code::InvalidArgument,
            format!("{path} is not UTF-8 text"),
        ));
    }
    drop(bytes);

    // The admitted reader of `lean-ctx read`: binary detection, the size
    // bound, the content-policy audit and the gateway admission.
    let admitted = lean_ctx_server::tools::ctx_read::read_file_lossy_admitted_for_cli(&path)
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::PermissionDenied => refused(
                caller,
                READ_TOOL,
                &path,
                Outcome::Denied,
                Failure::new(
                    Code::Withheld,
                    "content withheld by the active context policy",
                ),
            ),
            std::io::ErrorKind::NotFound => failed(Code::NotFound, format!("no file at {path}")),
            _ => failed(Code::Internal, error.to_string()),
        })?;
    let total_lines = admitted.text.split_inclusive('\n').count();
    let content = slice_lines(&admitted.text, line, limit);
    audit(READ_TOOL, caller, &path, Outcome::Done);
    Ok(FsRead {
        path,
        content,
        line,
        limit,
        total_lines,
    })
}

/// `lean-ctx client fs-write --json … --path ABS` (content on stdin).
pub(super) fn cmd_fs_write(args: &[String], caller: &Caller) -> ! {
    let root = &caller.project_root;
    emit_released("ctx_edit", root, "client.fs_write", || {
        let content = content_from(std::io::stdin().lock())
            .map_err(|f| audit_failure(WRITE_TOOL, caller, &named_path(args), f))?;
        write(args, root, caller, &content)
    })
}

/// The new content: UTF-8, at most the read limit.
pub(crate) fn content_from(source: impl Read) -> Result<String, Failure> {
    let cap = lean_ctx::core::limits::max_read_bytes();
    let mut content = Vec::new();
    source
        .take(cap as u64 + 1)
        .read_to_end(&mut content)
        .map_err(|e| Failure::new(Code::Internal, format!("stdin: {e}")))?;
    if content.len() > cap {
        return Err(Failure::new(
            Code::TooLarge,
            format!("content exceeds the write limit of {cap} bytes"),
        ));
    }
    String::from_utf8(content).map_err(|_| Failure::invalid("content on stdin is not UTF-8 text"))
}

/// [`write_checked`], every refusal recorded.
pub(crate) fn write(
    args: &[String],
    root: &str,
    caller: &Caller,
    content: &str,
) -> Result<FsWrite, Failure> {
    write_checked(args, root, caller, content)
        .map_err(|f| audit_failure(WRITE_TOOL, caller, &named_path(args), f))
}

fn write_checked(
    args: &[String],
    root: &str,
    caller: &Caller,
    content: &str,
) -> Result<FsWrite, Failure> {
    let raw = required_path(args)?;
    let path = jailed(&raw, root, caller, WRITE_TOOL)?;
    if let Err(message) = lean_ctx::core::pathjail::enforce_writable(Path::new(&path)) {
        return Err(refused(
            caller,
            WRITE_TOOL,
            &path,
            Outcome::Jailed,
            Failure::new(Code::Forbidden, message),
        ));
    }
    // A redaction marker is what an agent shown a redacted view would write
    // back over the real value; only source and test code may hold one.
    if let Err(error) = lean_ctx_shell_policy::refuse_redaction_marker_in_file(
        Path::new(root),
        Path::new(&path),
        content,
    ) {
        return Err(refused(
            caller,
            WRITE_TOOL,
            &path,
            Outcome::Denied,
            Failure::new(Code::Forbidden, error.message().to_owned()),
        ));
    }
    if Path::new(&path).is_dir() {
        return Err(refused(
            caller,
            WRITE_TOOL,
            &path,
            Outcome::Failed,
            Failure::invalid(format!("{path} is a directory")),
        ));
    }

    // Same-file writes are serialized like `ctx_edit`'s; the write itself is
    // `ctx_edit create=true` (atomic, permissions kept, parents created).
    let file_lock = lean_ctx::core::path_locks::per_file_lock(&path);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let _guard = loop {
        match file_lock.try_lock() {
            Ok(guard) => break guard,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => break poisoned.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {}
        }
        if std::time::Instant::now() >= deadline {
            return Err(refused(
                caller,
                WRITE_TOOL,
                &path,
                Outcome::Failed,
                Failure::new(
                    Code::Internal,
                    format!("another write to {path} is in progress; retry in a moment"),
                ),
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let created = !Path::new(&path).exists();
    let params = lean_ctx_server::tools::ctx_edit::EditParams {
        path: path.clone(),
        old_string: String::new(),
        new_string: content.to_owned(),
        replace_all: false,
        create: true,
        expected_blake3: None,
        expected_size: None,
        expected_mtime_ms: None,
        backup: false,
        backup_path: None,
        evidence: false,
        diff_artifact: false,
        diff_max_lines: 0,
        allow_lossy_utf8: false,
    };
    let (message, effect) = lean_ctx_server::tools::ctx_edit::run_io(&params, "");
    if !matches!(
        effect,
        lean_ctx_server::tools::ctx_edit::CacheEffect::Invalidate
    ) {
        return Err(refused(
            caller,
            WRITE_TOOL,
            &path,
            Outcome::Failed,
            Failure::new(
                Code::Internal,
                message.trim_start_matches("ERROR: ").to_owned(),
            ),
        ));
    }
    audit(WRITE_TOOL, caller, &path, Outcome::Done);
    Ok(FsWrite {
        path,
        bytes: content.len(),
        created,
    })
}
