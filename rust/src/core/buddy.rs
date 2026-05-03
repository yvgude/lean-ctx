use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Species (derived from dominant toolchain in commands)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Species {
    Egg,
    Crab,
    Snake,
    Owl,
    Gopher,
    Whale,
    Fox,
    Dragon,
}

impl Species {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Egg => "Egg",
            Self::Crab => "Crab",
            Self::Snake => "Snake",
            Self::Owl => "Owl",
            Self::Gopher => "Gopher",
            Self::Whale => "Whale",
            Self::Fox => "Fox",
            Self::Dragon => "Dragon",
        }
    }

    pub fn from_commands(commands: &HashMap<String, super::stats::CommandStats>) -> Self {
        let mut scores: HashMap<&str, u64> = HashMap::new();

        for (cmd, stats) in commands {
            let lang = classify_command(cmd);
            if !lang.is_empty() {
                *scores.entry(lang).or_default() += stats.count;
            }
        }

        if scores.is_empty() {
            return Self::Egg;
        }

        let total: u64 = scores.values().sum();
        let (top_lang, top_count) = scores
            .iter()
            .max_by_key(|(_, c)| **c)
            .map_or(("", 0), |(l, c)| (*l, *c));

        let dominance = top_count as f64 / total as f64;

        if dominance < 0.4 {
            return Self::Dragon;
        }

        match top_lang {
            "rust" => Self::Crab,
            "python" => Self::Snake,
            "js" => Self::Owl,
            "go" => Self::Gopher,
            "docker" => Self::Whale,
            "git" => Self::Fox,
            _ => Self::Dragon,
        }
    }
}

fn classify_command(cmd: &str) -> &'static str {
    let lower = cmd.to_lowercase();
    if lower.starts_with("cargo") || lower.starts_with("rustc") {
        "rust"
    } else if lower.starts_with("python")
        || lower.starts_with("pip")
        || lower.starts_with("uv ")
        || lower.starts_with("pytest")
        || lower.starts_with("ruff")
    {
        "python"
    } else if lower.starts_with("npm")
        || lower.starts_with("pnpm")
        || lower.starts_with("yarn")
        || lower.starts_with("tsc")
        || lower.starts_with("jest")
        || lower.starts_with("vitest")
        || lower.starts_with("node")
        || lower.starts_with("bun")
    {
        "js"
    } else if lower.starts_with("go ") {
        "go"
    } else if lower.starts_with("docker") || lower.starts_with("kubectl") {
        "docker"
    } else if lower.starts_with("git ") {
        "git"
    } else {
        ""
    }
}

// ---------------------------------------------------------------------------
// Rarity
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, PartialOrd)]
pub enum Rarity {
    Egg,
    Common,
    Uncommon,
    Rare,
    Epic,
    Legendary,
}

impl Rarity {
    pub fn from_tokens_saved(saved: u64) -> Self {
        match saved {
            0..=9_999 => Self::Egg,
            10_000..=99_999 => Self::Common,
            100_000..=999_999 => Self::Uncommon,
            1_000_000..=9_999_999 => Self::Rare,
            10_000_000..=99_999_999 => Self::Epic,
            _ => Self::Legendary,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Egg => "Egg",
            Self::Common => "Common",
            Self::Uncommon => "Uncommon",
            Self::Rare => "Rare",
            Self::Epic => "Epic",
            Self::Legendary => "Legendary",
        }
    }

    pub fn color_code(&self) -> &'static str {
        match self {
            Self::Egg | Self::Common => "\x1b[37m",
            Self::Uncommon => "\x1b[32m",
            Self::Rare => "\x1b[34m",
            Self::Epic => "\x1b[35m",
            Self::Legendary => "\x1b[33m",
        }
    }
}

// ---------------------------------------------------------------------------
// Mood
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Mood {
    Ecstatic,
    Happy,
    Content,
    Worried,
    Sleeping,
}

impl Mood {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Ecstatic => "Ecstatic",
            Self::Happy => "Happy",
            Self::Content => "Content",
            Self::Worried => "Worried",
            Self::Sleeping => "Sleeping",
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            Self::Ecstatic => "*_*",
            Self::Happy => "o_o",
            Self::Content => "-_-",
            Self::Worried => ">_<",
            Self::Sleeping => "u_u",
        }
    }
}

// ---------------------------------------------------------------------------
// RPG Stats
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuddyStats {
    pub compression: u8,
    pub vigilance: u8,
    pub endurance: u8,
    pub wisdom: u8,
    pub experience: u8,
}

// ---------------------------------------------------------------------------
// Procedural creature traits (8 axes, 69M+ combinations)
// 12 x 10 x 10 x 12 x 10 x 10 x 8 x 6 = 69,120,000
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatureTraits {
    pub head: u8,
    pub eyes: u8,
    pub mouth: u8,
    pub ears: u8,
    pub body: u8,
    pub legs: u8,
    pub tail: u8,
    pub markings: u8,
}

impl CreatureTraits {
    pub fn from_seed(seed: u64) -> Self {
        Self {
            head: (seed % 12) as u8,
            eyes: ((seed / 12) % 10) as u8,
            mouth: ((seed / 120) % 10) as u8,
            ears: ((seed / 1_200) % 12) as u8,
            body: ((seed / 14_400) % 10) as u8,
            legs: ((seed / 144_000) % 10) as u8,
            tail: ((seed / 1_440_000) % 8) as u8,
            markings: ((seed / 11_520_000) % 6) as u8,
        }
    }
}

fn user_seed() -> u64 {
    dirs::home_dir().map_or(42, |p| {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        p.hash(&mut h);
        h.finish()
    })
}

// ---------------------------------------------------------------------------
// BuddyState (full computed state)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuddyState {
    pub name: String,
    pub species: Species,
    pub rarity: Rarity,
    pub level: u32,
    pub xp: u64,
    pub xp_next_level: u64,
    pub mood: Mood,
    pub stats: BuddyStats,
    pub speech: String,
    pub tokens_saved: u64,
    pub bugs_prevented: u64,
    pub streak_days: u32,
    pub ascii_art: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ascii_frames: Vec<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anim_ms: Option<u32>,
    pub traits: CreatureTraits,
}

impl BuddyState {
    pub fn compute() -> Self {
        let store = super::stats::load();
        let tokens_saved = store
            .total_input_tokens
            .saturating_sub(store.total_output_tokens);

        let project_root = detect_project_root_for_buddy();
        let gotcha_store = if project_root.is_empty() {
            super::gotcha_tracker::GotchaStore::new("none")
        } else {
            super::gotcha_tracker::GotchaStore::load(&project_root)
        };

        let bugs_prevented = gotcha_store.stats.total_prevented;
        let errors_detected = gotcha_store.stats.total_errors_detected;

        let species = Species::from_commands(&store.commands);
        let rarity = Rarity::from_tokens_saved(tokens_saved);

        let xp = tokens_saved / 1000 + store.total_commands * 5 + bugs_prevented * 100;
        let level = ((xp as f64 / 50.0).sqrt().floor() as u32).min(99);
        let xp_next_level = ((level + 1) as u64) * ((level + 1) as u64) * 50;

        let streak_days = compute_streak(&store.daily);
        let compression_rate = if store.total_input_tokens > 0 {
            (tokens_saved as f64 / store.total_input_tokens as f64 * 100.0) as u8
        } else {
            0
        };

        let mood = compute_mood(
            compression_rate,
            errors_detected,
            bugs_prevented,
            streak_days,
            &store,
        );

        let rpg_stats = compute_rpg_stats(
            compression_rate,
            bugs_prevented,
            errors_detected,
            streak_days,
            store.commands.len(),
            store.total_commands,
        );

        let seed = user_seed();
        let traits = CreatureTraits::from_seed(seed);
        let name = generate_name(seed);
        let sprite = render_sprite_pack(&traits, &mood, level);
        let ascii_art = sprite.base.clone();
        let speech = generate_speech(&mood, tokens_saved, bugs_prevented, streak_days);

        Self {
            name,
            species,
            rarity,
            level,
            xp,
            xp_next_level,
            mood,
            stats: rpg_stats,
            speech,
            tokens_saved,
            bugs_prevented,
            streak_days,
            ascii_art,
            ascii_frames: sprite.frames,
            anim_ms: sprite.anim_ms,
            traits,
        }
    }
}

fn detect_project_root_for_buddy() -> String {
    if let Some(session) = super::session::SessionState::load_latest() {
        if let Some(root) = session.project_root.as_deref() {
            if !root.trim().is_empty() {
                return root.to_string();
            }
        }
        if let Some(cwd) = session.shell_cwd.as_deref() {
            if !cwd.trim().is_empty() {
                return super::protocol::detect_project_root_or_cwd(cwd);
            }
        }
        if let Some(last) = session.files_touched.last() {
            if !last.path.trim().is_empty() {
                if let Some(parent) = std::path::Path::new(&last.path).parent() {
                    let p = parent.to_string_lossy().to_string();
                    return super::protocol::detect_project_root_or_cwd(&p);
                }
            }
        }
    }
    std::env::current_dir()
        .map(|p| super::protocol::detect_project_root_or_cwd(&p.to_string_lossy()))
        .unwrap_or_default()
}

struct SpritePack {
    base: Vec<String>,
    frames: Vec<Vec<String>>,
    anim_ms: Option<u32>,
}

fn sprite_tier(level: u32) -> u8 {
    if level >= 75 {
        4
    } else if level >= 50 {
        3
    } else if level >= 25 {
        2
    } else {
        u8::from(level >= 10)
    }
}

fn tier_anim_ms(tier: u8) -> Option<u32> {
    match tier {
        0 => None,
        1 => Some(950),
        2 => Some(700),
        3 => Some(520),
        _ => Some(380),
    }
}

fn render_sprite_pack(traits: &CreatureTraits, mood: &Mood, level: u32) -> SpritePack {
    let base = render_sprite(traits, mood);
    let tier = sprite_tier(level);
    if tier == 0 {
        return SpritePack {
            base,
            frames: Vec::new(),
            anim_ms: None,
        };
    }

    let mut frames = Vec::new();
    frames.push(base.clone());

    // Frame 1: blink
    let blink = match mood {
        Mood::Sleeping => ("u", "u"),
        _ => (".", "."),
    };
    frames.push(render_sprite_with_eyes(traits, mood, blink.0, blink.1));

    // Frame 2+: level-based effects
    if tier >= 2 {
        let mut s = base.clone();
        if let Some(l0) = s.get_mut(0) {
            *l0 = sparkle_edges(l0, '*', '+');
        }
        frames.push(s);
    }
    if tier >= 3 {
        let mut s = base.clone();
        for line in &mut s {
            *line = shift(line, 1);
        }
        frames.push(s);
    }
    if tier >= 4 {
        let mut s = base.clone();
        for (i, line) in s.iter_mut().enumerate() {
            let (l, r) = if i % 2 == 0 { ('+', '+') } else { ('*', '*') };
            *line = edge_aura(line, l, r);
        }
        frames.push(s);
    }

    SpritePack {
        base,
        frames,
        anim_ms: tier_anim_ms(tier),
    }
}

fn render_sprite_with_eyes(
    traits: &CreatureTraits,
    _mood: &Mood,
    el: &str,
    er: &str,
) -> Vec<String> {
    let ears = ear_part(traits.ears);
    let head_top = head_top_part(traits.head);
    let face = face_line(traits.head, traits.eyes, el, er);
    let mouth = mouth_line(traits.head, traits.mouth);
    let neck = neck_part(traits.head);
    let body = body_part(traits.body, traits.markings);
    let feet = leg_part(traits.legs, traits.tail);

    vec![
        pad(&ears),
        pad(&head_top),
        pad(&face),
        pad(&mouth),
        pad(&neck),
        pad(&body),
        pad(&feet),
    ]
}

fn sparkle_edges(line: &str, left: char, right: char) -> String {
    let s = pad(line);
    let mut chars: Vec<char> = s.chars().collect();
    if chars.len() >= 2 {
        chars[0] = left;
        let last = chars.len() - 1;
        chars[last] = right;
    }
    chars.into_iter().collect()
}

fn edge_aura(line: &str, left: char, right: char) -> String {
    let s = pad(line);
    let mut chars: Vec<char> = s.chars().collect();
    if chars.len() >= 2 {
        chars[0] = left;
        let last = chars.len() - 1;
        chars[last] = right;
    }
    chars.into_iter().collect()
}

fn shift(line: &str, offset: i32) -> String {
    if offset == 0 {
        return pad(line);
    }
    let s = pad(line);
    let mut chars: Vec<char> = s.chars().collect();
    if chars.is_empty() {
        return s;
    }
    if offset > 0 {
        for _ in 0..offset {
            chars.insert(0, ' ');
            chars.pop();
        }
    } else {
        for _ in 0..(-offset) {
            chars.remove(0);
            chars.push(' ');
        }
    }
    chars.into_iter().collect()
}

fn sprite_lines_for_tick(state: &BuddyState, tick: Option<u64>) -> &[String] {
    if let Some(t) = tick {
        if !state.ascii_frames.is_empty() {
            let idx = (t as usize) % state.ascii_frames.len();
            return &state.ascii_frames[idx];
        }
    }
    &state.ascii_art
}

// ---------------------------------------------------------------------------
// Mood computation
// ---------------------------------------------------------------------------

fn compute_mood(
    compression: u8,
    errors: u64,
    prevented: u64,
    streak: u32,
    store: &super::stats::StatsStore,
) -> Mood {
    let hours_since_last = store
        .last_use
        .as_ref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map_or(999, |dt| {
            (chrono::Utc::now() - dt.with_timezone(&chrono::Utc)).num_hours()
        });

    if hours_since_last > 48 {
        return Mood::Sleeping;
    }

    let recent_errors = store
        .daily
        .iter()
        .rev()
        .take(1)
        .any(|d| d.input_tokens > 0 && d.output_tokens > d.input_tokens);

    if compression > 60 && errors == 0 && streak >= 7 {
        Mood::Ecstatic
    } else if compression > 40 || prevented > 0 {
        Mood::Happy
    } else if recent_errors || (errors > 5 && prevented == 0) {
        Mood::Worried
    } else {
        Mood::Content
    }
}

// ---------------------------------------------------------------------------
// RPG stats
// ---------------------------------------------------------------------------

fn compute_rpg_stats(
    compression: u8,
    prevented: u64,
    errors: u64,
    streak: u32,
    unique_cmds: usize,
    total_cmds: u64,
) -> BuddyStats {
    let compression_stat = compression.min(100);

    let vigilance = if errors > 0 {
        ((prevented as f64 / errors as f64) * 80.0).min(100.0) as u8
    } else if prevented > 0 {
        100
    } else {
        20
    };

    let endurance = (streak * 5).min(100) as u8;
    let wisdom = (unique_cmds as u8).min(100);
    let experience = if total_cmds > 0 {
        ((total_cmds as f64).log10() * 25.0).min(100.0) as u8
    } else {
        0
    };

    BuddyStats {
        compression: compression_stat,
        vigilance,
        endurance,
        wisdom,
        experience,
    }
}

// ---------------------------------------------------------------------------
// Streak
// ---------------------------------------------------------------------------

fn compute_streak(daily: &[super::stats::DayStats]) -> u32 {
    if daily.is_empty() {
        return 0;
    }

    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let mut streak = 0u32;
    let mut expected = today.clone();

    for day in daily.iter().rev() {
        if day.date == expected && day.commands > 0 {
            streak += 1;
            if let Ok(dt) = chrono::NaiveDate::parse_from_str(&expected, "%Y-%m-%d") {
                expected = (dt - chrono::Duration::days(1))
                    .format("%Y-%m-%d")
                    .to_string();
            } else {
                break;
            }
        } else if day.date < expected {
            break;
        }
    }
    streak
}

// ---------------------------------------------------------------------------
// Name generator -- Adjective + Noun (deterministic, ~900 combos)
// ---------------------------------------------------------------------------

fn generate_name(seed: u64) -> String {
    const ADJ: &[&str] = &[
        "Swift", "Quiet", "Bright", "Bold", "Clever", "Brave", "Lucky", "Tiny", "Cosmic", "Fuzzy",
        "Nimble", "Jolly", "Mighty", "Gentle", "Witty", "Keen", "Sly", "Calm", "Wild", "Vivid",
        "Dusk", "Dawn", "Neon", "Frost", "Solar", "Lunar", "Pixel", "Turbo", "Nano", "Mega",
    ];
    const NOUN: &[&str] = &[
        "Ember", "Reef", "Spark", "Byte", "Flux", "Echo", "Drift", "Glitch", "Pulse", "Shade",
        "Orbit", "Fern", "Rust", "Zinc", "Flint", "Quartz", "Maple", "Cedar", "Opal", "Moss",
        "Ridge", "Cove", "Peak", "Dune", "Vale", "Brook", "Cliff", "Storm", "Blaze", "Mist",
    ];

    let adj_idx = (seed >> 8) as usize % ADJ.len();
    let noun_idx = (seed >> 16) as usize % NOUN.len();
    format!("{} {}", ADJ[adj_idx], NOUN[noun_idx])
}

// ---------------------------------------------------------------------------
// Speech bubble
// ---------------------------------------------------------------------------

fn generate_speech(mood: &Mood, tokens_saved: u64, bugs_prevented: u64, streak: u32) -> String {
    match mood {
        Mood::Ecstatic => {
            if bugs_prevented > 0 {
                format!("{bugs_prevented} bugs prevented! We're unstoppable!")
            } else {
                format!("{} tokens saved! On fire!", format_compact(tokens_saved))
            }
        }
        Mood::Happy => {
            if streak >= 3 {
                format!("{streak}-day streak! Keep going!")
            } else if bugs_prevented > 0 {
                format!("Caught {bugs_prevented} bugs before they happened!")
            } else {
                format!("{} tokens saved so far!", format_compact(tokens_saved))
            }
        }
        Mood::Content => "Watching your code... all good.".to_string(),
        Mood::Worried => "I see some errors. Let's fix them!".to_string(),
        Mood::Sleeping => "Zzz... wake me with some code!".to_string(),
    }
}

fn format_compact(n: u64) -> String {
    if n >= 1_000_000_000 {
        format!("{:.1}B", n as f64 / 1_000_000_000.0)
    } else if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        format!("{n}")
    }
}

// ---------------------------------------------------------------------------
// Procedural sprite renderer (7 lines, 20 chars wide, center-aligned)
// 12 heads x 10 eyes x 10 mouths x 12 ears x 10 bodies x 10 legs x 8 tails x 6 markings
// = 69,120,000 unique creatures
// ---------------------------------------------------------------------------

const W: usize = 20;

fn pad(s: &str) -> String {
    let len = s.chars().count();
    if len >= W {
        s.chars().take(W).collect()
    } else {
        let left = (W - len) / 2;
        let right = W - len - left;
        format!("{}{}{}", " ".repeat(left), s, " ".repeat(right))
    }
}

pub fn render_sprite(traits: &CreatureTraits, mood: &Mood) -> Vec<String> {
    let (el, er) = mood_eyes(mood);
    let ears = ear_part(traits.ears);
    let head_top = head_top_part(traits.head);
    let face = face_line(traits.head, traits.eyes, el, er);
    let mouth = mouth_line(traits.head, traits.mouth);
    let neck = neck_part(traits.head);
    let body = body_part(traits.body, traits.markings);
    let feet = leg_part(traits.legs, traits.tail);

    vec![
        pad(&ears),
        pad(&head_top),
        pad(&face),
        pad(&mouth),
        pad(&neck),
        pad(&body),
        pad(&feet),
    ]
}

fn mood_eyes(mood: &Mood) -> (&'static str, &'static str) {
    match mood {
        Mood::Ecstatic => ("*", "*"),
        Mood::Happy => ("o", "o"),
        Mood::Content => ("-", "-"),
        Mood::Worried => (">", "<"),
        Mood::Sleeping => ("u", "u"),
    }
}

fn ear_part(idx: u8) -> String {
    match idx % 12 {
        0 => r"  /\    /\".into(),
        1 => r" /  \  /  \".into(),
        2 => r"  ()    ()".into(),
        3 => r"  ||    ||".into(),
        4 => r" ~'      '~".into(),
        5 => r"  >>    <<".into(),
        6 => r"  **    **".into(),
        7 => r" .'      '.".into(),
        8 => r"  ~~    ~~".into(),
        9 => r"  ^^    ^^".into(),
        10 => r"  {}    {}".into(),
        _ => r"  <>    <>".into(),
    }
}

fn head_top_part(idx: u8) -> String {
    match idx % 12 {
        0 => " .--------. ".into(),
        1 => " +--------+ ".into(),
        2 => " /--------\\ ".into(),
        3 => " .========. ".into(),
        4 => " (--------) ".into(),
        5 => " .~~~~~~~~. ".into(),
        6 => " /~~~~~~~~\\ ".into(),
        7 => " {--------} ".into(),
        8 => " <--------> ".into(),
        9 => " .'^----^'. ".into(),
        10 => " /********\\ ".into(),
        _ => " (________) ".into(),
    }
}

fn head_bracket(head: u8) -> (char, char) {
    match head % 12 {
        0 | 1 | 3 | 5 => ('|', '|'),
        2 | 6 | 10 => ('/', '\\'),
        7 => ('{', '}'),
        8 => ('<', '>'),
        _ => ('(', ')'),
    }
}

fn face_line(head: u8, eye_idx: u8, el: &str, er: &str) -> String {
    let (bl, br) = head_bracket(head);
    let deco = match eye_idx % 10 {
        1 => ("'", "'"),
        2 => (".", "."),
        3 => ("~", "~"),
        4 => ("*", "*"),
        5 => ("`", "`"),
        6 => ("^", "^"),
        7 => (",", ","),
        8 => (":", ":"),
        _ => (" ", " "),
    };
    format!(" {bl}  {}{el}  {er}{}  {br} ", deco.0, deco.1)
}

fn mouth_line(head: u8, mouth: u8) -> String {
    let (bl, br) = head_bracket(head);
    let m = match mouth % 10 {
        0 => " \\_/  ",
        1 => "  w   ",
        2 => "  ^   ",
        3 => "  ~   ",
        4 => " ===  ",
        5 => "  o   ",
        6 => "  3   ",
        7 => "  v   ",
        8 => " ---  ",
        _ => "  U   ",
    };
    format!(" {bl}  {m}  {br} ")
}

fn neck_part(head: u8) -> String {
    match head % 12 {
        0 => " '--------' ".into(),
        1 => " +--------+ ".into(),
        2 => " \\--------/ ".into(),
        3 => " '========' ".into(),
        4 => " (--------) ".into(),
        5 => " '~~~~~~~~' ".into(),
        6 => " \\~~~~~~~~/ ".into(),
        7 => " {--------} ".into(),
        8 => " <--------> ".into(),
        9 => " '.^----^.' ".into(),
        10 => " \\********/ ".into(),
        _ => " (__________) ".into(),
    }
}

fn body_part(body: u8, markings: u8) -> String {
    let fill = match markings % 6 {
        0 => "      ",
        1 => " |||| ",
        2 => " .... ",
        3 => " >><< ",
        4 => " ~~~~ ",
        _ => " :::: ",
    };
    match body % 10 {
        0 | 8 => format!("  /{fill}\\  "),
        1 | 7 => format!("  |{fill}|  "),
        2 => format!("  ({fill})  "),
        3 => format!("  [{fill}]  "),
        4 => format!("  ~{fill}~  "),
        5 => format!("  <{fill}>  "),
        6 => format!("  {{{fill}}}  "),
        _ => format!("  _{fill}_  "),
    }
}

fn leg_part(legs: u8, tail: u8) -> String {
    let t = match tail % 8 {
        0 => ' ',
        1 => '~',
        2 => '>',
        3 => ')',
        4 => '^',
        5 => '*',
        6 => '=',
        _ => '/',
    };
    let base = match legs % 10 {
        0 => " /|      |\\",
        1 => " ~~      ~~",
        2 => "_/|      |\\_",
        3 => " ||      ||",
        4 => " /\\      /\\",
        5 => " <>      <>",
        6 => " ()      ()",
        7 => " }{      }{",
        8 => " //      \\\\",
        _ => " \\/      \\/",
    };
    if t == ' ' {
        pad(base)
    } else {
        pad(&format!("{base} {t}"))
    }
}

// ---------------------------------------------------------------------------
// Terminal format
// ---------------------------------------------------------------------------

pub fn format_buddy_block(state: &BuddyState, theme: &super::theme::Theme) -> String {
    format_buddy_block_at(state, theme, None)
}

pub fn format_buddy_block_at(
    state: &BuddyState,
    theme: &super::theme::Theme,
    tick: Option<u64>,
) -> String {
    let r = super::theme::rst();
    let a = theme.accent.fg();
    let m = theme.muted.fg();
    let p = theme.primary.fg();
    let rarity_color = state.rarity.color_code();

    let info_lines = [
        format!(
            "{a}{}{r} | {p}{}{r} | {rarity_color}{}{r} | Lv.{}{r}",
            state.name,
            state.species.label(),
            state.rarity.label(),
            state.level,
        ),
        format!(
            "{m}Mood: {} | XP: {}{r}",
            state.mood.label(),
            format_compact(state.xp),
        ),
        format!("{m}\"{}\"{r}", state.speech),
    ];

    let mut lines = Vec::with_capacity(9);
    lines.push(String::new());
    let sprite = sprite_lines_for_tick(state, tick);
    for (i, sprite_line) in sprite.iter().enumerate() {
        let info = if i < info_lines.len() {
            &info_lines[i]
        } else {
            ""
        };
        lines.push(format!("  {p}{sprite_line}{r}  {info}"));
    }
    lines.push(String::new());
    lines.join("\n")
}

pub fn format_buddy_full(state: &BuddyState, theme: &super::theme::Theme) -> String {
    let rst = super::theme::rst();
    let accent = theme.accent.fg();
    let muted = theme.muted.fg();
    let primary = theme.primary.fg();
    let success = theme.success.fg();
    let warn = theme.warning.fg();
    let bold = super::theme::bold();
    let rarity_color = state.rarity.color_code();

    let mut out = Vec::new();

    out.push(String::new());
    out.push(format!("  {bold}{accent}Token Guardian{rst}"));
    out.push(String::new());

    for line in &state.ascii_art {
        out.push(format!("    {primary}{line}{rst}"));
    }
    out.push(String::new());

    out.push(format!(
        "  {bold}{accent}{}{rst}  {muted}the {}{rst}  {rarity_color}{}{rst}  {muted}Lv.{}{rst}",
        state.name,
        state.species.label(),
        state.rarity.label(),
        state.level,
    ));
    out.push(format!(
        "  {muted}Mood: {}  |  XP: {} / {}  |  Streak: {}d{rst}",
        state.mood.label(),
        format_compact(state.xp),
        format_compact(state.xp_next_level),
        state.streak_days,
    ));
    out.push(format!(
        "  {muted}Tokens saved: {}  |  Bugs prevented: {}{rst}",
        format_compact(state.tokens_saved),
        state.bugs_prevented,
    ));
    out.push(String::new());

    out.push(format!("  {bold}Stats{rst}"));
    out.push(format!(
        "  {success}Compression{rst}  {}",
        stat_bar(state.stats.compression, theme)
    ));
    out.push(format!(
        "  {warn}Vigilance  {rst}  {}",
        stat_bar(state.stats.vigilance, theme)
    ));
    out.push(format!(
        "  {primary}Endurance  {rst}  {}",
        stat_bar(state.stats.endurance, theme)
    ));
    out.push(format!(
        "  {accent}Wisdom     {rst}  {}",
        stat_bar(state.stats.wisdom, theme)
    ));
    out.push(format!(
        "  {muted}Experience {rst}  {}",
        stat_bar(state.stats.experience, theme)
    ));
    out.push(String::new());

    out.push(format!("  {muted}\"{}\"{rst}", state.speech));
    out.push(String::new());

    out.join("\n")
}

fn stat_bar(value: u8, theme: &super::theme::Theme) -> String {
    let filled = (value as usize) / 5;
    let empty = 20 - filled;
    let r = super::theme::rst();
    let g = theme.success.fg();
    let m = theme.muted.fg();
    format!(
        "{g}{}{m}{}{r} {value}/100",
        "█".repeat(filled),
        "░".repeat(empty),
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn species_from_cargo_commands() {
        let mut cmds = HashMap::new();
        cmds.insert(
            "cargo build".to_string(),
            super::super::stats::CommandStats {
                count: 50,
                input_tokens: 1000,
                output_tokens: 500,
            },
        );
        assert_eq!(Species::from_commands(&cmds), Species::Crab);
    }

    #[test]
    fn species_mixed_is_dragon() {
        let mut cmds = HashMap::new();
        cmds.insert(
            "cargo build".to_string(),
            super::super::stats::CommandStats {
                count: 10,
                input_tokens: 0,
                output_tokens: 0,
            },
        );
        cmds.insert(
            "npm install".to_string(),
            super::super::stats::CommandStats {
                count: 10,
                input_tokens: 0,
                output_tokens: 0,
            },
        );
        cmds.insert(
            "python app.py".to_string(),
            super::super::stats::CommandStats {
                count: 10,
                input_tokens: 0,
                output_tokens: 0,
            },
        );
        assert_eq!(Species::from_commands(&cmds), Species::Dragon);
    }

    #[test]
    fn species_empty_is_egg() {
        let cmds = HashMap::new();
        assert_eq!(Species::from_commands(&cmds), Species::Egg);
    }

    #[test]
    fn rarity_levels() {
        assert_eq!(Rarity::from_tokens_saved(0), Rarity::Egg);
        assert_eq!(Rarity::from_tokens_saved(5_000), Rarity::Egg);
        assert_eq!(Rarity::from_tokens_saved(50_000), Rarity::Common);
        assert_eq!(Rarity::from_tokens_saved(500_000), Rarity::Uncommon);
        assert_eq!(Rarity::from_tokens_saved(5_000_000), Rarity::Rare);
        assert_eq!(Rarity::from_tokens_saved(50_000_000), Rarity::Epic);
        assert_eq!(Rarity::from_tokens_saved(500_000_000), Rarity::Legendary);
    }

    #[test]
    fn name_is_deterministic() {
        let s = user_seed();
        let n1 = generate_name(s);
        let n2 = generate_name(s);
        assert_eq!(n1, n2);
    }

    #[test]
    fn format_compact_values() {
        assert_eq!(format_compact(500), "500");
        assert_eq!(format_compact(1_500), "1.5K");
        assert_eq!(format_compact(2_500_000), "2.5M");
        assert_eq!(format_compact(3_000_000_000), "3.0B");
    }

    #[test]
    fn procedural_sprite_returns_7_lines() {
        for seed in [0u64, 1, 42, 999, 12345, 69_119_999, u64::MAX] {
            let traits = CreatureTraits::from_seed(seed);
            for mood in &[
                Mood::Ecstatic,
                Mood::Happy,
                Mood::Content,
                Mood::Worried,
                Mood::Sleeping,
            ] {
                let sp = render_sprite(&traits, mood);
                assert_eq!(sp.len(), 7, "sprite for seed={seed}, mood={mood:?}");
            }
        }
    }

    #[test]
    fn creature_traits_are_deterministic() {
        let t1 = CreatureTraits::from_seed(42);
        let t2 = CreatureTraits::from_seed(42);
        assert_eq!(t1.head, t2.head);
        assert_eq!(t1.eyes, t2.eyes);
        assert_eq!(t1.mouth, t2.mouth);
        assert_eq!(t1.ears, t2.ears);
        assert_eq!(t1.body, t2.body);
        assert_eq!(t1.legs, t2.legs);
        assert_eq!(t1.tail, t2.tail);
        assert_eq!(t1.markings, t2.markings);
    }

    #[test]
    fn different_seeds_produce_different_traits() {
        let t1 = CreatureTraits::from_seed(1);
        let t2 = CreatureTraits::from_seed(9999);
        let same = t1.head == t2.head
            && t1.eyes == t2.eyes
            && t1.mouth == t2.mouth
            && t1.ears == t2.ears
            && t1.body == t2.body
            && t1.legs == t2.legs
            && t1.tail == t2.tail
            && t1.markings == t2.markings;
        assert!(
            !same,
            "seeds 1 and 9999 should differ in at least one trait"
        );
    }

    #[test]
    fn total_combinations_is_69m() {
        assert_eq!(12u64 * 10 * 10 * 12 * 10 * 10 * 8 * 6, 69_120_000);
    }

    #[test]
    fn xp_next_level_increases() {
        let lv1 = (1u64 + 1) * (1 + 1) * 50;
        let lv10 = (10u64 + 1) * (10 + 1) * 50;
        assert!(lv10 > lv1);
    }
}
