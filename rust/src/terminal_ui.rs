use std::io::{self, IsTerminal, Write};

const LOGO: [&str; 6] = [
    r"  ██╗     ███████╗ █████╗ ███╗   ██╗     ██████╗████████╗██╗  ██╗",
    r"  ██║     ██╔════╝██╔══██╗████╗  ██║    ██╔════╝╚══██╔══╝╚██╗██╔╝",
    r"  ██║     █████╗  ███████║██╔██╗ ██║    ██║        ██║    ╚███╔╝ ",
    r"  ██║     ██╔══╝  ██╔══██║██║╚██╗██║    ██║        ██║    ██╔██╗ ",
    r"  ███████╗███████╗██║  ██║██║ ╚████║    ╚██████╗   ██║   ██╔╝ ██╗",
    r"  ╚══════╝╚══════╝╚═╝  ╚═╝╚═╝  ╚═══╝     ╚═════╝   ╚═╝   ╚═╝  ╚═╝",
];

const TAGLINE: &str = "Context Runtime for AI Agents";

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
    let frames = 28;
    let frame_ms = 45;
    let top_padding = 2;

    let _ = writeln!(stdout);
    let _ = writeln!(stdout);

    for frame in 0..frames {
        if frame > 0 {
            print!("\x1b[{}A", LOGO.len() + 2 + top_padding);
            for _ in 0..top_padding {
                let _ = writeln!(stdout);
            }
        }

        let wave_offset = frame as f64 / frames as f64;

        for (i, line) in LOGO.iter().enumerate() {
            let chars: Vec<char> = line.chars().collect();
            let max_j = chars.len().max(1) as f64;
            let mut buf = String::with_capacity(chars.len() * 20);

            for (j, ch) in chars.iter().enumerate() {
                if *ch == ' ' {
                    buf.push(' ');
                    continue;
                }
                let pos = j as f64 / max_j + i as f64 * 0.15;
                let blend = ((pos + wave_offset * 2.0) * std::f64::consts::PI)
                    .sin()
                    .mul_add(0.5, 0.5);
                let c = t.primary.lerp(&t.secondary, blend);
                buf.push_str(&c.fg());
                buf.push(*ch);
            }
            buf.push_str("\x1b[0m");
            let _ = writeln!(stdout, "{buf}");
        }

        let tag_blend = ((wave_offset * 2.0 + 1.0) * std::f64::consts::PI)
            .sin()
            .mul_add(0.5, 0.5);
        let tag_color = t.muted.lerp(&t.accent, tag_blend * 0.5);
        let _ = writeln!(stdout, "{}             {TAGLINE}\x1b[0m", tag_color.fg());
        let _ = writeln!(stdout);

        let _ = stdout.flush();
        std::thread::sleep(std::time::Duration::from_millis(frame_ms));
    }

    print!("\x1b[{}A", LOGO.len() + 2 + top_padding);
    print_logo_themed_static(t);
}

fn print_logo_themed_static(t: &crate::core::theme::Theme) {
    if crate::core::theme::no_color() {
        print_logo_plain();
        return;
    }
    let mut stdout = io::stdout();

    let _ = writeln!(stdout);
    let _ = writeln!(stdout);

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
    println!();
    println!();
    for line in &LOGO {
        println!("{line}");
    }
    println!("             {TAGLINE}");
    println!();
}

#[allow(clippy::many_single_char_names)] // ANSI formatting: t=theme, r=reset, b=bold, d=dim
pub fn print_command_box() {
    use crate::core::theme;
    let cfg = crate::core::config::Config::load();
    let theme = theme::load_theme(&cfg.theme);
    let dim = theme::dim();
    let bold = theme::bold();
    let rst = theme::rst();
    let cmd = theme.accent.fg();
    let ok = theme.success.fg();
    let m = theme.muted.fg();

    println!("  {dim}┌─────────────────────────────────────────────────────────┐{rst}");
    println!(
        "  {dim}│{rst}  {cmd}{bold}lean-ctx gain{rst}        {m}Token savings dashboard{rst}         {dim}│{rst}"
    );
    println!(
        "  {dim}│{rst}  {cmd}{bold}lean-ctx cockpit{rst}     {m}The Cockpit (browser){rst}          {dim}│{rst}"
    );
    println!(
        "  {dim}│{rst}  {cmd}{bold}lean-ctx heatmap{rst}     {m}Project context heat map{rst}        {dim}│{rst}"
    );
    println!(
        "  {dim}│{rst}  {cmd}{bold}lean-ctx benchmark{rst}   {m}Test compression quality{rst}        {dim}│{rst}"
    );
    println!(
        "  {dim}│{rst}  {cmd}{bold}lean-ctx config{rst}      {m}Edit settings{rst}                   {dim}│{rst}"
    );
    println!(
        "  {dim}│{rst}  {cmd}{bold}lean-ctx doctor{rst}      {m}Verify installation{rst}             {dim}│{rst}"
    );
    println!(
        "  {dim}│{rst}  {cmd}{bold}lean-ctx update{rst}      {m}Self-update to latest{rst}           {dim}│{rst}"
    );
    println!(
        "  {dim}│{rst}  {cmd}{bold}LEAN_CTX_DISABLED=1{rst}  {m}Disable compression{rst}             {dim}│{rst}"
    );
    println!(
        "  {dim}│{rst}  {cmd}{bold}lean-ctx report-issue{rst} {m}Report a bug (auto-diagnostics){rst} {dim}│{rst}"
    );
    println!(
        "  {dim}│{rst}  {cmd}{bold}lean-ctx contribute{rst}  {m}Share anonymized compression stats{rst}{dim}│{rst}"
    );
    println!(
        "  {dim}│{rst}  {cmd}{bold}lean-ctx uninstall{rst}   {m}Clean removal{rst}                   {dim}│{rst}"
    );
    println!("  {dim}└─────────────────────────────────────────────────────────┘{rst}");
    println!("  {ok}Ready!{rst} Your next AI command will be automatically optimized.");
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

// ── Unified progress indicator (index builds, long CLI work) ──────────

/// Interior width of the progress bar (characters between `[` and `]`).
const PROGRESS_BAR_WIDTH: usize = 20;

/// Shared CLI progress renderer for measurable and indeterminate work.
///
/// * **Determinate** (`total > 0`): `label [=======>        ]  42%`
/// * **Indeterminate** (`total == 0`): bouncing arrow L↔R inside the bar
///
/// Renders on stderr so stdout stays free for machine-readable output.
/// Non-TTY: silent ticks; only \[`finish`\] prints a line.
#[derive(Debug)]
pub struct ProgressIndicator {
    label: String,
    done: u64,
    /// `0` → indeterminate (bouncing arrow).
    total: u64,
    frame: usize,
    tty: bool,
    finished: bool,
}

impl ProgressIndicator {
    /// Start a progress indicator with `label` (e.g. `"BM25"`, `"semantic"`).
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            done: 0,
            total: 0,
            frame: 0,
            tty: io::stderr().is_terminal(),
            finished: false,
        }
    }

    /// Change the stage label without finishing (shared indicator across phases).
    pub fn set_label(&mut self, label: impl Into<String>) {
        self.label = label.into();
    }

    /// Set determinate progress. `total == 0` switches to indeterminate.
    pub fn set(&mut self, done: u64, total: u64) {
        self.done = done;
        self.total = total;
    }

    /// Switch to indeterminate (infinite bouncing arrow).
    pub fn indeterminate(&mut self) {
        self.done = 0;
        self.total = 0;
    }

    /// Advance animation / redraw. Call ~10–20×/s while work runs.
    pub fn tick(&mut self) {
        if self.finished {
            return;
        }
        self.frame = self.frame.wrapping_add(1);
        if !self.tty {
            return;
        }
        let line = self.render_line();
        eprint!("\r{line}\x1b[K");
        let _ = io::stderr().flush();
    }

    /// Clear the bar and print a final success/status line.
    pub fn finish(&mut self, msg: &str) {
        if self.finished {
            return;
        }
        self.finished = true;
        if self.tty {
            eprint!("\r\x1b[K");
            let _ = io::stderr().flush();
        }
        eprintln!("{msg}");
    }

    /// Render the current bar line (no `\r`). Public for tests.
    pub fn render_line(&self) -> String {
        if self.total > 0 {
            Self::render_determinate(&self.label, self.done, self.total)
        } else {
            Self::render_indeterminate(&self.label, self.frame)
        }
    }

    fn render_determinate(label: &str, done: u64, total: u64) -> String {
        let total = total.max(1);
        let pct = ((done as f64 / total as f64) * 100.0).min(100.0).round() as u32;
        let filled = (((done as f64 / total as f64) * PROGRESS_BAR_WIDTH as f64).round() as usize)
            .min(PROGRESS_BAR_WIDTH);

        let mut bar = String::with_capacity(PROGRESS_BAR_WIDTH);
        for i in 0..PROGRESS_BAR_WIDTH {
            if filled == 0 {
                bar.push(' ');
            } else if i + 1 < filled {
                bar.push('=');
            } else if i + 1 == filled {
                bar.push('→');
            } else {
                bar.push(' ');
            }
        }
        format!("  {label} [{bar}] {pct:>3}%")
    }

    fn render_indeterminate(label: &str, frame: usize) -> String {
        let max = PROGRESS_BAR_WIDTH.saturating_sub(1).max(1);
        let cycle = max * 2;
        let t = frame % cycle;
        let (pos, arrow) = if t <= max {
            (t, '→')
        } else {
            (cycle - t, '←')
        };
        let mut bar: Vec<char> = vec![' '; PROGRESS_BAR_WIDTH];
        let idx = pos.min(PROGRESS_BAR_WIDTH.saturating_sub(1));
        bar[idx] = arrow;
        let bar: String = bar.into_iter().collect();
        format!("  {label} [{bar}]")
    }
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

#[cfg(test)]
mod progress_tests {
    use super::*;

    #[test]
    fn determinate_includes_arrow_and_percent() {
        let line = ProgressIndicator::render_determinate("BM25", 50, 100);
        assert!(line.contains("BM25"), "{line}");
        assert!(line.contains('→'), "{line}");
        assert!(line.contains("50%"), "{line}");
        assert!(line.contains('['), "{line}");
    }

    #[test]
    fn determinate_full_is_100() {
        let line = ProgressIndicator::render_determinate("semantic", 10, 10);
        assert!(line.contains("100%"), "{line}");
        assert!(line.contains('→'), "{line}");
    }

    #[test]
    fn indeterminate_bounces_left_and_right() {
        let right = ProgressIndicator::render_indeterminate("graph", 0);
        assert!(right.contains('→'), "{right}");
        assert!(!right.contains('%'), "{right}");

        let max = PROGRESS_BAR_WIDTH.saturating_sub(1).max(1);
        let left = ProgressIndicator::render_indeterminate("graph", max + 1);
        assert!(left.contains('←'), "{left}");
    }

    #[test]
    fn set_and_indeterminate_toggle() {
        let mut p = ProgressIndicator::new("BM25");
        p.tty = false;
        p.set(2, 8);
        assert!(p.render_line().contains("25%"));
        p.indeterminate();
        assert!(!p.render_line().contains('%'));
    }
}
