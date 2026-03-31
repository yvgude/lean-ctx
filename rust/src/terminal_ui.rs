use std::io::{self, IsTerminal, Write};

const LOGO: [&str; 6] = [
    r"  ██╗     ███████╗ █████╗ ███╗   ██╗     ██████╗████████╗██╗  ██╗",
    r"  ██║     ██╔════╝██╔══██╗████╗  ██║    ██╔════╝╚══██╔══╝╚██╗██╔╝",
    r"  ██║     █████╗  ███████║██╔██╗ ██║    ██║        ██║    ╚███╔╝ ",
    r"  ██║     ██╔══╝  ██╔══██║██║╚██╗██║    ██║        ██║    ██╔██╗ ",
    r"  ███████╗███████╗██║  ██║██║ ╚████║    ╚██████╗   ██║   ██╔╝ ██╗",
    r"  ╚══════╝╚══════╝╚═╝  ╚═╝╚═╝  ╚═══╝     ╚═════╝   ╚═╝   ╚═╝  ╚═╝",
];

const TAGLINE: &str = "The Intelligence Layer for AI Coding";

fn hsl_to_rgb(h: f64, s: f64, l: f64) -> (u8, u8, u8) {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let h2 = h / 60.0;
    let x = c * (1.0 - (h2 % 2.0 - 1.0).abs());
    let (r1, g1, b1) = match h2 as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    (
        ((r1 + m) * 255.0) as u8,
        ((g1 + m) * 255.0) as u8,
        ((b1 + m) * 255.0) as u8,
    )
}

fn rgb_fg(r: u8, g: u8, b: u8) -> String {
    format!("\x1b[38;2;{r};{g};{b}m")
}

pub fn print_logo_animated() {
    let cfg = crate::core::config::Config::load();
    let t = crate::core::theme::load_theme(&cfg.theme);
    print_logo_animated_themed(&t);
}

pub fn print_logo_animated_themed(t: &crate::core::theme::Theme) {
    if crate::core::theme::no_color() {
        print_logo_plain();
        return;
    }
    if !io::stdout().is_terminal() {
        print_logo_themed_static(t);
        return;
    }

    let mut stdout = io::stdout();
    let frames = 32;
    let frame_ms = 50;

    for frame in 0..frames {
        if frame > 0 {
            print!("\x1b[{}A", LOGO.len() + 2);
        }

        let base_hue = (frame as f64 / frames as f64) * 360.0;

        for (i, line) in LOGO.iter().enumerate() {
            let chars: Vec<char> = line.chars().collect();
            let mut buf = String::with_capacity(chars.len() * 20);

            for (j, ch) in chars.iter().enumerate() {
                if *ch == ' ' {
                    buf.push(' ');
                    continue;
                }
                let hue = (base_hue + (j as f64 * 2.5) + (i as f64 * 15.0)) % 360.0;
                let (r, g, b) = hsl_to_rgb(hue, 0.85, 0.65);
                buf.push_str(&rgb_fg(r, g, b));
                buf.push(*ch);
            }
            buf.push_str("\x1b[0m");
            let _ = writeln!(stdout, "{buf}");
        }

        let tag_hue = (base_hue + 120.0) % 360.0;
        let (tr, tg, tb) = hsl_to_rgb(tag_hue, 0.5, 0.55);
        let _ = writeln!(
            stdout,
            "{}             {TAGLINE}\x1b[0m",
            rgb_fg(tr, tg, tb)
        );
        let _ = writeln!(stdout);

        let _ = stdout.flush();
        std::thread::sleep(std::time::Duration::from_millis(frame_ms));
    }

    print!("\x1b[{}A", LOGO.len() + 2);
    print_logo_themed_static(t);
}

pub fn print_logo_static() {
    let cfg = crate::core::config::Config::load();
    let t = crate::core::theme::load_theme(&cfg.theme);
    print_logo_themed_static(&t);
}

fn print_logo_themed_static(t: &crate::core::theme::Theme) {
    if crate::core::theme::no_color() {
        print_logo_plain();
        return;
    }
    let mut stdout = io::stdout();

    for (i, line) in LOGO.iter().enumerate() {
        let chars: Vec<char> = line.chars().collect();
        let mut buf = String::with_capacity(chars.len() * 20);

        for (j, ch) in chars.iter().enumerate() {
            if *ch == ' ' {
                buf.push(' ');
                continue;
            }
            let progress = if chars.len() > 1 {
                j as f64 / (chars.len() - 1) as f64
            } else {
                0.5
            };
            let row_t = i as f64 / (LOGO.len() - 1).max(1) as f64;
            let blend = (progress + row_t * 0.3).min(1.0);
            let c = t.primary.lerp(&t.secondary, blend);
            buf.push_str(&c.fg());
            buf.push(*ch);
        }
        buf.push_str("\x1b[0m");
        let _ = writeln!(stdout, "{buf}");
    }

    let _ = writeln!(stdout, "{}             {TAGLINE}\x1b[0m", t.muted.fg());
    let _ = writeln!(stdout);
    let _ = stdout.flush();
}

fn print_logo_plain() {
    for line in &LOGO {
        println!("{line}");
    }
    println!("             {TAGLINE}");
    println!();
}

pub fn print_command_box() {
    let dim = "\x1b[2m";
    let rst = "\x1b[0m";
    let bold = "\x1b[1m";
    let cyan = "\x1b[36m";
    let green = "\x1b[32m";

    println!("  {dim}┌─────────────────────────────────────────────────────────┐{rst}");
    println!(
        "  {dim}│{rst}  {cyan}{bold}lean-ctx gain{rst}        {dim}Token savings dashboard{rst}         {dim}│{rst}"
    );
    println!(
        "  {dim}│{rst}  {cyan}{bold}lean-ctx dashboard{rst}   {dim}Web analytics (browser){rst}        {dim}│{rst}"
    );
    println!(
        "  {dim}│{rst}  {cyan}{bold}lean-ctx benchmark{rst}   {dim}Test compression quality{rst}        {dim}│{rst}"
    );
    println!(
        "  {dim}│{rst}  {cyan}{bold}lean-ctx config{rst}      {dim}Edit settings{rst}                   {dim}│{rst}"
    );
    println!(
        "  {dim}│{rst}  {cyan}{bold}lean-ctx doctor{rst}      {dim}Verify installation{rst}             {dim}│{rst}"
    );
    println!(
        "  {dim}│{rst}  {cyan}{bold}lean-ctx update{rst}      {dim}Self-update to latest{rst}           {dim}│{rst}"
    );
    println!(
        "  {dim}│{rst}  {cyan}{bold}lean-ctx off{rst} / {cyan}{bold}on{rst}    {dim}Toggle compression{rst}              {dim}│{rst}"
    );
    println!(
        "  {dim}│{rst}  {cyan}{bold}lean-ctx uninstall{rst}   {dim}Clean removal{rst}                   {dim}│{rst}"
    );
    println!("  {dim}└─────────────────────────────────────────────────────────┘{rst}");
    println!("  {green}Ready!{rst} Your next AI command will be automatically optimized.");
    println!("  {dim}Docs: https://leanctx.com/docs{rst}");
    println!();
}

pub fn print_step_header(step: u8, total: u8, title: &str) {
    let dim = "\x1b[2m";
    let bold = "\x1b[1m";
    let cyan = "\x1b[36m";
    let rst = "\x1b[0m";
    println!();
    println!("  {cyan}{bold}[{step}/{total}]{rst} {bold}{title}{rst}");
    println!("  {dim}─────────────────────────────────────────────────────{rst}");
}

pub fn print_status_ok(msg: &str) {
    println!("  \x1b[32m✓\x1b[0m {msg}");
}

pub fn print_status_skip(msg: &str) {
    println!("  \x1b[2m○\x1b[0m \x1b[2m{msg}\x1b[0m");
}

pub fn print_status_new(msg: &str) {
    println!("  \x1b[1;32m✓\x1b[0m \x1b[1m{msg}\x1b[0m");
}

pub fn print_status_warn(msg: &str) {
    println!("  \x1b[33m⚠\x1b[0m {msg}");
}

pub fn spinner_tick(msg: &str, frame: usize) {
    let frames = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    let ch = frames[frame % frames.len()];
    print!("\r  \x1b[36m{ch}\x1b[0m {msg}");
    let _ = io::stdout().flush();
}

pub fn spinner_done(msg: &str) {
    print!("\r  \x1b[32m✓\x1b[0m {msg}\x1b[K\n");
    let _ = io::stdout().flush();
}

pub fn print_setup_header() {
    let dim = "\x1b[2m";
    let bold = "\x1b[1m";
    let green = "\x1b[32m";
    let rst = "\x1b[0m";
    println!();
    println!("  {dim}╭──────────────────────────────────────────╮{rst}");
    println!(
        "  {dim}│{rst}  {green}{bold}◆ lean-ctx setup{rst}                         {dim}│{rst}"
    );
    println!("  {dim}│{rst}  {dim}Configuring your development environment{rst} {dim}│{rst}");
    println!("  {dim}╰──────────────────────────────────────────╯{rst}");
    println!();
}
