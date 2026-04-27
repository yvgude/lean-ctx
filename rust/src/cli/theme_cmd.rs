use crate::core::config;
use crate::core::theme;

pub fn cmd_theme(args: &[String]) {
    let sub = args.first().map_or("list", std::string::String::as_str);
    let r = theme::rst();
    let b = theme::bold();
    let d = theme::dim();

    match sub {
        "list" => {
            let cfg = config::Config::load();
            let active = cfg.theme.as_str();
            println!();
            println!("  {b}Available themes:{r}");
            println!("  {ln}", ln = "─".repeat(40));
            for name in theme::PRESET_NAMES {
                let marker = if *name == active { " ◀ active" } else { "" };
                let Some(t) = theme::from_preset(name) else {
                    continue;
                };
                let preview = format!(
                    "{p}██{r}{s}██{r}{a}██{r}{sc}██{r}{w}██{r}",
                    p = t.primary.fg(),
                    s = t.secondary.fg(),
                    a = t.accent.fg(),
                    sc = t.success.fg(),
                    w = t.warning.fg(),
                );
                println!("  {preview}  {b}{name:<12}{r}{d}{marker}{r}");
            }
            if let Some(path) = theme::theme_file_path() {
                if path.exists() {
                    let custom = theme::load_theme("_custom_");
                    let preview = format!(
                        "{p}██{r}{s}██{r}{a}██{r}{sc}██{r}{w}██{r}",
                        p = custom.primary.fg(),
                        s = custom.secondary.fg(),
                        a = custom.accent.fg(),
                        sc = custom.success.fg(),
                        w = custom.warning.fg(),
                    );
                    let marker = if active == "custom" {
                        " ◀ active"
                    } else {
                        ""
                    };
                    println!("  {preview}  {b}{:<12}{r}{d}{marker}{r}", custom.name,);
                }
            }
            println!();
            println!("  {d}Set theme: lean-ctx theme set <name>{r}");
            println!();
        }
        "set" => {
            if args.len() < 2 {
                eprintln!("Usage: lean-ctx theme set <name>");
                std::process::exit(1);
            }
            let name = &args[1];
            if theme::from_preset(name).is_none() && name != "custom" {
                eprintln!(
                    "Unknown theme '{name}'. Available: {}",
                    theme::PRESET_NAMES.join(", ")
                );
                std::process::exit(1);
            }
            let mut cfg = config::Config::load();
            cfg.theme.clone_from(name);
            match cfg.save() {
                Ok(()) => {
                    let t = theme::load_theme(name);
                    println!("  {sc}✓{r} Theme set to {b}{name}{r}", sc = t.success.fg(),);
                    let preview = t.gradient_bar(0.75, 30);
                    println!("  {preview}");
                }
                Err(e) => eprintln!("Error: {e}"),
            }
        }
        "export" => {
            let cfg = config::Config::load();
            let t = theme::load_theme(&cfg.theme);
            println!("{}", t.to_toml());
        }
        "import" => {
            if args.len() < 2 {
                eprintln!("Usage: lean-ctx theme import <path>");
                std::process::exit(1);
            }
            let path = std::path::Path::new(&args[1]);
            if !path.exists() {
                eprintln!("File not found: {}", args[1]);
                std::process::exit(1);
            }
            match std::fs::read_to_string(path) {
                Ok(content) => match toml::from_str::<theme::Theme>(&content) {
                    Ok(imported) => match theme::save_theme(&imported) {
                        Ok(()) => {
                            let mut cfg = config::Config::load();
                            cfg.theme = "custom".to_string();
                            let _ = cfg.save();
                            println!(
                                "  {sc}✓{r} Imported theme '{name}' → ~/.lean-ctx/theme.toml",
                                sc = imported.success.fg(),
                                name = imported.name,
                            );
                            println!("  Config updated: theme = custom");
                        }
                        Err(e) => eprintln!("Error saving theme: {e}"),
                    },
                    Err(e) => eprintln!("Invalid theme file: {e}"),
                },
                Err(e) => eprintln!("Error reading file: {e}"),
            }
        }
        "preview" => {
            let name = args.get(1).map_or("default", std::string::String::as_str);
            let Some(t) = theme::from_preset(name) else {
                eprintln!("Unknown theme: {name}");
                std::process::exit(1);
            };
            println!();
            println!(
                "  {icon} {title}  {d}Theme Preview: {name}{r}",
                icon = t.header_icon(),
                title = t.brand_title(),
            );
            println!("  {ln}", ln = t.border_line(50));
            println!();
            println!(
                "  {b}{sc} 1.2M      {r}  {b}{sec} 87.3%     {r}  {b}{wrn} 4,521    {r}  {b}{acc} $12.50   {r}",
                sc = t.success.fg(),
                sec = t.secondary.fg(),
                wrn = t.warning.fg(),
                acc = t.accent.fg(),
            );
            println!("  {d} tokens saved   compression    commands       USD saved{r}");
            println!();
            println!(
                "  {b}{txt}Gradient Bar{r}      {bar}",
                txt = t.text.fg(),
                bar = t.gradient_bar(0.85, 30),
            );
            println!(
                "  {b}{txt}Sparkline{r}         {spark}",
                txt = t.text.fg(),
                spark = t.gradient_sparkline(&[20, 40, 30, 80, 60, 90, 70]),
            );
            println!();
            println!("  {top}", top = t.box_top(50));
            println!(
                "  {side}  {b}{txt}Box content with themed borders{r}                  {side_r}",
                side = t.box_side(),
                side_r = t.box_side(),
                txt = t.text.fg(),
            );
            println!("  {bot}", bot = t.box_bottom(50));
            println!();
        }
        _ => {
            eprintln!("Usage: lean-ctx theme [list|set|export|import|preview]");
            std::process::exit(1);
        }
    }
}
