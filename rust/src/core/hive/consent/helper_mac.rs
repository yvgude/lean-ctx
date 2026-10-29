// SPDX-License-Identifier: Apache-2.0
//! Client of `LeanCTX Consent.app` (SPEC-ENGINE §3.8.5): the only signer of
//! consent proofs on macOS. The engine checks the helper's code signature
//! (Team ID pin) before every use, starts it with a private socket and a
//! session secret on stdin, and asks it over JSON-RPC lines.
//!
//! The helper renders the dialog from the subject it signs, so this client
//! sends no text: only `consent_id`, `kind` and the stored subject.

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use super::broker::Approval;
use super::{Channel, ConsentError, PresenceSignature, Record};

/// `~/Applications/LeanCTX Consent.app`.
pub const APP_DIR: &str = "Applications/LeanCTX Consent.app";
const EXECUTABLE: &str = "Contents/MacOS/LeanCTX Consent";
/// The release Team ID the helper must be signed with (set by the release
/// build; a build without it never trusts a helper, the host is tty-only).
pub const TEAM_ID: Option<&str> = option_env!("LEANCTX_RELEASE_TEAM_ID");
/// How long the helper may take to open its socket.
const START_WAIT: Duration = Duration::from_secs(5);

pub fn app_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(APP_DIR))
}

/// `TeamIdentifier=` of `codesign -dv` output.
pub fn team_of(codesign_display: &str) -> Option<&str> {
    codesign_display
        .lines()
        .find_map(|l| l.strip_prefix("TeamIdentifier="))
        .map(str::trim)
        .filter(|t| !t.is_empty() && *t != "not set")
}

/// `codesign --verify --strict` and the Team ID pin (`helper_signature`).
pub fn verify_signature(app: &Path) -> Result<(), ConsentError> {
    if !app.join(EXECUTABLE).is_file() {
        return Err(ConsentError::HelperMissing);
    }
    let Some(pinned) = TEAM_ID else {
        return Err(ConsentError::HelperSignature);
    };
    let run = |args: &[&str]| {
        std::process::Command::new("/usr/bin/codesign")
            .args(args)
            .arg(app)
            .stdin(std::process::Stdio::null())
            .output()
    };
    let verified = run(&["--verify", "--strict", "--deep"]).is_ok_and(|o| o.status.success());
    let shown = run(&["-dv", "--verbose=2"]).ok();
    let team = shown
        .as_ref()
        .map(|o| String::from_utf8_lossy(&o.stderr).into_owned())
        .and_then(|text| team_of(&text).map(str::to_string));
    if verified && team.as_deref() == Some(pinned) {
        Ok(())
    } else {
        Err(ConsentError::HelperSignature)
    }
}

/// A running helper with an authenticated connection.
pub struct Helper {
    child: std::process::Child,
    reader: BufReader<std::os::unix::net::UnixStream>,
    writer: std::os::unix::net::UnixStream,
    next_id: u64,
}

impl Helper {
    /// Verify, start and connect; `socket_dir` is `<data>/hive/consent/helper`.
    pub fn start(app: &Path, socket_dir: &Path) -> Result<Helper, ConsentError> {
        verify_signature(app)?;
        let local = |e: std::io::Error| ConsentError::Local(format!("consent helper: {e}"));
        crate::core::hive::state::write_private(socket_dir, ".keep", b"")
            .map_err(|e| ConsentError::Local(e.to_string()))?;
        let socket = socket_dir.join("ctl.sock");
        let _ = std::fs::remove_file(&socket);
        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret).map_err(|e| ConsentError::Local(e.to_string()))?;
        let secret = hex::encode(secret);
        let mut child = std::process::Command::new(app.join(EXECUTABLE))
            .arg("--socket")
            .arg(&socket)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(local)?;
        if let Some(mut stdin) = child.stdin.take() {
            writeln!(stdin, "{secret}").map_err(local)?;
        }
        let started = std::time::Instant::now();
        let stream = loop {
            match std::os::unix::net::UnixStream::connect(&socket) {
                Ok(stream) => break stream,
                Err(_) if started.elapsed() < START_WAIT => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => {
                    let _ = child.kill();
                    return Err(local(e));
                }
            }
        };
        let writer = stream.try_clone().map_err(local)?;
        let mut helper = Helper {
            child,
            reader: BufReader::new(stream),
            writer,
            next_id: 0,
        };
        helper.call("hello", &json!({"secret": secret}))?;
        Ok(helper)
    }

    fn call(&mut self, method: &str, params: &Value) -> Result<Value, ConsentError> {
        self.next_id += 1;
        let request =
            json!({"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params});
        let local = |e: std::io::Error| ConsentError::Local(format!("consent helper: {e}"));
        writeln!(self.writer, "{request}").map_err(local)?;
        let mut line = String::new();
        self.reader.read_line(&mut line).map_err(local)?;
        let answer: Value = serde_json::from_str(&line)
            .map_err(|_| ConsentError::Local("consent helper: malformed answer".into()))?;
        answer_result(&answer)
    }

    /// The OS dialog for `record`; the proof the broker stores.
    pub fn presence(&mut self, record: &Record) -> Result<Approval, ConsentError> {
        let result = self.call(
            "presence",
            &json!({"consent_id": record.consent_id, "kind": record.kind, "subject": record.subject}),
        )?;
        approval_of(&result)
    }

    /// `{alg, public_key, key_id, hardware, card_bind_signature}` for the
    /// device-flow finish (§3.8.4 binding).
    pub fn bind_key(&mut self, instance_id: &str) -> Result<Value, ConsentError> {
        self.call("bind_key", &json!({"instance_id": instance_id}))
    }

    pub fn public_key(&mut self) -> Result<Value, ConsentError> {
        self.call("public_key", &json!({}))
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        let _ = self.writer.shutdown(std::net::Shutdown::Both);
        let _ = self.child.wait();
    }
}

/// A JSON-RPC answer: `result`, or the helper's refusal (code 1 = the
/// person cancelled the dialog).
pub fn answer_result(answer: &Value) -> Result<Value, ConsentError> {
    if let Some(result) = answer.get("result") {
        return Ok(result.clone());
    }
    match answer["error"]["code"].as_i64() {
        Some(1) => Err(ConsentError::PresenceCancelled),
        _ => Err(ConsentError::Local(format!(
            "consent helper: {}",
            answer["error"]["message"].as_str().unwrap_or("refused")
        ))),
    }
}

/// `presence` result → the record's proof.
pub fn approval_of(result: &Value) -> Result<Approval, ConsentError> {
    let (Some(approved_at), Some(key_id), Some(signature)) = (
        result["approved_at"].as_u64(),
        result["key_id"].as_str(),
        result["signature"].as_str(),
    ) else {
        return Err(ConsentError::Local(
            "consent helper: incomplete proof".into(),
        ));
    };
    Ok(Approval::Presence {
        signature: PresenceSignature {
            key_id: key_id.into(),
            alg: "p256_ecdsa_sha256".into(),
            signature: signature.into(),
        },
        approved_at,
        channel: Channel::OsPresence,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_team_id_is_read_from_codesign_output() {
        let shown =
            "Executable=/x\nIdentifier=ch.thinkery.leanctx.consent\nTeamIdentifier=ABCDE12345\n";
        assert_eq!(team_of(shown), Some("ABCDE12345"));
        assert_eq!(team_of("TeamIdentifier=not set\n"), None);
        assert_eq!(team_of("Identifier=x\n"), None);
    }

    #[test]
    fn a_missing_helper_is_named() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            verify_signature(&dir.path().join("LeanCTX Consent.app")),
            Err(ConsentError::HelperMissing)
        );
    }

    #[test]
    fn answers_map_to_proofs_and_refusals() {
        let ok = json!({"jsonrpc": "2.0", "id": 1, "result": {
            "approved_at": 1_792_800_090_000u64, "key_id": "70daf6041ffc7219", "signature": "MEQC"}});
        let Approval::Presence {
            signature,
            approved_at,
            channel,
        } = approval_of(&answer_result(&ok).unwrap()).unwrap()
        else {
            panic!("a presence approval");
        };
        assert_eq!(
            (approved_at, channel),
            (1_792_800_090_000, Channel::OsPresence)
        );
        assert_eq!(signature.alg, "p256_ecdsa_sha256");
        let cancelled =
            json!({"jsonrpc": "2.0", "id": 2, "error": {"code": 1, "message": "cancelled"}});
        assert_eq!(
            answer_result(&cancelled),
            Err(ConsentError::PresenceCancelled)
        );
        let invalid = json!({"jsonrpc": "2.0", "id": 3, "error": {"code": -32602, "message": "invalid_params"}});
        assert!(matches!(
            answer_result(&invalid),
            Err(ConsentError::Local(_))
        ));
    }
}
