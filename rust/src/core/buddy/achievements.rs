use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum AchievementId {
    FirstSave,
    Streak7,
    Streak30,
    Million,
    HundredM,
    Billion,
    AllModes,
    Rate90,
    Bugs10,
    Dragon,
    Lv50,
    Mythic,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Achievement {
    pub id: AchievementId,
    pub name: &'static str,
    pub icon: &'static str,
    pub description: &'static str,
}

impl Achievement {
    #[must_use]
    pub fn badge(&self) -> String {
        format!("{} {}", self.icon, self.name)
    }
}

const ALL: &[Achievement] = &[
    Achievement {
        id: AchievementId::FirstSave,
        name: "First Blood",
        icon: "🩸",
        description: "Saved your first token",
    },
    Achievement {
        id: AchievementId::Streak7,
        name: "Week Warrior",
        icon: "🔥",
        description: "7-day usage streak",
    },
    Achievement {
        id: AchievementId::Streak30,
        name: "Monthly Master",
        icon: "👑",
        description: "30-day usage streak",
    },
    Achievement {
        id: AchievementId::Million,
        name: "Millionaire",
        icon: "💰",
        description: "Saved 1M tokens",
    },
    Achievement {
        id: AchievementId::HundredM,
        name: "Centurion",
        icon: "🏛️",
        description: "Saved 100M tokens",
    },
    Achievement {
        id: AchievementId::Billion,
        name: "Billionaire",
        icon: "💎",
        description: "Saved 1B tokens",
    },
    Achievement {
        id: AchievementId::AllModes,
        name: "Polyglot",
        icon: "📚",
        description: "Used all read modes",
    },
    Achievement {
        id: AchievementId::Rate90,
        name: "Efficiency Expert",
        icon: "⚡",
        description: "90%+ compression rate",
    },
    Achievement {
        id: AchievementId::Bugs10,
        name: "Bug Squasher",
        icon: "🐛",
        description: "Prevented 10 bugs",
    },
    Achievement {
        id: AchievementId::Dragon,
        name: "Dragon Tamer",
        icon: "🐉",
        description: "Mixed-language project",
    },
    Achievement {
        id: AchievementId::Lv50,
        name: "Half Century",
        icon: "🎯",
        description: "Reached Level 50",
    },
    Achievement {
        id: AchievementId::Mythic,
        name: "Ascended",
        icon: "✨",
        description: "Reached Mythic evolution",
    },
];

#[must_use]
pub fn catalog() -> &'static [Achievement] {
    ALL
}

#[must_use]
pub fn check_unlocked(
    tokens_saved: u64,
    streak_days: u32,
    bugs_prevented: u64,
    compression_pct: u8,
    level: u32,
    species: &super::types::Species,
    mode_count: usize,
    evolution: &super::evolution::EvolutionStage,
) -> Vec<&'static Achievement> {
    let mut unlocked = Vec::new();
    for a in ALL {
        let earned = match a.id {
            AchievementId::FirstSave => tokens_saved > 0,
            AchievementId::Streak7 => streak_days >= 7,
            AchievementId::Streak30 => streak_days >= 30,
            AchievementId::Million => tokens_saved >= 1_000_000,
            AchievementId::HundredM => tokens_saved >= 100_000_000,
            AchievementId::Billion => tokens_saved >= 1_000_000_000,
            AchievementId::AllModes => mode_count >= 8,
            AchievementId::Rate90 => compression_pct >= 90,
            AchievementId::Bugs10 => bugs_prevented >= 10,
            AchievementId::Dragon => *species == super::types::Species::Dragon,
            AchievementId::Lv50 => level >= 50,
            AchievementId::Mythic => *evolution == super::evolution::EvolutionStage::Mythic,
        };
        if earned {
            unlocked.push(a);
        }
    }
    unlocked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_save_unlocks() {
        let r = check_unlocked(
            1,
            0,
            0,
            0,
            0,
            &super::super::types::Species::Egg,
            0,
            &super::super::evolution::EvolutionStage::Egg,
        );
        assert!(r.iter().any(|a| a.id == AchievementId::FirstSave));
    }

    #[test]
    fn nothing_at_zero() {
        let r = check_unlocked(
            0,
            0,
            0,
            0,
            0,
            &super::super::types::Species::Egg,
            0,
            &super::super::evolution::EvolutionStage::Egg,
        );
        assert!(r.is_empty());
    }

    #[test]
    fn mythic_unlocks_at_mythic_stage() {
        let r = check_unlocked(
            500_000_000,
            40,
            20,
            95,
            70,
            &super::super::types::Species::Crab,
            8,
            &super::super::evolution::EvolutionStage::Mythic,
        );
        assert!(r.iter().any(|a| a.id == AchievementId::Mythic));
        assert!(r.iter().any(|a| a.id == AchievementId::HundredM));
        assert!(r.iter().any(|a| a.id == AchievementId::Lv50));
    }

    #[test]
    fn catalog_has_12_entries() {
        assert_eq!(catalog().len(), 12);
    }
}
