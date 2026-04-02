//! Learned attention weights replacing the heuristic U-curve.
//!
//! Instead of a static quadratic U-curve (Liu et al. approximation),
//! uses empirically measured attention distributions from Experiment B.
//!
//! The learned weights are a piecewise-linear function fitted to
//! real attention maps from multiple LLMs.

use std::path::Path;

pub struct LearnedAttention {
    breakpoints: Vec<(f64, f64)>,
}

/// Empirically measured attention curve from TinyLlama 1.1B on 106 Rust files.
/// Lab Experiment B (2026-04-02): NOT a U-curve (Liu et al.) but an L-curve —
/// massive attention at position 0.00, rapid decay, no recovery at end.
/// Normalized to [0, 1] from raw values where pos 0.0 had 20x the attention of mid-file.
const DEFAULT_BREAKPOINTS: &[(f64, f64)] = &[
    (0.00, 1.00),
    (0.05, 0.055),
    (0.10, 0.055),
    (0.15, 0.050),
    (0.20, 0.050),
    (0.25, 0.055),
    (0.30, 0.045),
    (0.35, 0.040),
    (0.40, 0.040),
    (0.45, 0.040),
    (0.50, 0.035),
    (0.55, 0.030),
    (0.60, 0.035),
    (0.65, 0.025),
    (0.70, 0.020),
    (0.75, 0.020),
    (0.80, 0.020),
    (0.85, 0.015),
    (0.90, 0.010),
    (0.95, 0.000),
    (1.00, 0.000),
];

impl LearnedAttention {
    pub fn load_or_default(model_dir: &Path) -> Self {
        let config_path = model_dir.join("attention_weights.json");
        if config_path.exists() {
            match Self::load_from_file(&config_path) {
                Ok(attn) => {
                    tracing::info!(
                        "Learned attention loaded ({} breakpoints) from {:?}",
                        attn.breakpoints.len(),
                        config_path,
                    );
                    return attn;
                }
                Err(e) => {
                    tracing::warn!("Failed to load attention weights: {e}. Using defaults.");
                }
            }
        }

        Self::with_defaults()
    }

    pub fn with_defaults() -> Self {
        Self {
            breakpoints: DEFAULT_BREAKPOINTS.to_vec(),
        }
    }

    fn load_from_file(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let data: Vec<(f64, f64)> = serde_json::from_str(&content)?;
        if data.len() < 2 {
            anyhow::bail!("Need at least 2 breakpoints");
        }
        Ok(Self { breakpoints: data })
    }

    /// Compute attention weight for a normalized position [0.0, 1.0].
    /// Uses piecewise-linear interpolation between breakpoints.
    pub fn weight(&self, position: f64) -> f64 {
        let pos = position.clamp(0.0, 1.0);

        if self.breakpoints.is_empty() {
            return 0.5;
        }

        if pos <= self.breakpoints[0].0 {
            return self.breakpoints[0].1;
        }

        let last = self.breakpoints.len() - 1;
        if pos >= self.breakpoints[last].0 {
            return self.breakpoints[last].1;
        }

        for i in 0..last {
            let (x0, y0) = self.breakpoints[i];
            let (x1, y1) = self.breakpoints[i + 1];
            if pos >= x0 && pos <= x1 {
                let t = (pos - x0) / (x1 - x0);
                return y0 + t * (y1 - y0);
            }
        }

        0.5
    }

    /// Replace breakpoints with new empirically measured values.
    pub fn update_from_experiment(&mut self, new_breakpoints: Vec<(f64, f64)>) {
        self.breakpoints = new_breakpoints;
    }

    pub fn breakpoint_count(&self) -> usize {
        self.breakpoints.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_l_curve() {
        let attn = LearnedAttention::with_defaults();
        let begin = attn.weight(0.0);
        let middle = attn.weight(0.5);
        let end = attn.weight(1.0);

        assert!(begin > middle, "begin ({begin}) should > middle ({middle})");
        assert!(begin > 0.9, "begin should be > 0.9, got {begin}");
        // L-curve: end is LOW, not high like U-curve
        assert!(end < 0.01, "end should be near 0 (L-curve), got {end}");
        assert!(middle < 0.05, "middle should be low, got {middle}");
    }

    #[test]
    fn interpolation_smooth_after_initial_drop() {
        let attn = LearnedAttention::with_defaults();
        // L-curve: huge drop from pos 0.0 (1.0) to pos 0.05 (0.055) is expected.
        // After the initial drop, the curve should be smooth.
        let mut prev = attn.weight(0.05);
        for i in 2..=20 {
            let pos = i as f64 / 20.0;
            let curr = attn.weight(pos);
            let diff = (curr - prev).abs();
            assert!(diff < 0.02, "Jump too large at pos {pos}: {diff}");
            prev = curr;
        }
    }

    #[test]
    fn boundary_values() {
        let attn = LearnedAttention::with_defaults();
        assert_eq!(attn.weight(-0.5), attn.weight(0.0));
        assert_eq!(attn.weight(1.5), attn.weight(1.0));
    }
}
