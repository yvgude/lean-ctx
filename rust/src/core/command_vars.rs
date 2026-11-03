// SPDX-License-Identifier: Apache-2.0
//! Which shell variables a command has certainly set to a literal (#1953, #2079).
//!
//! The write guards follow a command the way the shell would, but only as far
//! as the text alone allows. A variable is known when this same command set it
//! to a literal earlier, in a way that certainly took effect in the current
//! shell, or when it is `$TMPDIR` — the scratch variable the guards themselves
//! name as an allowed destination — and the command has not rebound it. Every
//! other variable stays unknown, and an unknown variable leaves whatever
//! depends on it unresolved.

use std::collections::HashMap;

use super::shell_allowlist::Separator;

/// The variables known to hold a value when one segment starts.
#[derive(Debug, Clone, Default)]
pub struct KnownVars {
    literals: HashMap<String, String>,
    /// `$TMPDIR` may no longer hold the environment's value: the command
    /// assigned it something this cannot read, or ran a segment that can
    /// rebind variables. `TMPDIR=$PWD; cd $TMPDIR` must not be read as the
    /// scratch directory.
    tmpdir_rebound: bool,
}

impl KnownVars {
    fn set(&mut self, name: &str, value: Option<&str>) {
        if let Some(value) = value {
            self.literals.insert(name.to_string(), value.to_string());
        } else {
            self.literals.remove(name);
            if name == "TMPDIR" {
                self.tmpdir_rebound = true;
            }
        }
    }

    fn forget_all(&mut self) {
        self.literals.clear();
        self.tmpdir_rebound = true;
    }

    /// `env_tmpdir` supplies the environment's `$TMPDIR`; it is asked only
    /// when that is the value in effect.
    fn get(&self, name: &str, env_tmpdir: impl FnOnce() -> Option<String>) -> Option<String> {
        if let Some(value) = self.literals.get(name) {
            return Some(value.clone());
        }
        if name == "TMPDIR" && !self.tmpdir_rebound {
            return env_tmpdir().filter(|v| !v.is_empty());
        }
        None
    }
}

/// For each segment, the variables known to hold a literal value when that
/// segment starts (#1953).
///
/// Only a standalone `NAME=value` / `export NAME=value` segment that certainly
/// runs in the current shell counts: not behind `&&`/`||` (it may be skipped),
/// not in a pipeline or the background (a subshell, so nothing propagates).
/// A value is literal only without `$`, a backtick or a leading `~`. Any
/// segment that can rebind variables some other way (`read`, `for`, `eval`,
/// `source`, …) forgets everything known so far. Where this cannot follow the
/// shell it knows less, and an unknown variable leaves the target refused.
pub fn literal_assignments_before_each_segment(
    command: &str,
    segment_count: usize,
) -> Vec<KnownVars> {
    let separated = super::shell_allowlist::segments_with_separators(command);
    if separated.len() != segment_count {
        // Both come from the same scanner; if they ever disagree, resolve
        // nothing — not even `$TMPDIR`.
        let nothing = KnownVars {
            tmpdir_rebound: true,
            ..KnownVars::default()
        };
        return vec![nothing; segment_count];
    }
    literal_assignments_before_each(&separated)
}

/// [`literal_assignments_before_each_segment`] over segments already split.
pub(crate) fn literal_assignments_before_each(
    separated: &[(String, Option<Separator>)],
) -> Vec<KnownVars> {
    let mut known = KnownVars::default();
    let mut prev: Option<Separator> = None;
    let mut before_each = Vec::with_capacity(separated.len());
    for (segment, sep) in separated {
        let sep = *sep;
        before_each.push(known.clone());
        let tokens = super::shell_allowlist::shell_tokenize(segment);
        let words: &[String] = match tokens.first().map(String::as_str) {
            Some("export") => &tokens[1..],
            _ => &tokens,
        };
        let assignments: Option<Vec<(&str, &str)>> = words
            .iter()
            .map(|word| {
                let (name, value) = word.split_once('=')?;
                let valid_name = name
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                    && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
                valid_name.then_some((name, value))
            })
            .collect();
        match assignments {
            Some(pairs) if !pairs.is_empty() => {
                let in_current_shell = matches!(prev, None | Some(Separator::Sequence))
                    && matches!(sep, None | Some(Separator::Sequence | Separator::And));
                for (name, value) in pairs {
                    let literal = !value.contains(['$', '`']) && !value.starts_with('~');
                    known.set(name, (in_current_shell && literal).then_some(value));
                }
            }
            _ => {
                let rebinds = matches!(
                    tokens.first().map(String::as_str),
                    Some(
                        "read"
                            | "for"
                            | "select"
                            | "eval"
                            | "source"
                            | "."
                            | "unset"
                            | "declare"
                            | "typeset"
                            | "local"
                            | "readonly"
                            | "export"
                            | "mapfile"
                            | "readarray"
                            | "getopts"
                            | "printf"
                    )
                );
                if rebinds {
                    known.forget_all();
                }
            }
        }
        prev = sep;
    }
    before_each
}

/// Expand a target that starts with `$NAME` or `${NAME}` from `known` — which
/// includes `$TMPDIR` from the environment unless the command rebound it.
/// `None` when the target starts with no variable or an unknown one.
pub fn expand_leading_variable(path: &str, known: &KnownVars) -> Option<String> {
    expand_with(path, known, || std::env::var("TMPDIR").ok())
}

fn expand_with(
    path: &str,
    known: &KnownVars,
    env_tmpdir: impl FnOnce() -> Option<String>,
) -> Option<String> {
    let rest = path.strip_prefix('$')?;
    let (name, tail) = if let Some(braced) = rest.strip_prefix('{') {
        let (name, tail) = braced.split_once('}')?;
        (name, tail)
    } else {
        let end = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(rest.len());
        rest.split_at(end)
    };
    if name.is_empty() || !(tail.is_empty() || tail.starts_with('/')) {
        return None;
    }
    Some(format!("{}{tail}", known.get(name, env_tmpdir)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn before_last(command: &str) -> KnownVars {
        let separated = crate::core::shell_allowlist::segments_with_separators(command);
        literal_assignments_before_each(&separated)
            .pop()
            .expect("at least one segment")
    }

    #[test]
    fn a_literal_assignment_in_the_current_shell_is_known() {
        let known = before_last("D=/tmp/x && ls");
        assert_eq!(
            expand_leading_variable("$D/f", &known).as_deref(),
            Some("/tmp/x/f")
        );
        assert_eq!(
            expand_leading_variable("${D}/f", &known).as_deref(),
            Some("/tmp/x/f")
        );
        assert_eq!(expand_leading_variable("$Dx/f", &known), None);
    }

    fn expand_tmpdir(command: &str) -> Option<String> {
        expand_with("$TMPDIR/a", &before_last(command), || {
            Some("/var/folders/xy/T".to_string())
        })
    }

    /// `$TMPDIR` is the environment's — until the command rebinds it.
    #[test]
    fn tmpdir_is_known_only_while_the_command_has_not_rebound_it() {
        assert_eq!(expand_tmpdir("ls").as_deref(), Some("/var/folders/xy/T/a"));
        assert_eq!(
            expand_tmpdir("TMPDIR=/tmp/own; ls").as_deref(),
            Some("/tmp/own/a"),
            "a literal rebinding is followed"
        );
        for command in [
            "TMPDIR=$PWD; ls",
            "export TMPDIR=$(pwd); ls",
            "true && TMPDIR=/tmp/maybe; ls",
            "read TMPDIR; ls",
            "unset TMPDIR; ls",
        ] {
            assert_eq!(expand_tmpdir(command), None, "{command}");
        }
    }
}
