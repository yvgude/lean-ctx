// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx models`: the subscription models of the connected Cloud plan
//! (Personal or team), with each model's origin and where it is hosted (mz6,
//! `docs/model-access-subscription.md` §4). Agents reach them through the
//! proxy's built-in `leanctx` provider.

const HELP: &str = "\
lean-ctx models — the models your LeanCTX Cloud plan includes

  lean-ctx models            List them: name, origin (vendor), hosts, status.
  lean-ctx models --json     The catalog as JSON (Local Read `cloud.models.catalog`).

Agents use them through the local proxy:
  http://127.0.0.1:<proxy port>/providers/leanctx/v1/chat/completions
Usage is paid from your plan's monthly model credit.
";

pub(crate) fn cmd_models(args: &[String]) {
    if args
        .iter()
        .any(|a| matches!(a.as_str(), "help" | "--help" | "-h"))
    {
        print!("{HELP}");
        return;
    }
    let rows = match lean_ctx_cloud::models_access::fetch_catalog() {
        Ok(rows) => rows,
        Err(error) => {
            eprintln!("models: {error}");
            std::process::exit(1);
        }
    };
    if rows.is_empty() {
        println!("No models in your plan's catalog.");
        return;
    }
    for row in rows {
        let hosts = if row.hosts.is_empty() {
            "no host".to_string()
        } else {
            row.hosts.join(", ")
        };
        println!(
            "{}  {} · origin {} ({}) · hosted {} · {}",
            row.id, row.display_name, row.origin_country, row.vendor, hosts, row.status
        );
    }
}
