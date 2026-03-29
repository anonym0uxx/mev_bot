//! Regime classifier — ported from TypeScript `src/regime/classifier.ts`.
//!
//! Classifies tokens into regimes based on bonding curve progress,
//! migration status, age, and exclusion flags (mayhem, tokenized agents).
//! Non-tradeable regimes are excluded from entry decisions.

/// Token regime classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Regime {
    Excluded,
    EarlyCurve,
    MidCurve,
    LateCurve,
    GraduationBoundary,
    PostMigration,
}

impl Regime {
    /// Whether this regime is tradeable (eligible for entry).
    /// EARLY_CURVE, MID_CURVE, LATE_CURVE are valid.
    /// GRADUATION_BOUNDARY is excluded (too close to migration, high slippage risk).
    #[inline]
    pub fn is_tradeable(self) -> bool {
        matches!(self, Regime::EarlyCurve | Regime::MidCurve | Regime::LateCurve)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Regime::Excluded => "EXCLUDED",
            Regime::EarlyCurve => "EARLY_CURVE",
            Regime::MidCurve => "MID_CURVE",
            Regime::LateCurve => "LATE_CURVE",
            Regime::GraduationBoundary => "GRADUATION_BOUNDARY",
            Regime::PostMigration => "POST_MIGRATION",
        }
    }
}

/// Configuration for regime classification thresholds.
#[derive(Debug, Clone)]
pub struct RegimeConfig {
    pub early_curve_max_progress: f64,
    pub mid_curve_max_progress: f64,
    pub late_curve_max_progress: f64,
    pub graduation_boundary_start: f64,
    pub graduation_boundary_end: f64,
    pub max_token_age_s: u64,
    pub exclude_mayhem: bool,
    pub exclude_tokenized_agent: bool,
}

impl Default for RegimeConfig {
    fn default() -> Self {
        Self {
            early_curve_max_progress: 0.15,
            mid_curve_max_progress: 0.50,
            late_curve_max_progress: 0.85,
            graduation_boundary_start: 0.85,
            graduation_boundary_end: 1.0,
            max_token_age_s: 300,
            exclude_mayhem: true,
            exclude_tokenized_agent: true,
        }
    }
}

/// Input data for regime classification.
pub struct RegimeInput {
    pub bonding_curve_progress: f64,
    pub migrated: bool,
    pub token_age_ms: u64,
    pub is_mayhem: bool,
    pub is_tokenized_agent: bool,
}

/// Classify a token into a regime.
///
/// Gate order (cheapest checks first):
/// 1. Exclusion flags (mayhem, tokenized agent)
/// 2. Age check
/// 3. Migration check
/// 4. Bonding curve progress ranges
#[inline]
pub fn classify_regime(input: &RegimeInput, config: &RegimeConfig) -> Regime {
    // Exclusion checks first
    if config.exclude_mayhem && input.is_mayhem {
        return Regime::Excluded;
    }
    if config.exclude_tokenized_agent && input.is_tokenized_agent {
        return Regime::Excluded;
    }

    // Age check
    let token_age_s = input.token_age_ms / 1000;
    if token_age_s > config.max_token_age_s {
        return Regime::Excluded;
    }

    // Post-migration check
    if input.migrated {
        return Regime::PostMigration;
    }

    let progress = input.bonding_curve_progress;

    // -1 sentinel = reserves unknown → classify as EARLY_CURVE (conservative)
    if progress < 0.0 {
        return Regime::EarlyCurve;
    }

    // Graduation boundary takes precedence over late curve
    if progress >= config.graduation_boundary_start
        && progress <= config.graduation_boundary_end
    {
        return Regime::GraduationBoundary;
    }

    // Curve stages
    if progress <= config.early_curve_max_progress {
        Regime::EarlyCurve
    } else if progress <= config.mid_curve_max_progress {
        Regime::MidCurve
    } else if progress <= config.late_curve_max_progress {
        Regime::LateCurve
    } else {
        // Fallthrough — should be caught by graduation boundary
        Regime::GraduationBoundary
    }
}

/// Compute bonding curve progress from virtual token reserves.
///
/// Pump.fun: starts with ~1,073,000,000 virtual tokens.
/// As tokens are bought, vTokens decreases.
/// Progress = 1 - (current_vTokens / initial_vTokens)
///
/// Returns -1.0 if reserves are 0 (data not yet available).
#[inline]
pub fn compute_bonding_curve_progress(
    vtokens_in_curve: u64,
    initial_virtual_tokens: u64,
) -> f64 {
    if initial_virtual_tokens == 0 {
        return 0.0;
    }
    // Zero reserves = data not available → return -1 sentinel
    if vtokens_in_curve == 0 {
        return -1.0;
    }
    let progress = 1.0 - (vtokens_in_curve as f64 / initial_virtual_tokens as f64);
    progress.clamp(0.0, 1.0)
}

/// Initial virtual token reserves for pump.fun bonding curves.
pub const INITIAL_VIRTUAL_TOKENS: u64 = 1_073_000_000_000_000;

/// Detect "Mayhem" mode tokens from name/symbol metadata.
///
/// Mayhem tokens have specific markers in their metadata.
/// Called on token creation events (PumpPortal `create`).
#[inline]
pub fn detect_mayhem(name: &str, symbol: &str) -> bool {
    let name_lower = name.to_ascii_lowercase();
    let symbol_lower = symbol.to_ascii_lowercase();
    name_lower.contains("mayhem")
        || symbol_lower.contains("mayhem")
        || name_lower.contains("🔥mayhem")
}

/// Detect "Tokenized Agent" tokens from name/symbol metadata.
///
/// Tokens that are AI agent tokens — typically low-quality, bot-driven.
#[inline]
pub fn detect_tokenized_agent(name: &str, symbol: &str) -> bool {
    let combined = format!("{} {}", name.to_ascii_lowercase(), symbol.to_ascii_lowercase());
    combined.contains("agent") && (combined.contains("ai") || combined.contains("bot"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_early_curve() {
        let cfg = RegimeConfig::default();
        let input = RegimeInput {
            bonding_curve_progress: 0.05,
            migrated: false,
            token_age_ms: 5000,
            is_mayhem: false,
            is_tokenized_agent: false,
        };
        assert_eq!(classify_regime(&input, &cfg), Regime::EarlyCurve);
        assert!(Regime::EarlyCurve.is_tradeable());
    }

    #[test]
    fn test_mid_curve() {
        let cfg = RegimeConfig::default();
        let input = RegimeInput {
            bonding_curve_progress: 0.30,
            migrated: false,
            token_age_ms: 30_000,
            is_mayhem: false,
            is_tokenized_agent: false,
        };
        assert_eq!(classify_regime(&input, &cfg), Regime::MidCurve);
        assert!(Regime::MidCurve.is_tradeable());
    }

    #[test]
    fn test_graduation_boundary_not_tradeable() {
        let cfg = RegimeConfig::default();
        let input = RegimeInput {
            bonding_curve_progress: 0.90,
            migrated: false,
            token_age_ms: 60_000,
            is_mayhem: false,
            is_tokenized_agent: false,
        };
        assert_eq!(classify_regime(&input, &cfg), Regime::GraduationBoundary);
        assert!(!Regime::GraduationBoundary.is_tradeable());
    }

    #[test]
    fn test_mayhem_excluded() {
        let cfg = RegimeConfig::default();
        let input = RegimeInput {
            bonding_curve_progress: 0.10,
            migrated: false,
            token_age_ms: 5000,
            is_mayhem: true,
            is_tokenized_agent: false,
        };
        assert_eq!(classify_regime(&input, &cfg), Regime::Excluded);
    }

    #[test]
    fn test_tokenized_agent_excluded() {
        let cfg = RegimeConfig::default();
        let input = RegimeInput {
            bonding_curve_progress: 0.10,
            migrated: false,
            token_age_ms: 5000,
            is_mayhem: false,
            is_tokenized_agent: true,
        };
        assert_eq!(classify_regime(&input, &cfg), Regime::Excluded);
    }

    #[test]
    fn test_too_old_excluded() {
        let cfg = RegimeConfig::default();
        let input = RegimeInput {
            bonding_curve_progress: 0.10,
            migrated: false,
            token_age_ms: 600_000, // 10 minutes > 300s max
            is_mayhem: false,
            is_tokenized_agent: false,
        };
        assert_eq!(classify_regime(&input, &cfg), Regime::Excluded);
    }

    #[test]
    fn test_migrated_post_migration() {
        let cfg = RegimeConfig::default();
        let input = RegimeInput {
            bonding_curve_progress: 1.0,
            migrated: true,
            token_age_ms: 120_000,
            is_mayhem: false,
            is_tokenized_agent: false,
        };
        assert_eq!(classify_regime(&input, &cfg), Regime::PostMigration);
        assert!(!Regime::PostMigration.is_tradeable());
    }

    #[test]
    fn test_unknown_reserves_sentinel() {
        assert_eq!(compute_bonding_curve_progress(0, INITIAL_VIRTUAL_TOKENS), -1.0);
    }

    #[test]
    fn test_full_reserves_zero_progress() {
        let p = compute_bonding_curve_progress(INITIAL_VIRTUAL_TOKENS, INITIAL_VIRTUAL_TOKENS);
        assert!((p - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_detect_mayhem() {
        assert!(detect_mayhem("🔥MAYHEM Token", "MHM"));
        assert!(detect_mayhem("Some Mayhem Coin", "X"));
        assert!(!detect_mayhem("Normal Token", "NRM"));
    }

    #[test]
    fn test_detect_tokenized_agent() {
        assert!(detect_tokenized_agent("AI Agent Token", "AAT"));
        assert!(detect_tokenized_agent("Trading Bot Agent", "TBA"));
        assert!(!detect_tokenized_agent("Normal Token", "NRM"));
        assert!(!detect_tokenized_agent("Agent Smith", "AS")); // no "ai" or "bot"
    }
}
