//! Bounded offline verifier for public OCLA v1 wire documents.

use std::error::Error;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::process::ExitCode;

use lean_ctx_client::{
    decode_agent_envelope, decode_canonical_token_envelope, verify_agent_gateway_admissibility,
    MAX_OCLA_WIRE_BYTES,
};

fn main() -> ExitCode {
    match run(std::env::args().skip(1)) {
        Ok(message) => {
            println!("{message}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("OCLA verification failed: {error}");
            ExitCode::from(2)
        }
    }
}

fn run(mut arguments: impl Iterator<Item = String>) -> Result<String, Box<dyn Error>> {
    let kind = arguments
        .next()
        .ok_or("usage: lean-ctx-ocla-verify <token|agent> <json-path> [--gateway]")?;
    let path = arguments
        .next()
        .ok_or("usage: lean-ctx-ocla-verify <token|agent> <json-path> [--gateway]")?;
    let gateway = match arguments.next().as_deref() {
        None => false,
        Some("--gateway") => true,
        Some(_) => return Err("only --gateway is supported as a third argument".into()),
    };
    if arguments.next().is_some() {
        return Err("too many arguments".into());
    }

    let wire = read_bounded(Path::new(&path))?;
    match kind.as_str() {
        "token" => {
            if gateway {
                return Err("--gateway applies only to agent envelopes".into());
            }
            let envelope = decode_canonical_token_envelope(&wire)?;
            Ok(format!(
                "valid OCLA token envelope v{} {}",
                envelope.schema_version, envelope.idempotency_key
            ))
        }
        "agent" => {
            let envelope = decode_agent_envelope(&wire)?;
            if gateway {
                verify_agent_gateway_admissibility(&envelope)?;
            }
            Ok(format!(
                "valid OCLA agent envelope v{} {}",
                envelope.schema_version, envelope.relay_id
            ))
        }
        _ => Err("wire kind must be token or agent".into()),
    }
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, Box<dyn Error>> {
    // Reject paths that are already visibly unsafe. On Unix this is repeated
    // atomically at open time below to close the check/open race.
    if !std::fs::symlink_metadata(path)?.file_type().is_file() {
        return Err(
            "wire path must be a direct regular file; symlinks and special files are not accepted"
                .into(),
        );
    }
    let file = open_regular_file(path)?;
    let limit = u64::try_from(MAX_OCLA_WIRE_BYTES)? + 1;
    let mut wire = Vec::with_capacity(MAX_OCLA_WIRE_BYTES.min(8 * 1024));
    file.take(limit).read_to_end(&mut wire)?;
    if wire.len() > MAX_OCLA_WIRE_BYTES {
        return Err(format!("wire document exceeds {} bytes", MAX_OCLA_WIRE_BYTES).into());
    }
    Ok(wire)
}

#[cfg(unix)]
fn open_regular_file(path: &Path) -> Result<File, Box<dyn Error>> {
    use std::fs::OpenOptions;
    use std::os::unix::fs::OpenOptionsExt;

    // O_NOFOLLOW prevents a post-check symlink swap; O_NONBLOCK prevents a
    // post-check FIFO swap from blocking before metadata can reject it.
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.file_type().is_file() {
        return Err(
            "wire path must be a direct regular file; symlinks and special files are not accepted"
                .into(),
        );
    }
    Ok(file)
}

#[cfg(not(unix))]
fn open_regular_file(path: &Path) -> Result<File, Box<dyn Error>> {
    // Best-effort fallback: the pre-open symlink_metadata check above and this
    // post-open handle check reject special files, but the standard library has
    // no portable no-follow/non-blocking open primitive.
    let file = File::open(path)?;
    if !file.metadata()?.file_type().is_file() {
        return Err(
            "wire path must be a direct regular file; symlinks and special files are not accepted"
                .into(),
        );
    }
    Ok(file)
}
