//! Entrypoint smoke gate (#902): every advertised entrypoint must be reachable.
//!
//! Background: the v3.4.7 release shipped tools/commands whose *implementation*
//! existed but whose *entrypoint* was not wired — `lean-ctx pack` / `index` fell
//! through to the global help, and some tools were missing from the manifest.
//! "Implemented" must imply "invokable". Two independent surfaces are guarded:
//!
//! 1. MCP — key entrypoint tools are present in the manifest SSOT.
//! 2. CLI — top-level subcommands route to their handler instead of falling
//!    through to the global help (the exact `pack`/`index` regression).
//!
//! Dispatch on the MCP side is registry-driven (`registry.get_arc(name)`), so the
//! manifest↔registry drift is already covered by `mcp_manifest_up_to_date.rs` and
//! `granular_defs_match_registry`. This gate adds the *advertised entrypoint* and
//! *CLI wiring* dimensions those tests do not cover.

use std::process::Command;

use serde_json::Value;

/// Collect every `"name"` string field anywhere in the manifest JSON. Tool names
/// (`ctx_*`) only ever appear as tool entry names, so membership is unambiguous.
fn collect_names(value: &Value, out: &mut std::collections::BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(n)) = map.get("name") {
                out.insert(n.clone());
            }
            for v in map.values() {
                collect_names(v, out);
            }
        }
        Value::Array(items) => {
            for v in items {
                collect_names(v, out);
            }
        }
        _ => {}
    }
}

#[test]
fn mcp_entrypoint_tools_are_advertised() {
    let manifest = lean_ctx::core::mcp_manifest::manifest_value();
    let mut names = std::collections::BTreeSet::new();
    collect_names(&manifest, &mut names);

    assert!(
        !names.is_empty(),
        "manifest advertises no tools — manifest_value() produced an empty surface"
    );

    // The entrypoint tools that back the regressed CLI commands plus the core
    // read/search surface. If a handler exists but is not advertised here, the
    // tool is unreachable for MCP clients (the v3.4.7 "missing manifest entry").
    let required = [
        "ctx_read",
        "ctx_shell",
        "ctx_search",
        "ctx_pack",
        "ctx_index",
        "ctx_proof",
        "ctx_verify",
        "ctx_explore",
    ];
    for tool in required {
        assert!(
            names.contains(tool),
            "entrypoint tool '{tool}' is not advertised in the manifest.\n\
             Register it in the tool registry and regenerate:\n  \
             cargo run --example gen_mcp_manifest --features dev-tools"
        );
    }
}

fn lean_ctx() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lean-ctx"));
    // Mirror cli_characterization.rs: sandbox the binary so subcommands are
    // side-effect-free (no daemon, no real HOME writes).
    cmd.env("LEAN_CTX_ACTIVE", "1");
    cmd.env("HOME", "/tmp/lean-ctx-entrypoint-test");
    cmd.env("LEAN_CTX_DISABLED", "1");
    cmd
}

/// Run the sandboxed binary; return trimmed stdout, trimmed stderr and the exit
/// code. Stderr matters because the unknown-command handler reports on stderr
/// (#1046 premium UX), so the wiring detector keys off it.
fn run(args: &[&str]) -> (String, String, i32) {
    let out = lean_ctx()
        .args(args)
        .output()
        .expect("failed to spawn lean-ctx binary");
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    (stdout, stderr, out.status.code().unwrap_or(-1))
}

#[test]
fn cli_subcommands_are_wired() {
    // The dispatch fallthrough for an unknown command (#1046 premium UX) reports on
    // stderr — `lean-ctx: unknown command '<cmd>'` plus a "did you mean?" hint —
    // with empty stdout and exit 1. That stderr marker is the *signature* of "not
    // wired" — the exact v3.4.7 `pack`/`index` regression. Derive it from a token
    // that can never be a real command, so the detector validates itself.
    let (stdout, stderr, code) = run(&["__leanctx_not_a_command__"]);
    assert!(
        stdout.is_empty() && stderr.contains("unknown command") && code == 1,
        "unknown-command fallthrough changed (stdout={stdout:?}, stderr={stderr:?}, \
         exit={code}); the wiring detector below keys off the stderr marker"
    );

    // Probe each command with a *garbage subcommand* — never `--help`, since e.g.
    // `pack --help` skips dashed args and runs the default PR packer (a side effect).
    // A wired command routes to its own handler and never emits the top-level
    // `unknown command '<cmd>'` marker; an un-wired one hits the fallthrough and
    // does. Set = the regressed entrypoints (`pack`, `index`) plus representative
    // read / compile / observability commands, each verified side-effect-free on an
    // unknown subcommand. The *full* tool surface is guarded MCP-side by
    // `mcp_entrypoint_tools_are_advertised`.
    let probe = "__leanctx_wiring_probe__";
    for cmd in ["pack", "index", "instructions", "verify"] {
        let (_stdout, stderr, _code) = run(&[cmd, probe]);
        assert!(
            !stderr.contains(&format!("unknown command '{cmd}'")),
            "`lean-ctx {cmd}` falls through to the unknown-command handler — it is not \
             wired in cli/dispatch (the v3.4.7 entrypoint regression)."
        );
    }
}
