//! `lean-ctx addon` — manage community addons (MCP extensions) (#858).
//!
//! Thin CLI over [`crate::core::addons`]: browse the registry, install an addon
//! (from the registry or a local `lean-ctx-addon.toml`), and remove it. `add`
//! and `remove` wire external code into the MCP gateway, so both pass through
//! the shared confirmation gate (`cli::prompt`).

use std::path::Path;

use crate::core::addons::manifest::AddonManifest;
use crate::core::addons::store::InstalledStore;
use crate::core::addons::{install, registry};

pub fn cmd_addon(args: &[String]) {
    let action = args.first().map_or("list", String::as_str);

    match action {
        "list" | "ls" => cmd_list(),
        "search" | "browse" => cmd_search(args.get(1).map_or("", String::as_str)),
        "info" | "show" => match positional(args) {
            Some(name) => cmd_info(&name),
            None => usage_exit("lean-ctx addon info <name>"),
        },
        "add" | "install" => match positional(args) {
            Some(target) => cmd_add(&target, args),
            None => usage_exit("lean-ctx addon add <name|path-to-lean-ctx-addon.toml>"),
        },
        "remove" | "rm" | "uninstall" => match positional(args) {
            Some(name) => cmd_remove(&name, args),
            None => usage_exit("lean-ctx addon remove <name>"),
        },
        "help" | "--help" | "-h" => print_help(),
        _ => {
            eprintln!("Unknown addon action: {action}");
            print_help();
            std::process::exit(1);
        }
    }
}

/// First non-flag argument after the action.
fn positional(args: &[String]) -> Option<String> {
    args.get(1)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && !s.starts_with('-'))
}

fn usage_exit(usage: &str) -> ! {
    eprintln!("Usage: {usage}");
    std::process::exit(1);
}

fn cmd_list() {
    let store = InstalledStore::load();
    let installed = store.list();

    if installed.is_empty() {
        println!("No addons installed.");
    } else {
        println!("Installed addons:\n");
        for a in &installed {
            let ver = if a.version.is_empty() {
                String::new()
            } else {
                format!(" v{}", a.version)
            };
            println!(
                "  ✓ {}{ver}  → gateway server `{}` ({})",
                a.name, a.gateway_server, a.source
            );
        }
    }

    let available = registry::all();
    if !available.is_empty() {
        println!("\nRegistry:\n");
        for m in &available {
            let installed_flag = if store.get(&m.addon.name).is_some() {
                " [installed]"
            } else {
                ""
            };
            let status = if m.is_installable() {
                ""
            } else {
                " · listed (no published endpoint yet)"
            };
            println!(
                "  • {} — {}{status}{installed_flag}",
                m.addon.name,
                first_line(&m.addon.description)
            );
        }
    }

    println!(
        "\nAdd one with `lean-ctx addon add <name>` · build your own with `lean-ctx addon help`."
    );
}

fn cmd_search(query: &str) {
    let hits = registry::search(query);
    if hits.is_empty() {
        println!("No addons match `{query}`.");
        return;
    }
    if query.trim().is_empty() {
        println!("All registry addons:\n");
    } else {
        println!("Addons matching `{query}`:\n");
    }
    for m in &hits {
        let status = if m.is_installable() {
            "installable"
        } else {
            "listed"
        };
        println!("  {} — {}", m.addon.name, m.display_name());
        println!("      {}", first_line(&m.addon.description));
        if m.addon.categories.is_empty() {
            println!("      {status}");
        } else {
            println!(
                "      categories: {} · {status}",
                m.addon.categories.join(", ")
            );
        }
    }
}

fn cmd_info(name: &str) {
    let store = InstalledStore::load();
    let Some(manifest) = registry::get(name).or_else(|| {
        // Allow `info` on a local manifest path too.
        looks_like_path(name)
            .then(|| AddonManifest::from_path(Path::new(name)).ok())
            .flatten()
    }) else {
        // Not in the registry and not a manifest path — but it may be a
        // locally-installed addon recorded in the store.
        if let Some(installed) = store.get(name) {
            println!("{}", installed.name);
            print_field("Version", &installed.version);
            println!(
                "  Status:    installed (gateway server `{}`, {})",
                installed.gateway_server, installed.source
            );
            return;
        }
        eprintln!(
            "Addon `{name}` not found. Try `lean-ctx addon search`, or pass a path to a \
             lean-ctx-addon.toml."
        );
        std::process::exit(1);
    };

    println!("{} ({})", manifest.display_name(), manifest.addon.name);
    if !manifest.addon.description.is_empty() {
        println!("  {}", manifest.addon.description);
    }
    print_field("Author", &manifest.addon.author);
    print_field("Version", &manifest.addon.version);
    print_field("License", &manifest.addon.license);
    print_field("Homepage", &manifest.addon.homepage);
    if !manifest.addon.categories.is_empty() {
        println!("  Categories: {}", manifest.addon.categories.join(", "));
    }

    if let Some(installed) = store.get(name) {
        println!(
            "  Status:    installed (gateway server `{}`, {})",
            installed.gateway_server, installed.source
        );
    } else if manifest.is_installable() {
        println!(
            "  Status:    installable — `lean-ctx addon add {}`",
            manifest.addon.name
        );
    } else {
        println!("  Status:    listed (no published MCP endpoint yet)");
    }

    if manifest.is_installable() {
        println!();
        print_install_preview(&manifest);
    }
}

fn cmd_add(target: &str, args: &[String]) {
    let (manifest, source) = if looks_like_path(target) {
        match AddonManifest::from_path(Path::new(target)) {
            Ok(m) => (m, "local".to_string()),
            Err(e) => {
                eprintln!("Error: {e}");
                std::process::exit(1);
            }
        }
    } else {
        let Some(m) = registry::get(target) else {
            eprintln!(
                "Unknown addon `{target}`.\n\
                 Browse with `lean-ctx addon search`, or pass a path to a \
                 lean-ctx-addon.toml."
            );
            std::process::exit(1);
        };
        (m, "registry".to_string())
    };

    if let Err(e) = manifest.validate() {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }

    if !manifest.is_installable() {
        eprintln!(
            "`{name}` is listed but not yet one-click installable (no published MCP endpoint).\n\
             Follow {home} — once it ships an MCP server, `lean-ctx addon add {name}` will \
             wire it automatically.",
            name = manifest.addon.name,
            home = if manifest.addon.homepage.is_empty() {
                "its homepage"
            } else {
                &manifest.addon.homepage
            }
        );
        std::process::exit(1);
    }

    println!("About to install `{}`:\n", manifest.addon.name);
    print_install_preview(&manifest);
    println!(
        "\nThis runs/connects to the above MCP server and exposes its tools through lean-ctx."
    );

    if !super::prompt::confirm(
        "Install this addon into the MCP gateway?",
        super::prompt::wants_yes(args),
    ) {
        println!("Aborted. Nothing was changed.");
        return;
    }

    match install::install(&manifest, &source) {
        Ok(outcome) => {
            println!(
                "\n✓ Installed `{}` → gateway server `{}`.",
                outcome.name, outcome.gateway_server
            );
            if outcome.enabled_gateway {
                println!("  Enabled the MCP gateway (gateway.enabled = true).");
            }
            println!(
                "  Its tools are reachable via `ctx_tools` (find/call). \
                 Restart your MCP client to pick them up."
            );
        }
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    }
}

fn cmd_remove(name: &str, args: &[String]) {
    if InstalledStore::load().get(name).is_none() {
        eprintln!("Addon `{name}` is not installed.");
        std::process::exit(1);
    }

    if !super::prompt::confirm(
        &format!("Remove addon `{name}` (unwire its MCP server)?"),
        super::prompt::wants_yes(args),
    ) {
        println!("Aborted.");
        return;
    }

    match install::remove(name) {
        Ok(outcome) => {
            println!(
                "✓ Removed `{}` (gateway server `{}`).",
                outcome.name, outcome.gateway_server
            );
            if outcome.last_removed {
                println!(
                    "  No addons remain. The gateway stays enabled — disable it with \
                     `lean-ctx config set gateway.enabled false` if you no longer need it."
                );
            }
        }
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    }
}

fn print_install_preview(manifest: &AddonManifest) {
    let mcp = &manifest.mcp;
    println!("  transport: {}", mcp.transport.as_str());
    match mcp.transport {
        crate::core::gateway::TransportKind::Stdio => {
            println!("  command:   {}", mcp.command);
            if !mcp.args.is_empty() {
                println!("  args:      {}", mcp.args.join(" "));
            }
            if !mcp.env.is_empty() {
                let keys: Vec<&str> = mcp.env.keys().map(String::as_str).collect();
                println!("  env:       {}", keys.join(", "));
            }
        }
        crate::core::gateway::TransportKind::Http => {
            println!("  url:       {}", mcp.url);
            if !mcp.headers.is_empty() {
                let keys: Vec<&str> = mcp.headers.keys().map(String::as_str).collect();
                println!("  headers:   {}", keys.join(", "));
            }
        }
    }
}

fn print_field(label: &str, value: &str) {
    if !value.trim().is_empty() {
        println!(
            "  {label}:{}{value}",
            " ".repeat(11usize.saturating_sub(label.len() + 1))
        );
    }
}

fn looks_like_path(target: &str) -> bool {
    Path::new(target)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"))
        || target.contains('/')
        || target.starts_with('.')
        || Path::new(target).is_file()
}

fn first_line(s: &str) -> String {
    let line = s.lines().next().unwrap_or("").trim();
    if line.chars().count() > 88 {
        let cut: String = line.chars().take(87).collect();
        format!("{cut}…")
    } else {
        line.to_string()
    }
}

fn print_help() {
    eprintln!(
        "lean-ctx addon — community extensions (MCP servers) for lean-ctx\n\
         \n\
         USAGE:\n    \
             lean-ctx addon <action> [args]\n\
         \n\
         ACTIONS:\n    \
             list                 List installed addons + the registry\n    \
             search [query]       Search the registry (empty = list all)\n    \
             info <name|path>     Show an addon's details + MCP wiring\n    \
             add <name|path>      Install from the registry or a local\n                         \
                                  lean-ctx-addon.toml (asks for confirmation)\n    \
             remove <name>        Uninstall an addon\n    \
             help                 Show this help\n\
         \n\
         FLAGS:\n    \
             -y, --yes            Skip the confirmation prompt (scripts/CI)\n\
         \n\
         BUILD YOUR OWN ADDON:\n    \
             1. Expose your tool as an MCP server (stdio binary or HTTP endpoint).\n    \
             2. Add a lean-ctx-addon.toml to your repo:\n\
         \n        \
                 [addon]\n        \
                 name = \"my-addon\"            # slug: [a-z0-9-]\n        \
                 display_name = \"My Addon\"\n        \
                 description = \"What it does, in one line.\"\n        \
                 author = \"you\"\n        \
                 homepage = \"https://github.com/you/my-addon\"\n        \
                 license = \"Apache-2.0\"\n        \
                 categories = [\"workflow\"]\n        \
                 keywords = [\"...\"]\n\
         \n        \
                 [mcp]\n        \
                 transport = \"stdio\"          # or \"http\"\n        \
                 command = \"my-addon-mcp\"     # stdio: executable to spawn\n        \
                 args = [\"serve\"]\n        \
                 # url = \"https://...\"         # http: streamable endpoint\n\
         \n    \
             3. Test it live:  lean-ctx addon add ./lean-ctx-addon.toml\n    \
             4. Get listed:    open a merge request adding your entry to\n                      \
                               rust/data/addon_registry.json (see docs/guides/addons.md).\n\
         \n    \
             Full guide: docs/guides/addons.md"
    );
}
