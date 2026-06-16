use crate::core::plugins::{
    PluginManager,
    executor::HookPoint,
    registry::{PluginRegistry, default_plugin_dir},
};

pub fn cmd_plugin(args: &[String]) {
    let action = args.first().map_or("help", String::as_str);

    match action {
        "list" | "ls" => cmd_list(),
        "enable" => {
            let Some(name) = args.get(1).map(String::as_str).filter(|s| !s.is_empty()) else {
                eprintln!("Usage: lean-ctx plugin enable <name>");
                std::process::exit(1);
            };
            cmd_enable(name);
        }
        "disable" => {
            let Some(name) = args.get(1).map(String::as_str).filter(|s| !s.is_empty()) else {
                eprintln!("Usage: lean-ctx plugin disable <name>");
                std::process::exit(1);
            };
            cmd_disable(name);
        }
        "info" => {
            let Some(name) = args.get(1).map(String::as_str).filter(|s| !s.is_empty()) else {
                eprintln!("Usage: lean-ctx plugin info <name>");
                std::process::exit(1);
            };
            cmd_info(name);
        }
        "init" => {
            let Some(name) = args.get(1).map(String::as_str).filter(|s| !s.is_empty()) else {
                eprintln!("Usage: lean-ctx plugin init <name>");
                std::process::exit(1);
            };
            cmd_init(name);
        }
        "hooks" => cmd_hooks(),
        "help" | "--help" | "-h" => print_help(),
        _ => {
            eprintln!("Unknown plugin action: {action}");
            print_help();
            std::process::exit(1);
        }
    }
}

fn cmd_list() {
    let mut registry = PluginRegistry::from_default_dir();
    let errors = registry.discover();
    for err in &errors {
        eprintln!("Warning: {}: {}", err.path.display(), err.error);
    }

    let plugins = registry.list();
    if plugins.is_empty() {
        println!("No plugins installed.");
        println!("\nPlugin directory: {}", default_plugin_dir().display());
        println!("Use 'lean-ctx plugin init <name>' to create a plugin template.");
        return;
    }

    println!("Installed plugins:\n");
    for plugin in &plugins {
        let status = if plugin.enabled { "✓" } else { "✗" };
        let hooks_count = plugin.manifest.hooks.len();
        println!(
            "  [{status}] {} v{} ({hooks_count} hook{})",
            plugin.manifest.plugin.name,
            plugin.manifest.plugin.version,
            if hooks_count == 1 { "" } else { "s" }
        );
        if !plugin.manifest.plugin.description.is_empty() {
            println!("      {}", plugin.manifest.plugin.description);
        }
    }
    println!("\nPlugin directory: {}", default_plugin_dir().display());
}

fn cmd_enable(name: &str) {
    PluginManager::init();
    match PluginManager::with_registry_mut(|reg| reg.enable(name)) {
        Some(Ok(())) => println!("Enabled plugin: {name}"),
        Some(Err(e)) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
        None => {
            eprintln!("Error: plugin registry not initialized");
            std::process::exit(1);
        }
    }
}

fn cmd_disable(name: &str) {
    PluginManager::init();
    match PluginManager::with_registry_mut(|reg| reg.disable(name)) {
        Some(Ok(())) => println!("Disabled plugin: {name}"),
        Some(Err(e)) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
        None => {
            eprintln!("Error: plugin registry not initialized");
            std::process::exit(1);
        }
    }
}

fn cmd_info(name: &str) {
    let mut registry = PluginRegistry::from_default_dir();
    registry.discover();

    if let Some(plugin) = registry.get(name) {
        println!("Plugin: {}", plugin.manifest.plugin.name);
        println!("Version: {}", plugin.manifest.plugin.version);
        if !plugin.manifest.plugin.description.is_empty() {
            println!("Description: {}", plugin.manifest.plugin.description);
        }
        if !plugin.manifest.plugin.author.is_empty() {
            println!("Author: {}", plugin.manifest.plugin.author);
        }
        println!("Enabled: {}", plugin.enabled);
        println!("Path: {}", plugin.path.display());
        if !plugin.manifest.hooks.is_empty() {
            println!("\nHooks:");
            for (hook_name, entry) in &plugin.manifest.hooks {
                println!(
                    "  {hook_name}: {} (timeout: {}ms)",
                    entry.command, entry.timeout_ms
                );
            }
        }
    } else {
        eprintln!("Plugin not found: {name}");
        std::process::exit(1);
    }
}

fn cmd_init(name: &str) {
    let dir = default_plugin_dir();
    match crate::core::plugins::init_plugin_template(name, &dir) {
        Ok(()) => {
            println!("Created plugin template: {}", dir.join(name).display());
            println!("\nNext steps:");
            println!("  1. Edit {}/plugin.toml", dir.join(name).display());
            println!("  2. Implement your plugin binary");
            println!("  3. Run 'lean-ctx plugin list' to verify");
        }
        Err(e) => {
            eprintln!("Error creating plugin template: {e}");
            std::process::exit(1);
        }
    }
}

fn cmd_hooks() {
    println!("Available hook points:\n");
    for name in HookPoint::all_hook_names() {
        let desc = match *name {
            "on_session_start" => "Called when a new lean-ctx session begins",
            "on_session_end" => "Called when a session ends",
            "pre_read" => "Called before a file is read (receives {path} in stdin)",
            "post_compress" => {
                "Called after compression (receives {path, original_tokens, compressed_tokens})"
            }
            "on_knowledge_update" => "Called when knowledge is updated (receives {fact_id})",
            _ => "",
        };
        println!("  {name}");
        println!("    {desc}\n");
    }
}

fn print_help() {
    eprintln!(
        "lean-ctx plugin — Plugin management\n\
         \n\
         USAGE:\n    \
             lean-ctx plugin <action> [args]\n\
         \n\
         ACTIONS:\n    \
             list              List installed plugins\n    \
             enable <name>     Enable a plugin\n    \
             disable <name>    Disable a plugin\n    \
             info <name>       Show plugin details\n    \
             init <name>       Create a plugin template\n    \
             hooks             List available hook points\n    \
             help              Show this help"
    );
}
