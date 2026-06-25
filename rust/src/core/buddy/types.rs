use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
    #[must_use]
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

    /// The element/type skin for the one mascot. A user's dominant language maps
    /// to an element that tints the sprite and titles the creature — the body
    /// silhouette is always the same iconic Pixel Sprite.
    #[must_use]
    pub fn element_name(&self) -> &'static str {
        match self {
            Self::Egg => "Null",
            Self::Crab => "Ember",
            Self::Snake => "Venom",
            Self::Owl => "Spark",
            Self::Gopher => "Tide",
            Self::Whale => "Aqua",
            Self::Fox => "Flux",
            Self::Dragon => "Prism",
        }
    }

    /// Intrinsic element colour as a raw 256-colour ANSI foreground escape.
    /// Distinct from the UI theme: fire is always orange, poison always green.
    #[must_use]
    pub fn element_color(&self) -> &'static str {
        match self {
            Self::Egg => "\x1b[38;5;245m",
            Self::Crab => "\x1b[38;5;208m",
            Self::Snake => "\x1b[38;5;70m",
            Self::Owl => "\x1b[38;5;220m",
            Self::Gopher => "\x1b[38;5;44m",
            Self::Whale => "\x1b[38;5;39m",
            Self::Fox => "\x1b[38;5;170m",
            Self::Dragon => "\x1b[38;5;213m",
        }
    }

    /// A single width-1 rune used as the element badge in the nameplate.
    #[must_use]
    pub fn element_glyph(&self) -> &'static str {
        match self {
            Self::Egg => "○",
            Self::Crab => "▲",
            Self::Owl => "✦",
            Self::Gopher | Self::Whale => "≈",
            Self::Fox => "◆",
            Self::Snake | Self::Dragon => "❖",
        }
    }

    #[must_use]
    pub fn from_commands(commands: &HashMap<String, super::super::stats::CommandStats>) -> Self {
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
    #[must_use]
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

    #[must_use]
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

    #[must_use]
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Mood {
    Ecstatic,
    Happy,
    Content,
    Worried,
    Sleeping,
}

impl Mood {
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Ecstatic => "Ecstatic",
            Self::Happy => "Happy",
            Self::Content => "Content",
            Self::Worried => "Worried",
            Self::Sleeping => "Sleeping",
        }
    }

    #[must_use]
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

pub(super) fn user_seed() -> u64 {
    dirs::home_dir().map_or(42, |p| {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        p.hash(&mut h);
        h.finish()
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuddyState {
    pub name: String,
    pub species: Species,
    pub rarity: Rarity,
    pub level: u32,
    pub xp: u64,
    pub xp_next_level: u64,
    pub mood: Mood,
    pub speech: String,
    pub tokens_saved: u64,
    /// Lifetime compression rate (0..100), the headline lean-ctx efficiency stat.
    #[serde(default)]
    pub compression_pct: u8,
    /// Cache hit rate (0..100) across MCP reads.
    #[serde(default)]
    pub cache_hit_rate: u8,
    pub bugs_prevented: u64,
    pub streak_days: u32,
    pub ascii_art: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ascii_frames: Vec<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anim_ms: Option<u32>,
    #[serde(default)]
    pub evolution: super::evolution::EvolutionStage,
    /// Infinite post-Mythic prestige tier. `0` until the buddy starts ascending.
    #[serde(default)]
    pub prestige: u32,
    /// Human-facing form/rank title (never a dead-end). Follows the endless
    /// ladder: Egg → Baby → Teen → Adult → Mythic → Ascended → Stellar → … →
    /// (cosmic ranks, then roman-numeral laps). Used verbatim by the web card.
    #[serde(default)]
    pub form: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub achievement_badges: Vec<String>,
}

impl BuddyState {
    /// The current form/rank title on the endless progression ladder. Before the
    /// buddy ascends this is its evolution stage; afterwards it is the unbounded
    /// cosmic ascension rank, so it is *never* a confusing low-stage word at a
    /// high level.
    #[must_use]
    pub fn form_title(&self) -> String {
        if self.prestige > 0 {
            super::ascension::title(self.prestige)
        } else {
            self.evolution.label().to_string()
        }
    }

    #[must_use]
    pub fn compute() -> Self {
        let store = super::super::stats::load();
        let tokens_saved = store
            .total_input_tokens
            .saturating_sub(store.total_output_tokens);

        let project_root = super::format::detect_project_root_for_buddy();
        let gotcha_store = if project_root.is_empty() {
            super::super::gotcha_tracker::GotchaStore::new("none")
        } else {
            super::super::gotcha_tracker::GotchaStore::load(&project_root)
        };

        let bugs_prevented = gotcha_store.stats.total_prevented;
        let errors_detected = gotcha_store.stats.total_errors_detected;

        let species = Species::from_commands(&store.commands);
        let rarity = Rarity::from_tokens_saved(tokens_saved);

        let xp = tokens_saved / 1000 + store.total_commands * 5 + bugs_prevented * 100;
        // Level is intentionally *uncapped*: the companion keeps levelling forever
        // (square-root curve, so each level costs progressively more XP).
        let level = (xp as f64 / 50.0).sqrt().floor() as u32;
        let xp_next_level = (u64::from(level) + 1) * (u64::from(level) + 1) * 50;

        let streak_days = super::rpg::compute_streak(&store.daily);
        let compression_rate = if store.total_input_tokens > 0 {
            (tokens_saved as f64 / store.total_input_tokens as f64 * 100.0) as u8
        } else {
            0
        };
        let cache_hit_rate = if store.cep.total_cache_reads > 0 {
            (store.cep.total_cache_hits as f64 / store.cep.total_cache_reads as f64 * 100.0) as u8
        } else {
            0
        };

        let mood = super::rpg::compute_mood(
            compression_rate,
            errors_detected,
            bugs_prevented,
            streak_days,
            &store,
        );

        let seed = user_seed();
        let name = super::rpg::generate_name(seed);
        let speech = super::rpg::generate_speech(&mood, tokens_saved, bugs_prevented, streak_days);

        // Buddy evolution: incorporate gain score level into the buddy's display level.
        // The buddy's visible level is the max of XP-based level and gain score level,
        // creating a natural milestone-driven evolution.
        let gain_engine = crate::core::gain::GainEngine::load();
        let gain_score = gain_engine.gain_score(None);
        let gain_level_boost = u32::from(gain_score.level().level);
        let effective_level = level.max(gain_level_boost * 10);

        let evolution = super::evolution::EvolutionStage::from_level(effective_level);
        let prestige = super::ascension::tier(xp);

        let ascii_art = super::mascot_art::sprite_for(&evolution, &mood);
        let ascii_frames = super::mascot_art::frames_for(&evolution, &mood);
        let anim_ms = Some(super::mascot_art::anim_ms_for(&evolution));

        let mode_count = store
            .commands
            .keys()
            .filter(|k| k.starts_with("cli_") || k.starts_with("ctx_"))
            .count();

        let achievements = super::achievements::check_unlocked(
            tokens_saved,
            streak_days,
            bugs_prevented,
            compression_rate,
            effective_level,
            &species,
            mode_count,
            &evolution,
        );
        let achievement_badges: Vec<String> = achievements.iter().map(|a| a.badge()).collect();

        let mut state = Self {
            name,
            species,
            rarity,
            level: effective_level,
            xp,
            xp_next_level,
            mood,
            speech,
            tokens_saved,
            compression_pct: compression_rate,
            cache_hit_rate,
            bugs_prevented,
            streak_days,
            ascii_art,
            ascii_frames,
            anim_ms,
            evolution,
            prestige,
            form: String::new(),
            achievement_badges,
        };
        state.form = state.form_title();
        state
    }
}
