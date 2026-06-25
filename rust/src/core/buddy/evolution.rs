use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum EvolutionStage {
    #[default]
    Egg,
    Baby,
    Teen,
    Adult,
    Mythic,
}

impl EvolutionStage {
    #[must_use]
    pub fn from_level(level: u32) -> Self {
        match level {
            0..=4 => Self::Egg,
            5..=14 => Self::Baby,
            15..=34 => Self::Teen,
            35..=64 => Self::Adult,
            _ => Self::Mythic,
        }
    }

    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Egg => "Egg",
            Self::Baby => "Baby",
            Self::Teen => "Teen",
            Self::Adult => "Adult",
            Self::Mythic => "Mythic",
        }
    }

    #[must_use]
    pub fn icon(&self) -> &'static str {
        match self {
            Self::Egg => "🥚",
            Self::Baby => "🐣",
            Self::Teen => "🌱",
            Self::Adult => "⚔️",
            Self::Mythic => "✨",
        }
    }

    #[must_use]
    pub fn sprite_height(&self) -> usize {
        match self {
            Self::Egg => 3,
            Self::Baby => 8,
            Self::Teen => 10,
            Self::Adult => 12,
            Self::Mythic => 15,
        }
    }

    /// The next stage, or `None` if already Mythic.
    #[must_use]
    pub fn next(&self) -> Option<Self> {
        match self {
            Self::Egg => Some(Self::Baby),
            Self::Baby => Some(Self::Teen),
            Self::Teen => Some(Self::Adult),
            Self::Adult => Some(Self::Mythic),
            Self::Mythic => None,
        }
    }

    /// Level required to reach this stage.
    #[must_use]
    pub fn min_level(&self) -> u32 {
        match self {
            Self::Egg => 0,
            Self::Baby => 5,
            Self::Teen => 15,
            Self::Adult => 35,
            Self::Mythic => 65,
        }
    }

    /// Progress (0.0..1.0) toward the next evolution within the current stage.
    #[must_use]
    pub fn progress(&self, level: u32) -> f64 {
        let Some(next) = self.next() else {
            return 1.0;
        };
        let range = next.min_level() - self.min_level();
        let within = level.saturating_sub(self.min_level());
        (f64::from(within) / f64::from(range)).min(1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_from_level() {
        assert_eq!(EvolutionStage::from_level(0), EvolutionStage::Egg);
        assert_eq!(EvolutionStage::from_level(4), EvolutionStage::Egg);
        assert_eq!(EvolutionStage::from_level(5), EvolutionStage::Baby);
        assert_eq!(EvolutionStage::from_level(14), EvolutionStage::Baby);
        assert_eq!(EvolutionStage::from_level(15), EvolutionStage::Teen);
        assert_eq!(EvolutionStage::from_level(34), EvolutionStage::Teen);
        assert_eq!(EvolutionStage::from_level(35), EvolutionStage::Adult);
        assert_eq!(EvolutionStage::from_level(64), EvolutionStage::Adult);
        assert_eq!(EvolutionStage::from_level(65), EvolutionStage::Mythic);
        assert_eq!(EvolutionStage::from_level(99), EvolutionStage::Mythic);
    }

    #[test]
    fn progress_within_stage() {
        let stage = EvolutionStage::Egg;
        assert!((stage.progress(0) - 0.0).abs() < 0.01);
        assert!((stage.progress(4) - 0.8).abs() < 0.01);
        assert_eq!(EvolutionStage::Mythic.progress(99), 1.0);
    }

    #[test]
    fn next_chain() {
        let mut s = EvolutionStage::Egg;
        let chain: Vec<_> = std::iter::from_fn(|| {
            let next = s.next()?;
            s = next;
            Some(next)
        })
        .collect();
        assert_eq!(chain.len(), 4);
        assert_eq!(chain[3], EvolutionStage::Mythic);
    }
}
