use serde::{Deserialize, Serialize};

/// Controls how strongly solution-efficiency guidance is applied.
#[derive(Default, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "lowercase")]
pub enum SolutionIntensity {
    #[serde(alias = "Off")]
    Off,
    #[serde(alias = "Minimal")]
    Minimal,
    #[default]
    #[serde(alias = "Balanced")]
    Balanced,
    #[serde(alias = "Aggressive")]
    Aggressive,
}

impl SolutionIntensity {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Minimal => "minimal",
            Self::Balanced => "balanced",
            Self::Aggressive => "aggressive",
        }
    }
}

static OFF_INTENSITY: SolutionIntensity = SolutionIntensity::Off;

/// Configuration for solution-efficiency guidance.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct SolutionConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub intensity: SolutionIntensity,
    #[serde(default = "default_true")]
    pub inject_in_instructions: bool,
    #[serde(default = "default_true")]
    pub inject_in_compose: bool,
    #[serde(default = "default_true")]
    pub inject_in_subagents: bool,
    #[serde(default = "default_true")]
    pub track_decisions: bool,
    #[serde(default = "default_true")]
    pub track_loc: bool,
    #[serde(default = "default_true")]
    pub platform_hints: bool,
}

const fn default_true() -> bool {
    true
}

impl Default for SolutionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            intensity: SolutionIntensity::Balanced,
            inject_in_instructions: true,
            inject_in_compose: true,
            inject_in_subagents: true,
            track_decisions: true,
            track_loc: true,
            platform_hints: true,
        }
    }
}

impl SolutionConfig {
    pub fn effective_intensity(&self) -> &SolutionIntensity {
        if self.enabled {
            &self.intensity
        } else {
            &OFF_INTENSITY
        }
    }

    pub fn ladder_text(&self) -> &'static str {
        const BALANCED: &str = "Solution efficiency ladder:\n\
1. YAGNI: is this needed at all?\n\
2. Reuse: does this codebase already provide it?\n\
3. Stdlib: can the standard library handle it?\n\
4. Native: use the platform's built-in capability.\n\
5. Dependency: use an already-installed dependency before adding one.\n\
6. One-line: can the correct solution be one line?\n\
7. Minimum: otherwise implement the minimum working code.\n\
Preserve validation, security, and error-handling.";
        const AGGRESSIVE: &str = "challenge every requirement, prefer deletion.\n\n\
Solution efficiency ladder:\n\
1. YAGNI: is this needed at all?\n\
2. Reuse: does this codebase already provide it?\n\
3. Stdlib: can the standard library handle it?\n\
4. Native: use the platform's built-in capability.\n\
5. Dependency: use an already-installed dependency before adding one.\n\
6. One-line: can the correct solution be one line?\n\
7. Minimum: otherwise implement the minimum working code.\n\
Preserve validation, security, and error-handling.";

        match self.effective_intensity() {
            SolutionIntensity::Off => "",
            SolutionIntensity::Minimal => {
                "Prefer stdlib and native platform alternatives before adding code or dependencies."
            }
            SolutionIntensity::Balanced => BALANCED,
            SolutionIntensity::Aggressive => AGGRESSIVE,
        }
    }
}
