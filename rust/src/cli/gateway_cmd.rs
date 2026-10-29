// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx gateway` — why a block of a model request was withheld, and the
//! human-only override.
//!
//! - `why <id>` / `list`: the recorded, content-free explanation.
//! - `allow-once <id> [--hours N]`: let exactly that block go to the provider
//!   it was withheld from, for N hours (default 24, at most 168), so the
//!   re-sends of a conversation stay consistent. Only a person can do this:
//!   stdin must be a terminal, no agent CLI may be an ancestor, and the id is
//!   typed on the terminal. Audited (`x-gateway_override`).
//! - `revoke <id>` (alias `redact`): withdraw a grant; the block is withheld
//!   again on the next request.

use crate::core::context_admission::withheld::{self, WithheldBlock};

const DEFAULT_HOURS: u64 = 24;
const MAX_HOURS: u64 = 168;
const REMOTE_RESTRICTED: &str = "destination.remote_restricted";

pub(crate) fn cmd_gateway(args: &[String]) {
    let code = run(args);
    if code != 0 {
        std::process::exit(code);
    }
}

fn run(args: &[String]) -> i32 {
    let json = args.iter().any(|a| a == "--json");
    let positional: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|a| *a != "--json")
        .collect();
    match positional.as_slice() {
        ["why", id] => why(id, json),
        ["list"] => list(json),
        ["allow-once", id] => allow_once(id, DEFAULT_HOURS),
        ["allow-once", id, "--hours", hours] => match hours.parse::<u64>() {
            Ok(hours) if (1..=MAX_HOURS).contains(&hours) => allow_once(id, hours),
            _ => fail(&format!("--hours takes 1 to {MAX_HOURS}")),
        },
        ["revoke" | "redact", id] => revoke(id),
        _ => {
            print_usage();
            i32::from(!positional.is_empty() && !matches!(positional[0], "help" | "--help" | "-h"))
                * 2
        }
    }
}

fn fail(message: &str) -> i32 {
    eprintln!("lean-ctx gateway: {message}");
    1
}

fn print_usage() {
    println!(
        "Why the Context Gateway withheld a block of a model request, and the human override.\n\n\
         Usage: lean-ctx gateway why <id> [--json]\n\
         \x20      lean-ctx gateway list [--json]\n\
         \x20      lean-ctx gateway allow-once <id> [--hours N]   (a person, in their own terminal)\n\
         \x20      lean-ctx gateway revoke <id>                   (alias: redact)\n\n\
         <id> is the w-… id in the withheld-content notice or in the refusal of a request.\n\
         allow-once lets exactly that block (the same bytes) go to the provider it was\n\
         withheld from for N hours (default {DEFAULT_HOURS}, at most {MAX_HOURS}); it waives only\n\
         destination.remote_restricted and only in developer mode. It cannot run under an\n\
         agent or without a terminal, and it is recorded in the audit trail."
    );
}

fn known(id: &str) -> Result<WithheldBlock, i32> {
    if !withheld::valid_id(id) {
        return Err(fail(&format!(
            "not a block id: {id} (expected w- and 12 hex digits)"
        )));
    }
    withheld::lookup(id).ok_or_else(|| {
        fail(&format!(
            "no withheld block {id} is recorded here (it was withheld by another installation, \
             or its record was pruned)"
        ))
    })
}

fn explain_lines(block: &WithheldBlock) -> Vec<String> {
    let mut lines = vec![
        format!("block      {}", block.id),
        format!("position   {}", block.position),
        format!("withheld   from {} ({})", block.provider, block.locality),
        format!("mode       {}", block.mode),
        "reasons".to_owned(),
    ];
    for code in &block.reason_codes {
        let sentence = super::inspect_cmd::explain(code);
        lines.push(format!("  {code}  {sentence}"));
    }
    lines
}

fn next_step(block: &WithheldBlock) -> String {
    let grant = withheld::grant(&block.id);
    if let Some(grant) = grant {
        return format!(
            "allowed for {} until unix {} — `lean-ctx gateway revoke {}` withholds it again",
            grant.provider, grant.expires_unix, block.id
        );
    }
    if waivable(block) {
        format!(
            "the rest of the request was sent; the block is withheld on every re-send.\n\
             If it is not sensitive, a person can allow it in their own terminal:\n  \
             lean-ctx gateway allow-once {}",
            block.id
        )
    } else {
        "no grant waives these reasons; change the content or the policy \
         ([context_gateway] in config.toml, or the project policy)"
            .to_owned()
    }
}

/// Only the destination rule can be waived, and only where the gateway
/// decided in developer mode; every other reason is the policy itself.
fn waivable(block: &WithheldBlock) -> bool {
    const ALSO_WAIVED: [&str; 3] = [
        "classification.flagged",
        "secret.flagged",
        "egress.immutable_position",
    ];
    block.mode == "developer"
        && block.reason_codes.iter().any(|c| c == REMOTE_RESTRICTED)
        && block
            .reason_codes
            .iter()
            .all(|c| c == REMOTE_RESTRICTED || ALSO_WAIVED.contains(&c.as_str()))
}

fn why(id: &str, json: bool) -> i32 {
    let block = match known(id) {
        Ok(block) => block,
        Err(code) => return code,
    };
    if json {
        let grant = withheld::grant(id);
        let value = serde_json::json!({
            "block": block,
            "waivable": waivable(&block),
            "granted_until_unix": grant.map(|g| g.expires_unix),
        });
        println!("{value:#}");
    } else {
        for line in explain_lines(&block) {
            println!("{line}");
        }
        println!("\n{}", next_step(&block));
    }
    0
}

fn list(json: bool) -> i32 {
    let blocks = withheld::list();
    if json {
        println!("{:#}", serde_json::json!({ "blocks": blocks }));
        return 0;
    }
    if blocks.is_empty() {
        println!("no withheld blocks are recorded");
        return 0;
    }
    for block in &blocks {
        let granted = if withheld::grant(&block.id).is_some() {
            "  [allowed]"
        } else {
            ""
        };
        println!(
            "{}  {}  {}  {}{granted}",
            block.id,
            block.position,
            block.provider,
            block.reason_codes.join(",")
        );
    }
    0
}

fn allow_once(id: &str, hours: u64) -> i32 {
    let block = match known(id) {
        Ok(block) => block,
        Err(code) => return code,
    };
    if !waivable(&block) {
        return fail(&format!(
            "{id} was withheld for {}; allow-once waives only {REMOTE_RESTRICTED} in developer mode",
            block.reason_codes.join(", ")
        ));
    }
    for line in explain_lines(&block) {
        eprintln!("{line}");
    }
    let prompt = format!(
        "\nAllow block {id} to go to {} for {hours} h? The content itself is not shown here.\n\
         Type the block id to allow it: ",
        block.provider
    );
    if let Err(reason) = crate::core::hive::consent::channel::human_typed(&prompt, id) {
        return fail(&format!("not allowed: {reason}"));
    }
    match withheld::write_grant(&block, hours) {
        Ok(grant) => {
            audit("allow-once", id);
            println!(
                "allowed {id} for {} until unix {} (revoke: lean-ctx gateway revoke {id})",
                grant.provider, grant.expires_unix
            );
            0
        }
        Err(reason) => fail(&reason),
    }
}

fn revoke(id: &str) -> i32 {
    match withheld::revoke(id) {
        Ok(true) => {
            audit("revoke", id);
            println!("{id} is withheld again on the next request");
            0
        }
        Ok(false) => {
            println!("{id} had no grant; it stays withheld");
            0
        }
        Err(reason) => fail(&reason),
    }
}

fn audit(action: &str, id: &str) {
    use crate::core::audit_trail::{AuditEntryData, AuditEventType, record};
    record(AuditEntryData {
        agent_id: "human-cli".to_owned(),
        tool: "gateway".to_owned(),
        action: Some(format!("{action}:{id}")),
        input_hash: id.to_owned(),
        output_tokens: 0,
        role: "human".to_owned(),
        event_type: AuditEventType::Unknown("x-gateway_override".to_owned()),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(codes: &[&str], mode: &str) -> WithheldBlock {
        WithheldBlock {
            id: "w-0123456789ab".into(),
            reason_codes: codes.iter().map(|c| (*c).to_owned()).collect(),
            position: "message 3".into(),
            provider: "anthropic".into(),
            locality: "remote".into(),
            mode: mode.into(),
        }
    }

    #[test]
    fn only_the_destination_rule_in_developer_mode_is_waivable() {
        assert!(waivable(&block(
            &["classification.flagged", REMOTE_RESTRICTED],
            "developer"
        )));
        assert!(!waivable(&block(
            &["classification.flagged", REMOTE_RESTRICTED],
            "governed"
        )));
        assert!(!waivable(&block(&["secret.blocked"], "developer")));
        assert!(!waivable(&block(
            &["policy.content_withheld", REMOTE_RESTRICTED],
            "developer"
        )));
    }

    #[test]
    fn allow_once_refuses_without_a_terminal() {
        // Without a terminal on stdin, as in an agent's shell. (Run from an
        // interactive terminal the prompt would wait for a person.)
        if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
            return;
        }
        let _env = crate::core::context_admission::withheld::tests_support::isolated();
        let id = crate::core::context_admission::withheld::tests_support::seed_block(&[
            "classification.flagged",
            REMOTE_RESTRICTED,
        ]);
        assert_eq!(allow_once(&id, 1), 1);
        assert!(withheld::grant(&id).is_none(), "no grant without a person");
    }

    #[test]
    fn usage_and_bad_ids_fail_closed() {
        assert_eq!(run(&["why".into(), "nope".into()]), 1);
        assert_eq!(run(&["allow-once".into(), "w-xyz".into()]), 1);
        assert_eq!(run(&["bogus".into()]), 2);
        assert_eq!(run(&[]), 0);
    }
}
