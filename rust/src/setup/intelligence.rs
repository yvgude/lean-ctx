// SPDX-License-Identifier: Apache-2.0
//! Setup presentation only; package admission and activation have one core authority.

use std::io::{BufRead, IsTerminal, Read, Write};

use crate::core::{
    intelligence_runtime::{DISCLOSURE, inspect_setup},
    setup_report::{SetupItem, SetupStepReport},
};

const FALLBACK: &str =
    "Continuing with the public reference runtime; optional Intelligence Runtime is unavailable.";

pub(super) fn report() -> SetupStepReport {
    let (status, warnings) = match inspect_setup() {
        Ok(candidate) => (candidate.status(), Vec::new()),
        Err(_) => ("unavailable", vec![FALLBACK.into()]),
    };
    SetupStepReport {
        name: "intelligence_runtime".into(),
        ok: true,
        items: vec![SetupItem {
            name: "Optional Intelligence Runtime".into(),
            status: status.into(),
            path: None,
            note: Some(format!(
                "{DISCLOSURE} Non-interactive setup never grants consent. Use setup runtime activate-configured for a verified installation, or sync-configured for the trusted channel, with --accept-proprietary. Explicit staging configurations additionally require --staging."
            )),
        }],
        warnings,
        errors: Vec::new(),
    }
}

/// Run just the runtime consent step, without changing editor/shell/daemon setup.
pub(crate) fn configure_runtime() -> bool {
    if !std::io::stdin().is_terminal() {
        eprintln!(
            "Runtime configuration requires an interactive terminal; automation must explicitly use activate-configured or sync-configured with --accept-proprietary (also --staging for staging configurations)."
        );
        return false;
    }
    let result = prompt(&mut std::io::stdin().lock(), &mut std::io::stdout().lock());
    if result.is_err() {
        crate::terminal_ui::print_status_warn(FALLBACK);
    }
    result.is_ok()
}

fn prompt(input: &mut impl BufRead, output: &mut impl Write) -> anyhow::Result<()> {
    writeln!(output, "  {DISCLOSURE}")?;
    let candidate = inspect_setup()?;
    if candidate.enabled() {
        writeln!(
            output,
            "  Existing opt-in and installed signature verified. Disable with: lean-ctx setup runtime deactivate"
        )?;
    } else if candidate.available() {
        write!(output, "  Enable the verified installed runtime? [y/N] ")?;
        output.flush()?;
        let mut answer = String::new();
        input.take(64).read_line(&mut answer)?;
        if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            candidate.enable()?;
            writeln!(
                output,
                "  Enabled after signature and compatibility checks."
            )?;
        } else {
            writeln!(output, "  Not enabled; configuration unchanged.")?;
        }
    } else if candidate.channel_available() {
        write!(
            output,
            "  Download and enable the signed runtime from your trusted channel? [y/N] "
        )?;
        output.flush()?;
        let mut answer = String::new();
        input.take(64).read_line(&mut answer)?;
        if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            candidate.install_and_enable()?;
            writeln!(
                output,
                "  Installed and enabled after signature and compatibility checks."
            )?;
        } else {
            writeln!(output, "  Not installed; configuration unchanged.")?;
        }
    } else {
        writeln!(output, "  {FALLBACK}")?;
        writeln!(
            output,
            "  Signed staging package installation: lean-ctx setup runtime install --staging --accept-proprietary (explicit artifact and trust pins required)."
        )?;
    }
    Ok(())
}
