use super::rpg::format_compact;
use super::sprite::sprite_lines_for_tick;
use super::types::{BuddyState, Rarity, Species};

/// Inner height (sprite rows) of the framed portrait. The sprite is vertically
/// centered within this many rows so every evolution stage lines up.
const PORTRAIT_H: usize = 8;
/// Visual width of the info column to the right of the portrait.
const RIGHT_W: usize = 44;
/// Max width of full-width caption lines below the card body.
const CAPTION_W: usize = 66;

/// Element foreground colour, gated by `NO_COLOR` so plain output stays clean.
fn elem_ansi(state: &BuddyState) -> &'static str {
    if super::super::theme::no_color() {
        ""
    } else {
        state.species.element_color()
    }
}

/// Rarity foreground colour, gated by `NO_COLOR`.
fn rarity_ansi(state: &BuddyState) -> &'static str {
    if super::super::theme::no_color() {
        ""
    } else {
        state.rarity.color_code()
    }
}

/// The colour the creature itself is drawn in: its element normally, but a
/// shifting cosmic hue once it begins ascending (each prestige tier recolours
/// it). Gated by `NO_COLOR`.
fn creature_color(state: &BuddyState) -> &'static str {
    if super::super::theme::no_color() {
        ""
    } else if state.prestige > 0 {
        super::ascension::color(state.prestige)
    } else {
        state.species.element_color()
    }
}

/// Ascension rank colour for prestige badges/bars, gated by `NO_COLOR`.
fn ascension_ansi(tier: u32) -> &'static str {
    if super::super::theme::no_color() {
        ""
    } else {
        super::ascension::color(tier)
    }
}

#[must_use]
pub fn format_buddy_block(state: &BuddyState, theme: &super::super::theme::Theme) -> String {
    format_buddy_block_at(state, theme, None)
}

/// Collector-card layout for the gain --deep dashboard: a framed pixel-art
/// portrait on the left, nameplate + element type + rarity pips + RPG stat bars
/// on the right, with mood, speech and achievement badges underneath.
pub fn format_buddy_block_at(
    state: &BuddyState,
    theme: &super::super::theme::Theme,
    tick: Option<u64>,
) -> String {
    let r = super::super::theme::rst();
    let dim = super::super::theme::dim();

    let sprite = sprite_lines_for_tick(state, tick);
    let portrait = portrait_box(sprite, creature_color(state));
    let right = build_right_column(state, theme);

    let rows = portrait.len().max(right.len());
    let mut out = Vec::with_capacity(rows + 6);
    out.push(String::new());

    for i in 0..rows {
        let pl = portrait.get(i).map_or("", String::as_str);
        let pl = super::super::theme::pad_right(pl, super::mascot_art::width() + 2);
        let rl = right.get(i).map_or("", String::as_str);
        out.push(format!("  {pl}  {rl}"));
    }

    out.push(String::new());
    let mc = mood_color(theme, &state.mood);
    let mood_line = if state.bugs_prevented > 0 {
        format!(
            "{mc}{} {}{r}{dim}  ·  {} bugs caught{r}",
            state.mood.icon(),
            state.mood.label(),
            state.bugs_prevented,
        )
    } else {
        format!("{mc}{} {}{r}", state.mood.icon(), state.mood.label())
    };
    out.push(format!(
        "  {}",
        super::super::theme::truncate_visual(&mood_line, CAPTION_W)
    ));
    // Speech tied to the mood face with a little tail, like the creature is talking.
    let speech = format!(
        "{dim}\u{2570}\u{2500}{r} {dim}\u{201c}{}\u{201d}{r}",
        state.speech
    );
    out.push(format!(
        "  {}",
        super::super::theme::truncate_visual(&speech, CAPTION_W)
    ));

    append_badges(&mut out, state, theme);

    out.push(String::new());
    out.join("\n")
}

/// Build the framed pixel portrait. Each returned line has visual width
/// `mascot width + 2` (frame on both sides); the block is `PORTRAIT_H + 2` tall.
fn portrait_box(sprite: &[String], element_color: &str) -> Vec<String> {
    let r = super::super::theme::rst();
    let w = super::mascot_art::width();
    let top = format!("{element_color}\u{256d}{}\u{256e}{r}", "\u{2500}".repeat(w));
    let bottom = format!("{element_color}\u{2570}{}\u{256f}{r}", "\u{2500}".repeat(w));

    let h = sprite.len().min(PORTRAIT_H);
    let top_pad = (PORTRAIT_H - h) / 2;

    let mut lines = Vec::with_capacity(PORTRAIT_H + 2);
    lines.push(top);
    for i in 0..PORTRAIT_H {
        let idx = i as isize - top_pad as isize;
        let body = if idx >= 0 && (idx as usize) < sprite.len() {
            sprite[idx as usize].as_str()
        } else {
            ""
        };
        let padded = super::super::theme::pad_right(body, w);
        lines.push(format!(
            "{element_color}\u{2502}{element_color}{padded}{element_color}\u{2502}{r}"
        ));
    }
    lines.push(bottom);
    lines
}

/// The right info column: exactly `PORTRAIT_H + 2` lines, each padded to
/// [`RIGHT_W`] so the portrait and info stay column-aligned.
fn build_right_column(state: &BuddyState, theme: &super::super::theme::Theme) -> Vec<String> {
    let r = super::super::theme::rst();
    let bold = super::super::theme::bold();

    let rarity_color = rarity_ansi(state);

    let name = format!(
        "{}{bold}{}{r}",
        creature_color(state),
        super::super::theme::truncate_visual(&state.name, 26)
    );
    let stars = rarity_pips(&state.rarity);

    let type_line = nameplate_form_line(state, theme);
    let rarity_label = format!("{rarity_color}{}{r}", state.rarity.label());

    let mut lines = Vec::with_capacity(PORTRAIT_H + 2);
    lines.push(right_align(&name, &stars, RIGHT_W));
    lines.push(right_align(&type_line, &rarity_label, RIGHT_W));
    lines.push(String::new());
    lines.push(super::super::theme::pad_right(
        &progression_bar(state, theme, 12),
        RIGHT_W,
    ));
    lines.push(String::new());
    lines.push(super::super::theme::pad_right(
        &metric_value(
            "saved",
            &format!("{} tokens", format_compact(state.tokens_saved)),
        ),
        RIGHT_W,
    ));
    lines.push(super::super::theme::pad_right(
        &metric_pct(theme, "compression", state.compression_pct),
        RIGHT_W,
    ));
    lines.push(super::super::theme::pad_right(
        &metric_pct(theme, "cache", state.cache_hit_rate),
        RIGHT_W,
    ));
    lines.push(super::super::theme::pad_right(
        &metric_value("streak", &format!("{} days", state.streak_days)),
        RIGHT_W,
    ));
    lines
}

/// Second nameplate line: the buddy's element flavour (omitted for the neutral
/// "Null" element so it never reads as a confusing word) plus its current form
/// on the *endless* ladder — the cosmic ascension rank with its tier star once
/// ascending, otherwise the evolution stage. The form is never a dead-end nor a
/// low-stage word at a high level.
fn nameplate_form_line(state: &BuddyState, theme: &super::super::theme::Theme) -> String {
    let r = super::super::theme::rst();
    let bold = super::super::theme::bold();
    let dim = super::super::theme::dim();
    let m = theme.muted.fg();

    let form = if state.prestige > 0 {
        format!(
            "{}\u{2605}{} {bold}{}{r}",
            ascension_ansi(state.prestige),
            state.prestige,
            state.form,
        )
    } else {
        format!("{}{bold}{}{r}", theme.accent.fg(), state.form)
    };

    if matches!(state.species, Species::Egg) {
        form
    } else {
        format!(
            "{}{}{r} {m}{}{r}  {dim}\u{00b7}{r}  {form}",
            elem_ansi(state),
            state.species.element_glyph(),
            state.species.element_name(),
        )
    }
}

/// A labelled key/value metric line for the companion stat sheet. The label sits
/// in a fixed dim column so values line up; the value is emphasised.
fn metric_value(label: &str, value: &str) -> String {
    let r = super::super::theme::rst();
    let dim = super::super::theme::dim();
    let bold = super::super::theme::bold();
    let label_col = super::super::theme::pad_right(&format!("{dim}{label}{r}"), 13);
    format!("{label_col}{bold}{value}{r}")
}

/// A labelled percentage metric with a compact fixed-width bar (lean-ctx efficiency).
fn metric_pct(theme: &super::super::theme::Theme, label: &str, pct: u8) -> String {
    let r = super::super::theme::rst();
    let dim = super::super::theme::dim();
    let label_col = super::super::theme::pad_right(&format!("{dim}{label}{r}"), 13);
    let filled = (pct as usize * 8) / 100;
    let empty = 8 - filled;
    let pc = theme.pct_color(f64::from(pct));
    format!(
        "{label_col}{pc}{}{dim}{}{r} {pc}{pct:>3}%{r}",
        "\u{2588}".repeat(filled),
        "\u{2591}".repeat(empty),
    )
}

/// Render the achievements footer: a progress header plus badges laid out in
/// tidy fixed-width columns. Variation selectors are stripped so emoji keep a
/// stable terminal width and the columns stay aligned.
fn append_badges(out: &mut Vec<String>, state: &BuddyState, theme: &super::super::theme::Theme) {
    if state.achievement_badges.is_empty() {
        return;
    }
    let r = super::super::theme::rst();
    let bold = super::super::theme::bold();
    let dim = super::super::theme::dim();
    let m = theme.muted.fg();
    let a = theme.accent.fg();

    let total = super::achievements::catalog().len();
    let got = state.achievement_badges.len();

    out.push(String::new());
    const BAR_W: usize = 12;
    let filled = (got * BAR_W) / total.max(1);
    out.push(format!(
        "  {dim}achievements{r}  {a}{}{dim}{}{r}  {bold}{got}{r}{dim}/{total}{r}",
        "\u{2588}".repeat(filled),
        "\u{2591}".repeat(BAR_W.saturating_sub(filled)),
    ));

    const COLS: usize = 3;
    const CELL: usize = 21;
    let mut col = 0usize;
    let mut line = String::from("  ");
    for badge in &state.achievement_badges {
        if col == COLS {
            out.push(std::mem::replace(&mut line, String::from("  ")));
            col = 0;
        }
        line.push_str(&badge_cell(badge, CELL, &m, r));
        col += 1;
    }
    if col > 0 {
        out.push(line);
    }
}

/// Format one achievement as a fixed-width grid cell. Each badge has exactly one
/// emoji icon assumed to render double-width; because that assumption is applied
/// uniformly the columns line up regardless of the terminal's emoji metrics.
fn badge_cell(badge: &str, width: usize, color: &str, r: &str) -> String {
    let stripped = strip_vs16(badge);
    let name = stripped.split_once(' ').map_or("", |(_, n)| n);
    let content_w = 2 + 1 + name.chars().count();
    let pad = width.saturating_sub(content_w);
    format!("{color}{stripped}{r}{}", " ".repeat(pad))
}

/// Element/theme colour expressing the buddy's current mood.
fn mood_color(theme: &super::super::theme::Theme, mood: &super::types::Mood) -> String {
    use super::types::Mood;
    match mood {
        Mood::Ecstatic => theme.success.fg(),
        Mood::Happy => theme.secondary.fg(),
        Mood::Content => theme.accent.fg(),
        Mood::Worried => theme.warning.fg(),
        Mood::Sleeping => theme.muted.fg(),
    }
}

/// Strip Unicode variation selectors (U+FE0F) so emoji render at a stable
/// monospace width across terminals, keeping column alignment intact.
fn strip_vs16(s: &str) -> String {
    s.chars().filter(|&c| c != '\u{fe0f}').collect()
}

/// Left text + right text padded so the whole thing is exactly `w` visual cols.
fn right_align(left: &str, right: &str, w: usize) -> String {
    let rv = super::super::theme::visual_len(right);
    let left = if super::super::theme::visual_len(left) + rv + 1 > w {
        super::super::theme::truncate_visual(left, w.saturating_sub(rv + 1))
    } else {
        left.to_string()
    };
    let lv = super::super::theme::visual_len(&left);
    let gap = w.saturating_sub(lv + rv);
    format!("{left}{}{right}", " ".repeat(gap))
}

fn rarity_pips(rarity: &Rarity) -> String {
    let r = super::super::theme::rst();
    let dim = super::super::theme::dim();
    let color = if super::super::theme::no_color() {
        ""
    } else {
        rarity.color_code()
    };
    let filled = match rarity {
        Rarity::Egg => 0,
        Rarity::Common => 1,
        Rarity::Uncommon => 2,
        Rarity::Rare => 3,
        Rarity::Epic => 4,
        Rarity::Legendary => 5,
    };
    format!(
        "{color}{}{r}{dim}{}{r}",
        "\u{25c6}".repeat(filled),
        "\u{25c7}".repeat(5 - filled),
    )
}

/// Progression bar. Before Mythic it tracks evolution toward the next stage;
/// at Mythic it switches to the *endless* ascension ladder — there is always a
/// next cosmic rank, so it never shows a dead-end "MAX".
fn progression_bar(state: &BuddyState, theme: &super::super::theme::Theme, width: usize) -> String {
    let r = super::super::theme::rst();
    let dim = super::super::theme::dim();
    let a = theme.accent.fg();

    if let Some(next) = state.evolution.next() {
        let progress = state.evolution.progress(state.level);
        let bar = theme.gradient_bar(progress, width);
        return format!(
            "{dim}evo{r} {bar} {a}{:.0}%{r} {dim}\u{2192} {}{r}",
            progress * 100.0,
            next.label()
        );
    }

    // Mythic → infinite ascension. Always a next tier.
    let next_tier = state.prestige + 1;
    let progress = super::ascension::progress(state.xp);
    let bar = theme.gradient_bar(progress, width);
    format!(
        "{dim}ascend{r} {bar} {}\u{2605}{}{r} {dim}{:.0}% \u{2192} {}{r}",
        ascension_ansi(next_tier),
        next_tier,
        progress * 100.0,
        super::ascension::title(next_tier),
    )
}

#[must_use]
pub fn format_buddy_full(state: &BuddyState, theme: &super::super::theme::Theme) -> String {
    let rst = super::super::theme::rst();
    let accent = theme.accent.fg();
    let muted = theme.muted.fg();
    let bold = super::super::theme::bold();
    let dim = super::super::theme::dim();
    let rarity_color = rarity_ansi(state);
    let body_color = creature_color(state);

    let mut out = Vec::new();

    let rank_label = if state.prestige > 0 {
        format!(
            "{}\u{2605}{} {}{rst}",
            ascension_ansi(state.prestige),
            state.prestige,
            state.form,
        )
    } else {
        format!("{muted}{} {}{rst}", state.evolution.icon(), state.form)
    };

    out.push(String::new());
    out.push(format!("  {bold}{accent}Pixel Sprite{rst}  {rank_label}"));
    out.push(String::new());

    for line in &state.ascii_art {
        out.push(format!("    {body_color}{line}{rst}"));
    }
    out.push(String::new());

    // Element flavour is omitted for the neutral "Null" element so the line never
    // reads as a confusing "the Null-type".
    let element_phrase = if matches!(state.species, Species::Egg) {
        String::new()
    } else {
        format!("{muted}the {}-type{rst}  ", state.species.element_name())
    };
    out.push(format!(
        "  {bold}{body_color}{}{rst}  {element_phrase}{rarity_color}{}{rst}",
        state.name,
        state.rarity.label(),
    ));
    out.push(format!(
        "  {muted}Mood: {}  |  Streak: {}d  |  Bugs prevented: {}{rst}",
        state.mood.label(),
        state.streak_days,
        state.bugs_prevented,
    ));

    let evo_bar = progression_bar(state, theme, 16);
    out.push(format!("  {evo_bar}"));
    out.push(String::new());

    out.push(format!("  {bold}Efficiency{rst}"));
    out.push(format!(
        "  {}",
        metric_value(
            "saved",
            &format!("{} tokens", format_compact(state.tokens_saved))
        )
    ));
    out.push(format!(
        "  {}",
        metric_pct(theme, "compression", state.compression_pct)
    ));
    out.push(format!(
        "  {}",
        metric_pct(theme, "cache", state.cache_hit_rate)
    ));
    out.push(String::new());

    if !state.achievement_badges.is_empty() {
        let total = super::achievements::catalog().len();
        let got = state.achievement_badges.len();
        out.push(format!(
            "  {bold}Achievements{rst}  {muted}{got}/{total}{rst}"
        ));
        let mut col = 0usize;
        let mut line = String::from("  ");
        for badge in &state.achievement_badges {
            if col == 3 {
                out.push(std::mem::replace(&mut line, String::from("  ")));
                col = 0;
            }
            line.push_str(&badge_cell(badge, 21, &muted, rst));
            col += 1;
        }
        if col > 0 {
            out.push(line);
        }
        out.push(String::new());
    }

    out.push(format!("  {dim}\"{}\"{rst}", state.speech));
    out.push(String::new());

    out.join("\n")
}

pub(super) fn detect_project_root_for_buddy() -> String {
    if let Some(session) = super::super::session::SessionState::load_latest() {
        if let Some(root) = session.project_root.as_deref()
            && !root.trim().is_empty()
        {
            return root.to_string();
        }
        if let Some(cwd) = session.shell_cwd.as_deref()
            && !cwd.trim().is_empty()
        {
            return super::super::protocol::detect_project_root_or_cwd(cwd);
        }
        if let Some(last) = session.files_touched.last()
            && !last.path.trim().is_empty()
            && let Some(parent) = std::path::Path::new(&last.path).parent()
        {
            let p = parent.to_string_lossy().to_string();
            return super::super::protocol::detect_project_root_or_cwd(&p);
        }
    }
    std::env::current_dir()
        .map(|p| super::super::protocol::detect_project_root_or_cwd(&p.to_string_lossy()))
        .unwrap_or_default()
}
