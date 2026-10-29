// SPDX-License-Identifier: Apache-2.0
//! "Also in the Cockpit" footer for human report output (Stage 1 of
//! "everything goes through the Cockpit"). One dim stderr line naming the
//! Cockpit route that shows the same report; the report itself is unchanged.
//!
//! Printed only for a human on a terminal: never for machine formats, never
//! when stdout is redirected, never in CI and never with `LEAN_CTX_NO_COCKPIT`
//! set. The line is a constant per route — no URL, port or token.
//!
//! Stage 2 (3.12): dashboard-like commands (`gain`, `watch`, `wrapped`,
//! `cockpit --export`) open the Cockpit instead, under the same conditions
//! — a person on a terminal, no machine format, no `--terminal`, no CI, no
//! `LEAN_CTX_NO_COCKPIT` — and only when the Cockpit can be served;
//! otherwise they print today's text (plus one install line when the
//! Cockpit is missing). [`decide`] is that decision, free of I/O.

use super::network::cockpit_start;
use crate::dashboard::cockpit::CockpitUnavailable;

/// Flags that select a machine format, write a file, or ask for help.
const MACHINE_FLAGS: &[&str] = &[
    "--terminal",
    "--json",
    "--md",
    "--markdown",
    "--raw",
    "--export",
    "--svg",
    "--card",
    "--share",
    "--page",
    "--copy",
    "--reset",
    "--web",
    "--live",
    "--help",
    "-h",
    "help",
];

/// The footer text for `route`, or `None` when `args` select a machine
/// format. Pure, so the decision is testable without a terminal.
pub(super) fn footer_line(route: &str, args: &[String]) -> Option<String> {
    if selects_text(args) {
        return None;
    }
    Some(format!(
        "Also in the Cockpit: lean-ctx cockpit \u{2192} #{route}"
    ))
}

/// `args` select a machine format, a file, help or `--terminal`.
fn selects_text(args: &[String]) -> bool {
    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        let flag = arg.split_once('=').map_or(arg.as_str(), |(flag, _)| flag);
        if MACHINE_FLAGS.contains(&flag) {
            return true;
        }
        let format = match arg.strip_prefix("--format=") {
            Some(value) => Some(value),
            None if arg == "--format" => iter.peek().map(|value| value.as_str()),
            None => None,
        };
        if format.is_some_and(|value| !matches!(value, "table" | "text" | "human")) {
            return true;
        }
    }
    false
}

/// Who is watching: decided once per command from the process context.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Surroundings {
    /// stdout and stderr are terminals.
    pub(crate) terminal: bool,
    pub(crate) ci: bool,
    /// `LEAN_CTX_NO_COCKPIT` is set.
    pub(crate) opted_out: bool,
}

impl Surroundings {
    pub(crate) fn now() -> Self {
        use std::io::IsTerminal as _;
        let var = crate::core::host_env::var;
        Self {
            terminal: std::io::stdout().is_terminal() && std::io::stderr().is_terminal(),
            ci: crate::core::telemetry_consent::ci_detected(var),
            opted_out: var("LEAN_CTX_NO_COCKPIT").is_some(),
        }
    }

    fn person(self) -> bool {
        self.terminal && !self.ci && !self.opted_out
    }
}

/// Print the footer to stderr when a human is watching.
pub(super) fn print_footer(route: &str, args: &[String]) {
    if !Surroundings::now().person() {
        return;
    }
    if let Some(line) = footer_line(route, args) {
        eprintln!("\n\x1b[2m{line}\x1b[0m");
    }
}

/// What a dashboard-like command does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Redirect {
    /// Open the Cockpit at this hash route.
    Open(&'static str),
    /// Print today's text output, nothing else.
    Text,
    /// Print today's text output and one line on how to get the Cockpit.
    TextWithInstallHint(CockpitUnavailable),
}

/// Decide a dashboard-like command. `route` is `None` when its arguments
/// have no Cockpit view; `availability` is asked only when the Cockpit
/// would be opened.
pub(crate) fn decide(
    route: Option<&'static str>,
    args: &[String],
    surroundings: Surroundings,
    availability: impl FnOnce() -> Result<(), CockpitUnavailable>,
) -> Redirect {
    let Some(route) = route else {
        return Redirect::Text;
    };
    if !surroundings.person() || selects_text(args) {
        return Redirect::Text;
    }
    match availability() {
        Ok(()) => Redirect::Open(route),
        Err(reason) => Redirect::TextWithInstallHint(reason),
    }
}

/// A dashboard-like command may open the Cockpit now (person on a
/// terminal, no CI, no opt-out).
pub(crate) fn redirect_allowed() -> bool {
    Surroundings::now().person()
}

/// Gain views the Cockpit shows. Value flags take their value inline or as
/// the next argument.
const GAIN_VIEW_FLAGS: &[&str] = &[
    "--graph",
    "--daily",
    "--by-tool",
    "--cost",
    "--agents",
    "--score",
    "--tasks",
    "--deep",
    "--pipeline",
    "--heatmap",
    "--no-cache-adjust",
];
const GAIN_VALUE_FLAGS: &[&str] = &["--period", "--limit", "--model"];

/// The Cockpit route of `lean-ctx gain <args>`, or `None` when the
/// arguments ask for something only the terminal does (exports, Wrapped,
/// `--opportunity`, unknown flags, …). The first view flag wins, in the
/// order `cmd_gain` checks them.
pub(crate) fn gain_route(args: &[String]) -> Option<&'static str> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let flag = arg.split_once('=').map_or(arg.as_str(), |(flag, _)| flag);
        if GAIN_VALUE_FLAGS.contains(&flag) {
            if !arg.contains('=') {
                iter.next();
            }
        } else if !GAIN_VIEW_FLAGS.contains(&flag) || arg.contains('=') {
            return None;
        }
    }
    let has = |flag: &str| args.iter().any(|a| a == flag);
    let ordered: [(&str, &'static str); 6] = [
        ("--by-tool", "savings"),
        ("--graph", "savings"),
        ("--daily", "savings"),
        ("--score", "savings"),
        ("--cost", "spend"),
        ("--tasks", "savings"),
    ];
    if let Some((_, route)) = ordered.iter().find(|(flag, _)| has(flag)) {
        return Some(route);
    }
    if has("--agents") {
        return Some("spend");
    }
    if has("--heatmap") {
        return Some("context?detail=heatmap");
    }
    Some("savings")
}

/// How a redirected command ended.
#[derive(Debug)]
pub(crate) enum Handled {
    /// The Cockpit was opened; print nothing else.
    Opened,
    /// Print the text output, then this line on stderr when given.
    Text(Option<String>),
}

/// Run the redirect decision for `route` and open the Cockpit when it says
/// so. Never prints the token; bounded by the background start's timeouts.
pub(crate) fn redirect(route: Option<&'static str>, args: &[String]) -> Handled {
    let decision = decide(
        route,
        args,
        Surroundings::now(),
        crate::core::intelligence_runtime::cockpit::availability,
    );
    match decision {
        Redirect::Text => Handled::Text(None),
        Redirect::TextWithInstallHint(reason) => {
            Handled::Text(Some(cockpit_start::unavailable_line(reason).to_string()))
        }
        Redirect::Open(route) => match cockpit_start::open(Some(route), &[]) {
            cockpit_start::Outcome::Opened { port, base_path } => {
                println!(
                    "{}",
                    cockpit_start::opened_line(Some(route), port, &base_path)
                );
                Handled::Opened
            }
            cockpit_start::Outcome::Unavailable(reason) => {
                Handled::Text(Some(cockpit_start::unavailable_line(reason).to_string()))
            }
            cockpit_start::Outcome::Failed(message) => Handled::Text(Some(message)),
        },
    }
}

/// The line after a text fallback, dim on stderr.
pub(crate) fn print_hint(hint: Option<&str>) {
    if let Some(line) = hint {
        eprintln!("\n\x1b[2m{line}\x1b[0m");
    }
}

#[cfg(test)]
mod tests {
    use super::footer_line;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn human_output_names_the_route() {
        assert_eq!(
            footer_line("savings", &args(&["--deep"])).as_deref(),
            Some("Also in the Cockpit: lean-ctx cockpit \u{2192} #savings")
        );
        assert!(footer_line("report", &[]).is_some());
        assert!(footer_line("savings", &args(&["--format", "table"])).is_some());
    }

    #[test]
    fn machine_formats_get_no_footer() {
        for case in [
            &["--json"][..],
            &["--md"],
            &["--raw"],
            &["--export", "out.html"],
            &["--export=out.md"],
            &["--format", "json"],
            &["--format=markdown"],
            &["--svg=card.svg"],
            &["--help"],
        ] {
            assert_eq!(footer_line("savings", &args(case)), None, "{case:?}");
        }
    }

    #[test]
    fn footer_is_deterministic_and_carries_no_token() {
        let a = footer_line("context?detail=heatmap", &[]);
        assert_eq!(a, footer_line("context?detail=heatmap", &[]));
        let line = a.unwrap();
        assert!(!line.contains("token") && !line.contains("http"));
    }

    use super::{CockpitUnavailable, Redirect, Surroundings, decide, gain_route};

    const PERSON: Surroundings = Surroundings {
        terminal: true,
        ci: false,
        opted_out: false,
    };

    fn installed() -> Result<(), CockpitUnavailable> {
        Ok(())
    }

    #[test]
    fn a_person_on_a_terminal_gets_the_cockpit_route() {
        assert_eq!(
            decide(gain_route(&[]), &[], PERSON, installed),
            Redirect::Open("savings")
        );
        for (flags, route) in [
            (&["--graph"][..], "savings"),
            (&["--daily"], "savings"),
            (&["--by-tool", "--limit", "5"], "savings"),
            (&["--cost"], "spend"),
            (&["--agents"], "spend"),
            (&["--score"], "savings"),
            (&["--tasks"], "savings"),
            (&["--deep", "--model=gpt-5"], "savings"),
            (&["--pipeline"], "savings"),
            (&["--heatmap"], "context?detail=heatmap"),
            (&["--period", "week"], "savings"),
        ] {
            let a = args(flags);
            assert_eq!(
                decide(gain_route(&a), &a, PERSON, installed),
                Redirect::Open(route),
                "{flags:?}"
            );
        }
    }

    #[test]
    fn no_terminal_ci_opt_out_and_text_flags_keep_todays_text() {
        let quiet = [
            Surroundings {
                terminal: false,
                ..PERSON
            },
            Surroundings { ci: true, ..PERSON },
            Surroundings {
                opted_out: true,
                ..PERSON
            },
        ];
        for surroundings in quiet {
            let mut asked = false;
            let decision = decide(gain_route(&[]), &[], surroundings, || {
                asked = true;
                Ok(())
            });
            assert_eq!(decision, Redirect::Text, "{surroundings:?}");
            assert!(!asked, "availability is not probed without a person");
        }
        for flags in [
            &["--terminal"][..],
            &["--json"],
            &["--md"],
            &["--raw"],
            &["--export", "x.html"],
            &["--format", "json"],
            &["--format=markdown"],
            &["--svg"],
            &["--share"],
            &["--copy"],
            &["--reset"],
            &["--wrapped"],
            &["--opportunity"],
            &["--web"],
            &["--live"],
            &["--cost", "--terminal"],
            &["--unknown-flag"],
        ] {
            let a = args(flags);
            assert_eq!(
                decide(gain_route(&a), &a, PERSON, installed),
                Redirect::Text,
                "{flags:?}"
            );
        }
    }

    #[test]
    fn a_missing_cockpit_prints_the_text_with_the_install_line() {
        for reason in [
            CockpitUnavailable::NotInstalled,
            CockpitUnavailable::SourceBuild,
            CockpitUnavailable::Disabled,
            CockpitUnavailable::VerificationFailed,
        ] {
            assert_eq!(
                decide(Some("savings"), &[], PERSON, || Err(reason)),
                Redirect::TextWithInstallHint(reason)
            );
        }
        assert_eq!(
            super::cockpit_start::unavailable_line(CockpitUnavailable::NotInstalled),
            "Cockpit not installed \u{2014} lean-ctx cockpit install"
        );
    }
}
