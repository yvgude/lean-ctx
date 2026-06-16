//! Electricity-footprint estimate for saved tokens.
//!
//! This mirrors the website `/metrics` methodology (same `J_PER_TOKEN`) so a user's local
//! "energy saved" figure and the community scoreboard always reconcile.
//!
//! ~0.4 J per saved token is a deliberately conservative midpoint of measured modern
//! inference (Llama-3-70B FP8 on H100 + vLLM ≈ 0.39 J/token, John Snow Labs "Tokens per
//! Joule", 2025; query-level estimates such as Epoch AI's ~0.3 Wh per GPT-4o query imply
//! more). lean-ctx mostly removes cheaper prefill/context tokens, so we never overstate.
//! Real figures vary by model and hardware — this is always surfaced as an estimate.

/// Joules of inference compute avoided per token that lean-ctx kept out of the context.
pub const J_PER_TOKEN: f64 = 0.4;

/// Watt-hours in a full smartphone charge — the relatable yardstick used in the UI.
pub const WH_PER_PHONE_CHARGE: f64 = 12.0;

/// Grams of CO₂-equivalent per kWh of grid electricity. ~475 g/kWh is the global
/// average grid carbon intensity (IEA, ~2023) — a transparent, conservative
/// midpoint. Users on cleaner grids can override via `LEAN_CTX_GRID_CO2_G_PER_KWH`
/// so the local footprint reflects their actual electricity mix.
pub const G_CO2_PER_KWH: f64 = 475.0;

/// Energy (Wh) saved for a given number of saved tokens. `Wh = tokens · J/token / 3600`.
#[must_use]
pub fn wh_for_tokens(tokens_saved: u64) -> f64 {
    tokens_saved as f64 * J_PER_TOKEN / 3600.0
}

/// Equivalent number of full smartphone charges for a given saved-token count.
#[must_use]
pub fn phone_charges(tokens_saved: u64) -> f64 {
    wh_for_tokens(tokens_saved) / WH_PER_PHONE_CHARGE
}

/// Effective grid carbon intensity (g CO₂/kWh), honoring the
/// `LEAN_CTX_GRID_CO2_G_PER_KWH` override when it is a positive, finite number.
#[must_use]
pub fn grid_co2_g_per_kwh() -> f64 {
    std::env::var("LEAN_CTX_GRID_CO2_G_PER_KWH")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v > 0.0)
        .unwrap_or(G_CO2_PER_KWH)
}

/// Grams of CO₂-equivalent avoided for a given number of saved tokens.
/// `g = Wh / 1000 · gridIntensity`.
#[must_use]
pub fn co2_grams_for_tokens(tokens_saved: u64) -> f64 {
    wh_for_tokens(tokens_saved) / 1_000.0 * grid_co2_g_per_kwh()
}

/// Human-readable CO₂ mass with adaptive units (`g` / `kg` / `t`).
#[must_use]
pub fn format_co2(grams: f64) -> String {
    if !grams.is_finite() || grams <= 0.0 {
        "0 g".to_string()
    } else if grams >= 1_000_000.0 {
        format!("{:.1} t", grams / 1_000_000.0)
    } else if grams >= 1_000.0 {
        format!("{:.1} kg", grams / 1_000.0)
    } else {
        format!("{grams:.0} g")
    }
}

/// Convenience: formatted CO₂ string straight from a saved-token count.
#[must_use]
pub fn format_co2_for_tokens(tokens_saved: u64) -> String {
    format_co2(co2_grams_for_tokens(tokens_saved))
}

/// Human-readable energy with adaptive units (`Wh` / `kWh` / `MWh`).
#[must_use]
pub fn format_wh(wh: f64) -> String {
    if !wh.is_finite() || wh <= 0.0 {
        "0 Wh".to_string()
    } else if wh >= 1_000_000.0 {
        format!("{:.1} MWh", wh / 1_000_000.0)
    } else if wh >= 1_000.0 {
        format!("{:.1} kWh", wh / 1_000.0)
    } else {
        format!("{wh:.0} Wh")
    }
}

/// Convenience: formatted energy string straight from a saved-token count.
#[must_use]
pub fn format_for_tokens(tokens_saved: u64) -> String {
    format_wh(wh_for_tokens(tokens_saved))
}

/// Rounded phone-charge equivalent as a display string (e.g. `"≈ 117 phone charges"`),
/// or `None` when the saving is too small to round to at least one charge.
#[must_use]
pub fn phone_charges_hint(tokens_saved: u64) -> Option<String> {
    let charges = phone_charges(tokens_saved);
    if charges < 0.5 {
        return None;
    }
    Some(format!("≈ {} phone charges", charges.round() as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wh_scales_linearly_with_tokens() {
        // 9000 tokens · 0.4 J / 3600 = exactly 1 Wh.
        assert!((wh_for_tokens(9_000) - 1.0).abs() < 1e-9);
        assert!((wh_for_tokens(0)).abs() < 1e-9);
    }

    #[test]
    fn format_picks_adaptive_units() {
        assert_eq!(format_wh(0.0), "0 Wh");
        assert_eq!(format_wh(-5.0), "0 Wh");
        assert_eq!(format_wh(42.4), "42 Wh");
        assert_eq!(format_wh(1_500.0), "1.5 kWh");
        assert_eq!(format_wh(2_500_000.0), "2.5 MWh");
    }

    #[test]
    fn format_for_tokens_matches_methodology() {
        // 12.8M tokens ≈ 1422 Wh ≈ 1.4 kWh (the figure a real user would see).
        assert_eq!(format_for_tokens(12_800_000), "1.4 kWh");
    }

    #[test]
    fn phone_charge_hint_suppressed_when_tiny() {
        assert!(phone_charges_hint(0).is_none());
        // 12 Wh = 1 charge needs 108k tokens; far below that → no hint.
        assert!(phone_charges_hint(1_000).is_none());
        assert!(phone_charges_hint(12_800_000).is_some());
    }

    #[test]
    fn co2_scales_with_energy_and_grid_intensity() {
        // 9000 tokens = 1 Wh = 0.001 kWh · 475 g/kWh = 0.475 g.
        let g = co2_grams_for_tokens(9_000);
        assert!((g - 0.475).abs() < 1e-6, "got {g}");
        assert!(co2_grams_for_tokens(0).abs() < 1e-12);
        // Linear in tokens.
        assert!((co2_grams_for_tokens(18_000) - 2.0 * g).abs() < 1e-6);
    }

    #[test]
    fn format_co2_picks_adaptive_units() {
        assert_eq!(format_co2(0.0), "0 g");
        assert_eq!(format_co2(-3.0), "0 g");
        assert_eq!(format_co2(42.4), "42 g");
        assert_eq!(format_co2(1_500.0), "1.5 kg");
        assert_eq!(format_co2(2_500_000.0), "2.5 t");
    }

    #[test]
    fn grid_intensity_default_when_no_override() {
        // The override is read from the environment; without it we use the constant.
        // (Env mutation is covered indirectly; here we assert the documented default.)
        // SAFETY: single-threaded context (test/startup); no concurrent env access.
        unsafe { std::env::remove_var("LEAN_CTX_GRID_CO2_G_PER_KWH") };
        assert!((grid_co2_g_per_kwh() - G_CO2_PER_KWH).abs() < 1e-9);
    }
}
