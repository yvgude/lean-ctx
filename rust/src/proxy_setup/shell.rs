//! Shell rc/profile proxy env exports.

use std::path::Path;

use crate::marked_block;

use super::claude::anthropic_api_key_available;
use super::codex::codex_uses_chatgpt_login;
use super::commandcode::{commandcode_auth_available, render_commandcode_shell_exports};
use super::grok::{ShellFlavor, effective_grok_auth_mode, render_grok_shell_exports};
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
            marked_block::upsert(
                rc,
                PROXY_ENV_START,
                PROXY_ENV_END,
                &posix_block,
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
