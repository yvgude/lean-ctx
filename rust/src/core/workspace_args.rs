// SPDX-License-Identifier: Apache-2.0
//! Argument grammar of `lean-ctx workspace …` (#2120), shared by the command
//! and by the agent-shell allowlist, so both judge the same verb.

/// One parsed `lean-ctx workspace` invocation.
pub(crate) struct Parsed {
    pub(crate) verb: String,
    pub(crate) positional: Option<String>,
    pub(crate) file: Option<String>,
    pub(crate) id: Option<String>,
    pub(crate) expect_revision: Option<String>,
    pub(crate) read_only: bool,
    pub(crate) json: bool,
}

/// Parse the arguments after `lean-ctx workspace`.
pub(crate) fn parse(args: &[String]) -> Result<Parsed, String> {
    let mut parsed = Parsed {
        verb: String::new(),
        positional: None,
        file: None,
        id: None,
        expect_revision: None,
        read_only: false,
        json: false,
    };
    let mut words = parse_into(args, &mut parsed)?.into_iter();
    parsed.verb = words.next().ok_or("a command is required")?;
    parsed.positional = words.next();
    if let Some(extra) = words.next() {
        return Err(format!("unexpected argument: {extra}"));
    }
    Ok(parsed)
}

/// Fill `parsed` from flags (`--flag value` or `--flag=value`); returns the
/// remaining words in order.
fn parse_into(args: &[String], parsed: &mut Parsed) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--json" => parsed.json = true,
            "--read-only" => parsed.read_only = true,
            "--file" | "--id" | "--expect-revision" => {
                let value = rest
                    .next()
                    .ok_or_else(|| format!("{arg} needs a value"))?
                    .clone();
                let slot = match arg.as_str() {
                    "--file" => &mut parsed.file,
                    "--id" => &mut parsed.id,
                    _ => &mut parsed.expect_revision,
                };
                if slot.replace(value).is_some() {
                    return Err(format!("{arg} is given twice"));
                }
            }
            flag if flag.starts_with("--") => match flag.split_once('=') {
                Some((name, _)) if matches!(name, "--json" | "--read-only") => {
                    return Err(format!("{name} takes no value"));
                }
                Some((name, value)) => {
                    let pair = [name.to_string(), value.to_string()];
                    words.extend(parse_into(&pair, parsed)?);
                }
                None => return Err(format!("unknown argument: {flag}")),
            },
            _ => words.push(arg.clone()),
        }
    }
    Ok(words)
}

/// Whether an agent shell may run `lean-ctx workspace <args>`: only forms
/// that read or narrow (`show`, `diagnose`, `remove`, help). Anything that
/// does not parse is refused — the verb must not hide behind a flag.
pub(crate) fn agent_shell_may_run(args: &[String]) -> bool {
    if args
        .first()
        .is_none_or(|first| matches!(first.as_str(), "-h" | "--help" | "help"))
    {
        return true;
    }
    parse(args)
        .is_ok_and(|parsed| matches!(parsed.verb.as_str(), "show" | "diagnose" | "remove" | "rm"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn agent_shells_may_only_read_or_narrow() {
        for allowed in [
            "",
            "--help",
            "show",
            "diagnose --json",
            "remove --file /w /x",
        ] {
            assert!(agent_shell_may_run(&words(allowed)), "{allowed}");
        }
        for refused in [
            "add /etc",
            "--file /w.json --expect-revision 3 add /etc",
            "--json add /etc",
            "--read-only add /etc",
            "--json=x add /etc",
            "create --id x /code",
            "--id x create /code",
            "frobnicate",
        ] {
            assert!(!agent_shell_may_run(&words(refused)), "{refused}");
        }
    }
}
