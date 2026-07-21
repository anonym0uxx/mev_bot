//! Shared types for the momentum engine.

/// A token that passed scoring and is ready for momentum entry.
/// Zero-copy: all fields are Copy, no heap.
#[derive(Debug, Clone, Copy)]
pub struct ScoredToken {
    pub mint: [u8; 32],
    pub score: f64,
    pub magnitude: f64,
    pub kelly_size_lamports: u64,
    pub p_permille: u16,
    pub r_x100: u16,
    pub f_permille: u16,
    pub conviction_tier: u8,
    pub vsol_reserves: u64,
    pub timestamp_ms: u64,
}

/// Graduation enrichment data passed to momentum engine at migration time.
/// Zero-copy, all Copy fields.
#[derive(Debug, Clone, Copy)]
pub struct GradEnrichment {
    pub grad_speed_s: u32,
    pub volume_sol_x100: u32,
    pub buys_5s: u16,
    pub unique_buyers: u16,
    pub sells_5s: u16,
}

impl GradEnrichment {
    pub const UNKNOWN: Self = Self {
        grad_speed_s: 0,
        volume_sol_x100: 0,
        buys_5s: 0,
        unique_buyers: 0,
        sells_5s: 0,
    };
}
