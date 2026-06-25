use serde::{Deserialize, Serialize};
use std::io::IsTerminal;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Color {
    Hex(String),
}

impl Color {
    #[must_use]
    pub fn rgb(&self) -> (u8, u8, u8) {
        let Color::Hex(hex) = self;
        let hex = hex.trim_start_matches('#');
        if hex.len() < 6 {
            return (255, 255, 255);
        }
        let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(255);
        let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(255);
        let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(255);
        (r, g, b)
    }

    #[must_use]
    pub fn fg(&self) -> String {
        if no_color() {
            return String::new();
        }
        let (r, g, b) = self.rgb();
        format!("\x1b[38;2;{r};{g};{b}m")
    }

    #[must_use]
    pub fn bg(&self) -> String {
        if no_color() {
            return String::new();
        }
        let (r, g, b) = self.rgb();
        format!("\x1b[48;2;{r};{g};{b}m")
    }

    fn lerp_channel(a: u8, b: u8, t: f64) -> u8 {
        (f64::from(a) + (f64::from(b) - f64::from(a)) * t).round() as u8
    }

    #[must_use]
    pub fn lerp(&self, other: &Color, t: f64) -> Color {
        let (r1, g1, b1) = self.rgb();
        let (r2, g2, b2) = other.rgb();
        let r = Self::lerp_channel(r1, r2, t);
        let g = Self::lerp_channel(g1, g2, t);
        let b = Self::lerp_channel(b1, b2, t);
        Color::Hex(format!("#{r:02X}{g:02X}{b:02X}"))
    }
}

impl Default for Color {
    fn default() -> Self {
        Color::Hex("#FFFFFF".to_string())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Theme {
    pub name: String,
    pub primary: Color,
    pub secondary: Color,
    pub accent: Color,
    pub success: Color,
    pub warning: Color,
    #[serde(default = "default_danger")]
    pub danger: Color,
    pub muted: Color,
    pub text: Color,
    #[serde(default = "default_surface")]
    pub surface: Color,
    #[serde(default = "default_background")]
    pub background: Color,
    pub bar_start: Color,
    pub bar_end: Color,
    pub highlight: Color,
    pub border: Color,
}

fn default_danger() -> Color {
    Color::Hex("#EF4444".to_string())
}
fn default_surface() -> Color {
    Color::Hex("#0A0A12".to_string())
}
fn default_background() -> Color {
    Color::Hex("#06060A".to_string())
}

impl Default for Theme {
    fn default() -> Self {
        preset_default()
    }
}

#[must_use]
pub fn no_color() -> bool {
    std::env::var("NO_COLOR").is_ok() || !std::io::stdout().is_terminal()
}

pub const RST: &str = "\x1b[0m";
pub const BOLD: &str = "\x1b[1m";
pub const DIM: &str = "\x1b[2m";

#[must_use]
pub fn rst() -> &'static str {
    if no_color() { "" } else { RST }
}

#[must_use]
pub fn bold() -> &'static str {
    if no_color() { "" } else { BOLD }
}

#[must_use]
pub fn dim() -> &'static str {
    if no_color() { "" } else { DIM }
}

impl Theme {
    #[must_use]
    pub fn pct_color(&self, pct: f64) -> String {
        if no_color() {
            return String::new();
        }
        if pct >= 90.0 {
            self.success.fg()
        } else if pct >= 70.0 {
            self.secondary.fg()
        } else if pct >= 50.0 {
            self.warning.fg()
        } else if pct >= 30.0 {
            self.accent.fg()
        } else {
            self.muted.fg()
        }
    }

    #[must_use]
    pub fn gradient_bar(&self, ratio: f64, width: usize) -> String {
        let blocks = ['▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'];
        let full = (ratio * width as f64).max(0.0);
        let whole = full as usize;
        let frac = ((full - whole as f64) * 8.0) as usize;

        if no_color() {
            let mut s = "█".repeat(whole);
            if whole < width && frac > 0 {
                s.push(blocks[frac.min(7)]);
            }
            if s.is_empty() && ratio > 0.0 {
                s.push('▏');
            }
            return s;
        }

        let mut buf = String::with_capacity(whole * 20 + 30);
        let total_chars = if whole < width && frac > 0 {
            whole + 1
        } else if whole == 0 && ratio > 0.0 {
            1
        } else {
            whole
        };

        for i in 0..whole {
            let t = if total_chars > 1 {
                i as f64 / (total_chars - 1) as f64
            } else {
                0.5
            };
            let c = self.bar_start.lerp(&self.bar_end, t);
            buf.push_str(&c.fg());
            buf.push('█');
        }

        if whole < width && frac > 0 {
            let t = if total_chars > 1 {
                whole as f64 / (total_chars - 1) as f64
            } else {
                1.0
            };
            let c = self.bar_start.lerp(&self.bar_end, t);
            buf.push_str(&c.fg());
            buf.push(blocks[frac.min(7)]);
        } else if whole == 0 && ratio > 0.0 {
            buf.push_str(&self.bar_start.fg());
            buf.push('▏');
        }

        if !buf.is_empty() {
            buf.push_str(RST);
        }
        buf
    }

    #[must_use]
    pub fn gradient_sparkline(&self, values: &[u64]) -> String {
        let ticks = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
        let max = *values.iter().max().unwrap_or(&1) as f64;
        if max == 0.0 {
            return " ".repeat(values.len());
        }

        let nc = no_color();
        let mut buf = String::with_capacity(values.len() * 20);
        let len = values.len();

        for (i, v) in values.iter().enumerate() {
            let idx = ((*v as f64 / max) * 7.0).round() as usize;
            let ch = ticks[idx.min(7)];
            if nc {
                buf.push(ch);
            } else {
                let t = if len > 1 {
                    i as f64 / (len - 1) as f64
                } else {
                    0.5
                };
                let c = self.bar_start.lerp(&self.bar_end, t);
                buf.push_str(&c.fg());
                buf.push(ch);
            }
        }
        if !nc && !buf.is_empty() {
            buf.push_str(RST);
        }
        buf
    }

    #[must_use]
    pub fn badge(&self, _label: &str, value: &str, color: &Color) -> String {
        if no_color() {
            return format!(" {value:<12}");
        }
        format!("{bg}{BOLD} {value} {RST}", bg = color.bg())
    }

    #[must_use]
    pub fn border_line(&self, width: usize) -> String {
        if no_color() {
            return "─".repeat(width);
        }
        let line: String = std::iter::repeat_n('─', width).collect();
        format!("{}{line}{RST}", self.border.fg())
    }

    #[must_use]
    pub fn box_top(&self, width: usize) -> String {
        if no_color() {
            let line: String = std::iter::repeat_n('─', width).collect();
            return format!("╭{line}╮");
        }
        let line: String = std::iter::repeat_n('─', width).collect();
        format!("{}╭{line}╮{RST}", self.border.fg())
    }

    #[must_use]
    pub fn box_bottom(&self, width: usize) -> String {
        if no_color() {
            let line: String = std::iter::repeat_n('─', width).collect();
            return format!("╰{line}╯");
        }
        let line: String = std::iter::repeat_n('─', width).collect();
        format!("{}╰{line}╯{RST}", self.border.fg())
    }

    #[must_use]
    pub fn box_mid(&self, width: usize) -> String {
        if no_color() {
            let line: String = std::iter::repeat_n('─', width).collect();
            return format!("├{line}┤");
        }
        let line: String = std::iter::repeat_n('─', width).collect();
        format!("{}├{line}┤{RST}", self.border.fg())
    }

    #[must_use]
    pub fn box_side(&self) -> String {
        if no_color() {
            return "│".to_string();
        }
        format!("{}│{RST}", self.border.fg())
    }

    #[must_use]
    pub fn header_icon(&self) -> String {
        if no_color() {
            return "◆".to_string();
        }
        format!("{}◆{RST}", self.accent.fg())
    }

    #[must_use]
    pub fn brand_title(&self) -> String {
        if no_color() {
            return "lean-ctx".to_string();
        }
        let p = self.primary.fg();
        let s = self.secondary.fg();
        format!("{p}{BOLD}lean{RST}{s}{BOLD}-ctx{RST}")
    }

    #[must_use]
    pub fn section_title(&self, title: &str) -> String {
        if no_color() {
            return title.to_string();
        }
        format!("{}{BOLD}{title}{RST}", self.text.fg())
    }

    #[must_use]
    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).unwrap_or_default()
    }

    /// Export theme as CSS custom properties for the web dashboard.
    #[must_use]
    pub fn to_css_vars(&self) -> String {
        let Color::Hex(ref primary) = self.primary;
        let Color::Hex(ref secondary) = self.secondary;
        let Color::Hex(ref accent) = self.accent;
        let Color::Hex(ref success) = self.success;
        let Color::Hex(ref warning) = self.warning;
        let Color::Hex(ref danger) = self.danger;
        let Color::Hex(ref muted) = self.muted;
        let Color::Hex(ref text) = self.text;
        let Color::Hex(ref surface) = self.surface;
        let Color::Hex(ref background) = self.background;
        let Color::Hex(ref bar_start) = self.bar_start;
        let Color::Hex(ref bar_end) = self.bar_end;
        let Color::Hex(ref border) = self.border;
        format!(
            ":root {{\n\
             \x20 --lctx-primary: {primary};\n\
             \x20 --lctx-secondary: {secondary};\n\
             \x20 --lctx-accent: {accent};\n\
             \x20 --lctx-success: {success};\n\
             \x20 --lctx-warning: {warning};\n\
             \x20 --lctx-danger: {danger};\n\
             \x20 --lctx-muted: {muted};\n\
             \x20 --lctx-text: {text};\n\
             \x20 --lctx-surface: {surface};\n\
             \x20 --lctx-background: {background};\n\
             \x20 --lctx-bar-start: {bar_start};\n\
             \x20 --lctx-bar-end: {bar_end};\n\
             \x20 --lctx-border: {border};\n\
             }}"
        )
    }

    /// Labeled section box top: `┌─ LABEL ──────────────────┐`
    #[must_use]
    pub fn box_top_labeled(&self, width: usize, label: &str) -> String {
        let max_label = width.saturating_sub(4);
        let label_display = if visual_len(label) > max_label {
            truncate_visual(label, max_label)
        } else {
            label.to_string()
        };
        let label_part = format!("─ {label_display} ");
        let remaining = width.saturating_sub(visual_len(&label_part));
        let fill: String = std::iter::repeat_n('─', remaining).collect();
        if no_color() {
            return format!("┌{label_part}{fill}┐");
        }
        let a = self.accent.fg();
        let b = self.border.fg();
        format!("{b}┌─ {a}{BOLD}{label_display}{RST}{b} {fill}┐{RST}")
    }

    /// Labeled section box bottom: `└──────────────────────────┘`
    #[must_use]
    pub fn box_bottom_square(&self, width: usize) -> String {
        let line: String = std::iter::repeat_n('─', width).collect();
        if no_color() {
            return format!("└{line}┘");
        }
        format!("{}└{line}┘{RST}", self.border.fg())
    }

    /// Square box side: `│`
    #[must_use]
    pub fn box_side_square(&self) -> String {
        if no_color() {
            return "│".to_string();
        }
        format!("{}│{RST}", self.border.fg())
    }

    /// KPI underline using bold unicode lines, colored with the metric's color.
    #[must_use]
    pub fn kpi_underline(&self, width: usize, color: &Color) -> String {
        let line: String = std::iter::repeat_n('━', width).collect();
        if no_color() {
            return line;
        }
        format!("{}{line}{RST}", color.fg())
    }

    /// Export theme as a JS module for the web dashboard.
    #[must_use]
    pub fn to_js_tokens(&self) -> String {
        let Color::Hex(ref primary) = self.primary;
        let Color::Hex(ref secondary) = self.secondary;
        let Color::Hex(ref accent) = self.accent;
        let Color::Hex(ref success) = self.success;
        let Color::Hex(ref warning) = self.warning;
        let Color::Hex(ref danger) = self.danger;
        let Color::Hex(ref muted) = self.muted;
        let Color::Hex(ref text) = self.text;
        let Color::Hex(ref surface) = self.surface;
        let Color::Hex(ref background) = self.background;
        let Color::Hex(ref bar_start) = self.bar_start;
        let Color::Hex(ref bar_end) = self.bar_end;
        let Color::Hex(ref border) = self.border;
        format!(
            "// Auto-generated by lean-ctx — do not edit manually\n\
             export const tokens = {{\n\
             \x20 name: \"{name}\",\n\
             \x20 primary: \"{primary}\",\n\
             \x20 secondary: \"{secondary}\",\n\
             \x20 accent: \"{accent}\",\n\
             \x20 success: \"{success}\",\n\
             \x20 warning: \"{warning}\",\n\
             \x20 danger: \"{danger}\",\n\
             \x20 muted: \"{muted}\",\n\
             \x20 text: \"{text}\",\n\
             \x20 surface: \"{surface}\",\n\
             \x20 background: \"{background}\",\n\
             \x20 barStart: \"{bar_start}\",\n\
             \x20 barEnd: \"{bar_end}\",\n\
             \x20 border: \"{border}\",\n\
             }};\n",
            name = self.name,
        )
    }
}

/// Visual width of a string in terminal columns, ignoring ANSI escape sequences
/// and accounting for wide characters (emoji, CJK = 2 columns).
#[must_use]
pub fn visual_len(s: &str) -> usize {
    use unicode_width::UnicodeWidthChar;
    let mut len = 0usize;
    let mut in_escape = false;
    for ch in s.chars() {
        if in_escape {
            if ch == 'm' {
                in_escape = false;
            }
        } else if ch == '\x1b' {
            in_escape = true;
        } else {
            len += UnicodeWidthChar::width(ch).unwrap_or(0);
        }
    }
    len
}

/// Pad a string to `target` visual width with spaces on the right.
/// If the string exceeds `target`, it is visually truncated.
#[must_use]
pub fn pad_right(s: &str, target: usize) -> String {
    use std::cmp::Ordering;
    let vlen = visual_len(s);
    match vlen.cmp(&target) {
        Ordering::Equal => s.to_string(),
        Ordering::Less => format!("{s}{pad}", pad = " ".repeat(target - vlen)),
        Ordering::Greater => truncate_visual(s, target),
    }
}

/// Truncate a string to at most `max_cols` terminal columns,
/// preserving ANSI escape sequences and respecting wide characters.
#[must_use]
pub fn truncate_visual(s: &str, max_cols: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    let mut out = String::with_capacity(s.len());
    let mut cols = 0usize;
    let mut in_escape = false;
    for ch in s.chars() {
        if in_escape {
            out.push(ch);
            if ch == 'm' {
                in_escape = false;
            }
        } else if ch == '\x1b' {
            in_escape = true;
            out.push(ch);
        } else {
            let w = UnicodeWidthChar::width(ch).unwrap_or(0);
            if cols + w > max_cols {
                break;
            }
            cols += w;
            out.push(ch);
        }
    }
    if cols < max_cols {
        out.push_str(&" ".repeat(max_cols - cols));
    }
    out
}

// ---------------------------------------------------------------------------
// Built-in presets
// ---------------------------------------------------------------------------

fn c(hex: &str) -> Color {
    Color::Hex(hex.to_string())
}

#[must_use]
pub fn preset_default() -> Theme {
    Theme {
        name: "default".into(),
        primary: c("#36D399"),
        secondary: c("#66CCFF"),
        accent: c("#CC66FF"),
        success: c("#36D399"),
        warning: c("#FFCC33"),
        danger: c("#EF4444"),
        muted: c("#888888"),
        text: c("#F5F5F5"),
        surface: c("#0A0A12"),
        background: c("#06060A"),
        bar_start: c("#36D399"),
        bar_end: c("#66CCFF"),
        highlight: c("#FF6633"),
        border: c("#555555"),
    }
}

#[must_use]
pub fn preset_neon() -> Theme {
    Theme {
        name: "neon".into(),
        primary: c("#00FF88"),
        secondary: c("#00FFFF"),
        accent: c("#FF00FF"),
        success: c("#00FF44"),
        warning: c("#FFE100"),
        danger: c("#FF3300"),
        muted: c("#666666"),
        text: c("#FFFFFF"),
        surface: c("#0D0D1A"),
        background: c("#050510"),
        bar_start: c("#FF00FF"),
        bar_end: c("#00FFFF"),
        highlight: c("#FF3300"),
        border: c("#333333"),
    }
}

#[must_use]
pub fn preset_ocean() -> Theme {
    Theme {
        name: "ocean".into(),
        primary: c("#0EA5E9"),
        secondary: c("#38BDF8"),
        accent: c("#06B6D4"),
        success: c("#22D3EE"),
        warning: c("#F59E0B"),
        danger: c("#EF4444"),
        muted: c("#64748B"),
        text: c("#E2E8F0"),
        surface: c("#0C1524"),
        background: c("#060D18"),
        bar_start: c("#0284C7"),
        bar_end: c("#67E8F9"),
        highlight: c("#F97316"),
        border: c("#475569"),
    }
}

#[must_use]
pub fn preset_sunset() -> Theme {
    Theme {
        name: "sunset".into(),
        primary: c("#F97316"),
        secondary: c("#FB923C"),
        accent: c("#EC4899"),
        success: c("#F59E0B"),
        warning: c("#EF4444"),
        danger: c("#DC2626"),
        muted: c("#78716C"),
        text: c("#FEF3C7"),
        surface: c("#1C1410"),
        background: c("#0F0A08"),
        bar_start: c("#F97316"),
        bar_end: c("#EC4899"),
        highlight: c("#A855F7"),
        border: c("#57534E"),
    }
}

#[must_use]
pub fn preset_monochrome() -> Theme {
    Theme {
        name: "monochrome".into(),
        primary: c("#D4D4D4"),
        secondary: c("#A3A3A3"),
        accent: c("#E5E5E5"),
        success: c("#D4D4D4"),
        warning: c("#A3A3A3"),
        danger: c("#737373"),
        muted: c("#737373"),
        text: c("#F5F5F5"),
        surface: c("#141414"),
        background: c("#0A0A0A"),
        bar_start: c("#A3A3A3"),
        bar_end: c("#E5E5E5"),
        highlight: c("#FFFFFF"),
        border: c("#525252"),
    }
}

#[must_use]
pub fn preset_cyberpunk() -> Theme {
    Theme {
        name: "cyberpunk".into(),
        primary: c("#FF2D95"),
        secondary: c("#00F0FF"),
        accent: c("#FFE100"),
        success: c("#00FF66"),
        warning: c("#FF6B00"),
        danger: c("#FF0033"),
        muted: c("#555577"),
        text: c("#EEEEFF"),
        surface: c("#12122A"),
        background: c("#080816"),
        bar_start: c("#FF2D95"),
        bar_end: c("#FFE100"),
        highlight: c("#00F0FF"),
        border: c("#3D3D5C"),
    }
}

pub const PRESET_NAMES: &[&str] = &[
    "default",
    "neon",
    "ocean",
    "sunset",
    "monochrome",
    "cyberpunk",
];

#[must_use]
pub fn from_preset(name: &str) -> Option<Theme> {
    match name {
        "default" => Some(preset_default()),
        "neon" => Some(preset_neon()),
        "ocean" => Some(preset_ocean()),
        "sunset" => Some(preset_sunset()),
        "monochrome" => Some(preset_monochrome()),
        "cyberpunk" => Some(preset_cyberpunk()),
        _ => None,
    }
}

#[must_use]
pub fn theme_file_path() -> Option<PathBuf> {
    crate::core::data_dir::lean_ctx_data_dir()
        .ok()
        .map(|d| d.join("theme.toml"))
}

#[must_use]
pub fn load_theme(config_theme: &str) -> Theme {
    if let Some(path) = theme_file_path()
        && path.exists()
        && let Ok(content) = std::fs::read_to_string(&path)
        && let Ok(theme) = toml::from_str::<Theme>(&content)
    {
        return theme;
    }

    from_preset(config_theme).unwrap_or_default()
}

pub fn save_theme(theme: &Theme) -> Result<(), String> {
    let path = theme_file_path().ok_or("cannot determine home directory")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let content = toml::to_string_pretty(theme).map_err(|e| e.to_string())?;
    std::fs::write(&path, content).map_err(|e| e.to_string())
}

#[must_use]
pub fn animate_countup(final_value: u64, width: usize) -> Vec<String> {
    let frames = 10;
    (0..=frames)
        .map(|f| {
            let t = f64::from(f) / f64::from(frames);
            let eased = t * t * (3.0 - 2.0 * t);
            let v = (final_value as f64 * eased).round() as u64;
            format!("{:>width$}", format_big_animated(v), width = width)
        })
        .collect()
}

/// Count-up for percentage values (0.0 -> `final_pct`, displayed as "68.3%").
#[must_use]
pub fn animate_countup_pct(final_pct: f64, width: usize) -> Vec<String> {
    let frames = 10;
    (0..=frames)
        .map(|f| {
            let t = f64::from(f) / f64::from(frames);
            let eased = t * t * (3.0 - 2.0 * t);
            let v = final_pct * eased;
            format!("{:>width$}", format!("{v:.1}%"), width = width)
        })
        .collect()
}

/// Count-up for USD values (0.00 -> `final_usd`, displayed as "$1,289.35").
#[must_use]
pub fn animate_countup_usd(final_usd: f64, width: usize) -> Vec<String> {
    let frames = 10;
    (0..=frames)
        .map(|f| {
            let t = f64::from(f) / f64::from(frames);
            let eased = t * t * (3.0 - 2.0 * t);
            let v = final_usd * eased;
            let formatted = format!("${v:.2}");
            format!("{formatted:>width$}")
        })
        .collect()
}

/// Writes sections to stdout one by one with a delay between each, using cursor
/// control to create a reveal effect. Skips animation when `NO_COLOR` or non-TTY.
pub fn animate_section_reveal(sections: &[String], delay_ms: u64) {
    use std::io::Write;
    let is_tty = std::io::stdout().is_terminal();
    if no_color() || !is_tty || delay_ms == 0 {
        for s in sections {
            println!("{s}");
        }
        return;
    }
    let mut stdout = std::io::stdout();
    for s in sections {
        let _ = writeln!(stdout, "{s}");
        let _ = stdout.flush();
        std::thread::sleep(std::time::Duration::from_millis(delay_ms));
    }
}

fn format_big_animated(n: u64) -> String {
    if n >= 1_000_000_000_000 {
        format!("{:.2}T", n as f64 / 1_000_000_000_000.0)
    } else if n >= 1_000_000_000 {
        format!("{:.2}B", n as f64 / 1_000_000_000.0)
    } else if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        format!("{n}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_to_rgb() {
        let c = Color::Hex("#FF8800".into());
        assert_eq!(c.rgb(), (255, 136, 0));
    }

    #[test]
    fn lerp_colors() {
        let a = Color::Hex("#000000".into());
        let b = Color::Hex("#FF0000".into());
        let mid = a.lerp(&b, 0.5);
        let (r, g, bl) = mid.rgb();
        assert!((r as i16 - 128).abs() <= 1);
        assert_eq!(g, 0);
        assert_eq!(bl, 0);
    }

    #[test]
    fn gradient_bar_produces_output() {
        let theme = preset_default();
        let bar = theme.gradient_bar(0.5, 20);
        assert!(!bar.is_empty());
    }

    #[test]
    fn gradient_sparkline_produces_output() {
        let theme = preset_default();
        let spark = theme.gradient_sparkline(&[10, 50, 30, 80, 20]);
        assert!(!spark.is_empty());
        assert!(spark.chars().count() >= 5);
    }

    #[test]
    fn all_presets_load() {
        for name in PRESET_NAMES {
            let t = from_preset(name);
            assert!(t.is_some(), "preset {name} should exist");
        }
    }

    #[test]
    fn preset_serializes_to_toml() {
        let t = preset_neon();
        let toml_str = t.to_toml();
        assert!(toml_str.contains("neon"));
        assert!(toml_str.contains("#00FF88"));
    }

    #[test]
    fn border_line_width() {
        crate::test_env::set_var("NO_COLOR", "1");
        let theme = preset_default();
        let line = theme.border_line(10);
        assert_eq!(line.chars().count(), 10);
        crate::test_env::remove_var("NO_COLOR");
    }

    #[test]
    fn box_top_bottom_symmetric() {
        crate::test_env::set_var("NO_COLOR", "1");
        let theme = preset_default();
        let top = theme.box_top(20);
        let bot = theme.box_bottom(20);
        assert_eq!(top.chars().count(), bot.chars().count());
        crate::test_env::remove_var("NO_COLOR");
    }

    #[test]
    fn countup_frames() {
        let frames = animate_countup(1000, 6);
        assert_eq!(frames.len(), 11);
        assert!(frames.last().unwrap().contains("1.0K"));
    }

    #[test]
    fn visual_len_plain() {
        assert_eq!(visual_len("hello"), 5);
        assert_eq!(visual_len(""), 0);
    }

    #[test]
    fn visual_len_with_ansi() {
        assert_eq!(visual_len("\x1b[32mhello\x1b[0m"), 5);
        assert_eq!(visual_len("\x1b[38;2;255;0;0mX\x1b[0m"), 1);
    }

    #[test]
    fn pad_right_works() {
        assert_eq!(pad_right("hi", 5), "hi   ");
        assert_eq!(pad_right("hello", 3), "hel");
        assert_eq!(visual_len(&pad_right("hello", 3)), 3);
        let ansi = "\x1b[32mhi\x1b[0m";
        let padded = pad_right(ansi, 5);
        assert_eq!(visual_len(&padded), 5);
        assert!(padded.starts_with("\x1b[32m"));
    }
}
