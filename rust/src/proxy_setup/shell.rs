//! Shell rc/profile proxy env exports.

use std::path::Path;

use crate::marked_block;

use super::claude::anthropic_api_key_available;
use super::codex::codex_uses_chatgpt_login;
use super::commandcode::{commandcode_auth_available, render_commandcode_shell_exports};
use super::grok::{ShellFlavor, effective_grok_auth_mode, render_grok_shell_exports};
#[cfg(feature = "tree-sitter")]
use super::util::is_lean_ctx_url_on;
use super::util::{
    ANTHROPIC_OMITTED_NOTE, OPENAI_OMITTED_NOTE, PROXY_ENV_END, PROXY_ENV_START, is_proxy_reachable,
};

pub(crate) fn install_shell_exports(home: &Path, port: u16, quiet: bool, force_endpoint: bool) {
    if !is_proxy_reachable(port) {
        if !quiet {
            println!("  Skipping shell proxy exports (proxy not running on port {port})");
        }
        return;
    }

    let base = format!("http://127.0.0.1:{port}");
    // OpenAI SDK convention: the base URL INCLUDES the `/v1` prefix (default is
    // `https://api.openai.com/v1`); clients append bare endpoints like `/responses`.
    // Without `/v1`, OpenCode's ChatGPT-OAuth plugin fails to recognize Responses-API
    // requests (it matches on `/v1/responses`) and OAuth traffic leaks to the platform
    // API with the wrong credential ("Missing scopes: api.responses.write", #366).
    // Anthropic and Gemini SDKs expect a bare origin instead — they append `/v1/...`
    // / `/v1beta/...` themselves.
    let openai_base = format!("{base}/v1");

    // …but only when the credential in play belongs on that rail. A Codex
    // ChatGPT-subscription login is OAuth against chatgpt.com; sending it to the
    // platform `/v1` rail returns `401 … Missing scopes: api.responses.write`,
    // a message about organization roles that names nothing real (#1685).
    //
    // `install_codex_env` already decides this correctly and writes *nothing*
    // for a ChatGPT login, leaving Codex native. This export used to override
    // that decision from the environment, so the careful config answer never
    // took effect. The rule now lives in one place and both readers agree.
    //
    // Every other provider in this block is already gated the same way —
    // Anthropic on an API key, Grok and Command Code on their auth mode. OpenAI
    // was the only unconditional one.
    let include_openai = !codex_uses_chatgpt_login(home);

    // Only route Claude through the proxy when an API key is available; a Pro/Max
    // subscription must keep talking to api.anthropic.com directly (see
    // `anthropic_api_key_available`).
    let include_anthropic = anthropic_api_key_available(home);
    // Grok (xAI): dual rail — subscription → cli-chat-proxy; API-key → api.x.ai.
    // Match install_grok_env: --force with no auth still exports the subscription rail.
    let grok_mode = effective_grok_auth_mode(home, force_endpoint);
    let posix_grok = render_grok_shell_exports(&base, grok_mode, ShellFlavor::Posix);
    // Command Code: single gateway rail — session/API-key → api.commandcode.ai.
    // Match install_commandcode_env: --force with no auth still exports the rail.
    let commandcode_available = commandcode_auth_available(home) || force_endpoint;
    let posix_commandcode =
        render_commandcode_shell_exports(&base, commandcode_available, ShellFlavor::Posix);

    let posix_anthropic = if include_anthropic {
        format!(r#"export ANTHROPIC_BASE_URL="{base}""#)
    } else {
        format!("# {ANTHROPIC_OMITTED_NOTE}")
    };
    let posix_openai = if include_openai {
        format!(r#"export OPENAI_BASE_URL="{openai_base}""#)
    } else {
        format!("# {OPENAI_OMITTED_NOTE}")
    };
    let posix_block = format!(
        r#"{PROXY_ENV_START}
{posix_anthropic}
{posix_openai}
export GEMINI_API_BASE_URL="{base}"
{posix_grok}
{posix_commandcode}
{PROXY_ENV_END}"#
    );

    for rc in &[home.join(".zshrc"), home.join(".bashrc")] {
        if rc.exists() {
            let label = format!(
                "proxy env in ~/{}",
                rc.file_name().unwrap_or_default().to_string_lossy()
            );
            install_posix_exports(
                rc,
                &posix_block,
                port,
                include_anthropic,
                include_openai,
                quiet,
                &label,
            );
        }
    }

    let fish_config = home.join(".config/fish/config.fish");
    if fish_config.exists() {
        let fish_anthropic = if include_anthropic {
            format!(r#"set -gx ANTHROPIC_BASE_URL "{base}""#)
        } else {
            format!("# {ANTHROPIC_OMITTED_NOTE}")
        };
        let fish_grok = render_grok_shell_exports(&base, grok_mode, ShellFlavor::Fish);
        let fish_commandcode =
            render_commandcode_shell_exports(&base, commandcode_available, ShellFlavor::Fish);
        let fish_openai = if include_openai {
            format!(r#"set -gx OPENAI_BASE_URL "{openai_base}""#)
        } else {
            format!("# {OPENAI_OMITTED_NOTE}")
        };
        let fish_block = format!(
            r#"{PROXY_ENV_START}
{fish_anthropic}
{fish_openai}
set -gx GEMINI_API_BASE_URL "{base}"
{fish_grok}
{fish_commandcode}
{PROXY_ENV_END}"#
        );
        marked_block::upsert(
            &fish_config,
            PROXY_ENV_START,
            PROXY_ENV_END,
            &fish_block,
            quiet,
            "proxy env in ~/.config/fish/config.fish",
        );
    }

    let ps_profile =
        dirs::home_dir().map(|h| crate::shell::platform::resolve_powershell_profile_path(&h));
    if let Some(ref ps) = ps_profile
        && ps.exists()
    {
        let ps_anthropic = if include_anthropic {
            format!(r#"$env:ANTHROPIC_BASE_URL = "{base}""#)
        } else {
            format!("# {ANTHROPIC_OMITTED_NOTE}")
        };
        let ps_grok = render_grok_shell_exports(&base, grok_mode, ShellFlavor::PowerShell);
        let ps_commandcode =
            render_commandcode_shell_exports(&base, commandcode_available, ShellFlavor::PowerShell);
        let ps_openai = if include_openai {
            format!(r#"$env:OPENAI_BASE_URL = "{openai_base}""#)
        } else {
            format!("# {OPENAI_OMITTED_NOTE}")
        };
        let ps_block = format!(
            r#"{PROXY_ENV_START}
{ps_anthropic}
{ps_openai}
$env:GEMINI_API_BASE_URL = "{base}"
{ps_grok}
{ps_commandcode}
{PROXY_ENV_END}"#
        );
        marked_block::upsert(
            ps,
            PROXY_ENV_START,
            PROXY_ENV_END,
            &ps_block,
            quiet,
            "proxy env in PowerShell profile",
        );
    }
}

fn install_posix_exports(
    path: &Path,
    block: &str,
    port: u16,
    include_anthropic: bool,
    include_openai: bool,
    quiet: bool,
    label: &str,
) {
    let mtime = crate::config_io::snapshot_mtime(path);
    let Ok(existing) = std::fs::read_to_string(path) else {
        return;
    };
    #[cfg(not(feature = "tree-sitter"))]
    let filtered = {
        // Minimal builds retain existing managed-block behavior. Unmarked text is
        // never deleted without a parser proving it is a top-level export.
        let _ = (port, include_anthropic, include_openai);
        marked_block::remove_content(&existing, PROXY_ENV_START, PROXY_ENV_END)
    };
    #[cfg(feature = "tree-sitter")]
    let filtered = match reconcile_posix_exports(&existing, port, include_anthropic, include_openai)
    {
        Ok(filtered) => filtered,
        Err(error) => {
            eprintln!("  Cannot safely update {label}: {error}");
            return;
        }
    };
    // Preserve user lines, including their spaces; normalize only the separator
    // before the managed block so a second install does not rewrite the RC.
    let mut output = filtered.trim_end_matches(['\r', '\n']).to_owned();
    if !output.is_empty() {
        output.push_str("\n\n");
    }
    output.push_str(block);
    output.push('\n');
    if output == existing {
        return;
    }
    if let Err(error) = crate::config_io::write_atomic_with_backup_checked(path, &output, mtime) {
        eprintln!("  Cannot update {label}: {error}");
    } else if !quiet {
        println!("  Updated {label}");
    }
}

#[cfg(feature = "tree-sitter")]
fn reconcile_posix_exports(
    existing: &str,
    port: u16,
    include_anthropic: bool,
    include_openai: bool,
) -> Result<String, &'static str> {
    if existing.len() > 1024 * 1024 {
        return Err("shell profile exceeds the automatic migration limit");
    }
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_bash::LANGUAGE.into())
        .map_err(|_| "shell parser unavailable")?;
    let tree = parser.parse(existing, None).ok_or("shell parse failed")?;
    let root = tree.root_node();
    if root.has_error() {
        return Err("shell syntax could not be safely parsed; profile left unchanged");
    }
    let mut cursor = root.walk();
    let children: Vec<_> = root.named_children(&mut cursor).collect();
    let starts = existing
        .lines()
        .filter(|l| l.trim() == PROXY_ENV_START)
        .count();
    let ends = existing
        .lines()
        .filter(|l| l.trim() == PROXY_ENV_END)
        .count();
    let block = match (starts, ends) {
        (0, 0) => None,
        (1, 1) => {
            let start = marked_block::marker_line_span(existing, PROXY_ENV_START)
                .ok_or("proxy start marker could not be located")?;
            let end = marked_block::marker_line_span(existing, PROXY_ENV_END)
                .ok_or("proxy end marker could not be located")?;
            let is_comment = |span: (usize, usize), marker| {
                children.iter().any(|node| {
                    node.kind() == "comment"
                        && node.start_byte() >= span.0
                        && node.end_byte() <= span.1
                        && existing[node.byte_range()].trim() == marker
                })
            };
            if start.0 >= end.0
                || !is_comment(start, PROXY_ENV_START)
                || !is_comment(end, PROXY_ENV_END)
            {
                return Err("proxy markers are not a top-level shell comment block");
            }
            Some((start.0, end.1))
        }
        _ => return Err("duplicate or unpaired proxy markers; profile left unchanged"),
    };
    let mut removed: Vec<_> = block.into_iter().collect();
    for node in children {
        if node.kind() != "declaration_command"
            || node.start_position().row != node.end_position().row
            || block
                .is_some_and(|(start, end)| node.start_byte() >= start && node.end_byte() <= end)
        {
            continue;
        }
        let start = existing[..node.start_byte()]
            .rfind('\n')
            .map_or(0, |p| p + 1);
        let end = existing[node.end_byte()..]
            .find('\n')
            .map_or(existing.len(), |p| node.end_byte() + p + 1);
        if omitted_legacy_export(
            &existing[start..end],
            port,
            include_anthropic,
            include_openai,
        ) {
            removed.push((start, end));
        }
    }
    removed.sort_unstable();
    let mut output = String::with_capacity(existing.len());
    let mut offset = 0;
    for (start, end) in removed {
        output.push_str(&existing[offset..start]);
        offset = end;
    }
    output.push_str(&existing[offset..]);
    Ok(output)
}

#[cfg(feature = "tree-sitter")]
fn omitted_legacy_export(
    line: &str,
    port: u16,
    include_anthropic: bool,
    include_openai: bool,
) -> bool {
    let Some(assignment) = line.trim().strip_prefix("export ") else {
        return false;
    };
    let Some((key, value)) = assignment.split_once('=') else {
        return false;
    };
    let paths: &[&str] = match key {
        "ANTHROPIC_BASE_URL" if !include_anthropic => &["/"],
        "OPENAI_BASE_URL" if !include_openai => &["/v1", "/v1/"],
        _ => return false,
    };
    let value = if value.starts_with('"') {
        value.strip_prefix('"').and_then(|v| v.strip_suffix('"'))
    } else if value.starts_with('\'') {
        value.strip_prefix('\'').and_then(|v| v.strip_suffix('\''))
    } else {
        Some(value)
    };
    let Some(value) = value else {
        return false;
    };
    // Match a single literal assignment, never shell expressions or commands.
    if value.chars().any(char::is_whitespace)
        || value.contains(['$', '`', '\\', '"', '\'', ';', '&', '|'])
        || !is_lean_ctx_url_on(value, Some(port))
    {
        return false;
    }
    // Compare the literal path, before any URL dot-segment normalization.
    let authority = value.strip_prefix("http://").unwrap_or(value);
    let path = authority.find('/').map_or("/", |p| &authority[p..]);
    paths.contains(&path)
}
