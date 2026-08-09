//! Rug-type classification + bundle detection (G6 + G7).
//!
//! Grounded in academic research:
//! - arXiv:2603.24625 "From Hype to Collapse" — 3 rug patterns on Solana:
//!   1. Freeze Authority Abuse — creator freezes accounts, then dumps
//!   2. Liquidity Withdrawal — creator pulls LP liquidity
//!   3. Pump-and-Dump — creator inflates price, then dumps tokens
//! - NoesisAPI bundle classification — 4 categories:
//!   Bundler / Sniper / RatTrader / Organic
//! - arXiv:2509.01168 — 5-minute early-warning window for rug detection;
//!   we use 4 ticks (~2 min) for Solana's faster finality.

/// The type of rug pull being executed (G6).
///
/// Derived from on-chain behavior patterns identified in arXiv:2603.24625.
/// Each rug type requires a different exit strategy — the engine uses this
/// to accelerate exits when a rug pattern is detected mid-hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RugType {
    /// No rug detected — the token is behaving normally.
    Clean,
    /// Freeze Authority Abuse: the creator freezes holder accounts (or the
    /// associated token account), preventing sells. This is the most
    /// extractive pattern — the creator traps capital then dumps their own
    /// tokens at the artificially-maintained price. Exit ASAP if held.
    FreezeAuthorityAbuse,
    /// Liquidity Withdrawal: the creator pulls liquidity from the bonding
    /// curve or AMM pool. This causes immediate price collapse. The
    /// distinguishing signal is a large liquidity drop paired with the
    /// creator's sell activity.
    LiquidityWithdrawal,
    /// Pump-and-Dump: the creator (or coordinated wallets) inflates the
    /// price through wash trading or coordinated buys, then dumps their
    /// holdings. The distinguishing signal is a rapid price spike followed
    /// by concentrated selling from wallets that bought early.
    PumpAndDump,
    /// Unknown rug pattern — rug signals detected but the type cannot be
    /// classified with available data. Treat as the worst case (exit).
    UnknownRug,
}

/// The bundle classification of a token's launch activity (G7).
///
/// Derived from NoesisAPI's bundle detection taxonomy. The engine uses this
/// to veto pre-entry when bundler supply exceeds the rejection threshold.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BundleClass {
    /// No bundle activity detected — organic trading patterns.
    Organic,
    /// Bundler: a single entity submits multiple transactions in the same
    /// block to manipulate the market (e.g., snipe the bonding curve).
    /// High bundler supply (>25%) indicates organized extraction.
    Bundler,
    /// Sniper: rapid automated buys at launch, typically within the first
    /// few slots. Distinct from bundlers in that snipers use speed rather
    /// than block-level manipulation.
    Sniper,
    /// Rat Trader: coordinated wash-trading rings that inflate volume to
    /// attract organic buyers, then dump. The "rat" refers to the
    /// scam-like coordinated behavior.
    RatTrader,
}

/// Inputs for rug-type classification (G6).
#[derive(Clone, Copy, Debug, Default)]
pub struct RugTypeInputs {
    /// Whether the creator has freeze authority over the token's mint.
    pub creator_has_freeze_authority: bool,
    /// Whether freeze authority was exercised (accounts frozen).
    pub freeze_exercised: bool,
    /// Percentage of liquidity withdrawn by the creator, in bps.
    /// 10000 = 100% withdrawn. 0 = no withdrawal.
    pub liquidity_withdrawn_bps: u32,
    /// Percentage of creator's token supply sold, in bps.
    /// 10000 = 100% of creator tokens dumped. 0 = no dump.
    pub creator_sold_bps: u32,
    /// Price change from peak, in bps. 10000 = 100% drop from peak.
    /// 0 = at or above peak. Used to detect pump-and-dump pattern.
    pub price_drop_from_peak_bps: u32,
    /// Number of slots between the token's launch and the rug signal.
    /// Used to distinguish rapid pump-and-dumps from slow extraction.
    pub slots_since_launch: u64,
    /// Whether coordinated wallets (detected via wallet graph) participated
    /// in the price pump before the dump.
    pub coordinated_pump_detected: bool,
}

/// Inputs for bundle classification (G7).
#[derive(Clone, Copy, Debug, Default)]
pub struct BundleInputs {
    /// Percentage of the first-slot transactions attributed to bundlers.
    /// 10000 = 100%. NoesisAPI: >25% = organized extraction (hard veto).
    pub bundler_supply_pct_bps: u32,
    /// Number of distinct bundler wallets detected in the first slot.
    pub bundler_wallet_count: u32,
    /// Whether sniper bots were detected (rapid automated buys at launch).
    pub sniper_detected: bool,
    /// Whether wash-trading rings were detected (coordinated buy/sell cycles).
    pub wash_trading_detected: bool,
    /// Total transaction count in the first slot.
    pub first_slot_tx_count: u32,
}

/// Classify the rug type from on-chain behavior (G6).
///
/// The classification follows the arXiv:2603.24625 taxonomy:
/// 1. Freeze Authority Abuse — freeze authority exists AND was exercised
/// 2. Liquidity Withdrawal — >50% liquidity withdrawn by creator
/// 3. Pump-and-Dump — >70% price drop from peak AND creator sold >50%
/// 4. Unknown Rug — rug signals present but pattern unclear
#[must_use]
pub fn classify_rug_type(inputs: &RugTypeInputs) -> RugType {
    // Pattern 1: Freeze Authority Abuse
    // The creator has freeze authority AND has exercised it — this is the
    // most extractive pattern. arXiv:2603.24625 found this in ~18% of rugs.
    if inputs.creator_has_freeze_authority && inputs.freeze_exercised {
        return RugType::FreezeAuthorityAbuse;
    }

    // Pattern 2: Liquidity Withdrawal
    // >50% of liquidity pulled by the creator. arXiv:2603.24625 found this
    // in ~31% of rugs.
    if inputs.liquidity_withdrawn_bps >= 5_000 {
        return RugType::LiquidityWithdrawal;
    }

    // Pattern 3: Pump-and-Dump
    // >70% price drop from peak AND creator sold >50% of their tokens AND
    // the dump happened within 5000 slots (~33 min) of launch. The
    // coordinated_pump_detected flag strengthens the classification.
    if inputs.price_drop_from_peak_bps >= 7_000
        && inputs.creator_sold_bps >= 5_000
        && inputs.slots_since_launch <= 5_000
    {
        return RugType::PumpAndDump;
    }

    // Pattern 3b: Pump-and-Dump with coordinated wallets
    // If coordinated wallets were detected in the pump phase, lower the
    // thresholds (coordinated behavior is a stronger rug signal).
    if inputs.coordinated_pump_detected
        && inputs.price_drop_from_peak_bps >= 5_000
        && inputs.creator_sold_bps >= 3_000
    {
        return RugType::PumpAndDump;
    }

    // Fallback: if any rug signal is present but the pattern is unclear,
    // classify as UnknownRug. The engine treats this as the worst case.
    if inputs.freeze_exercised
        || inputs.liquidity_withdrawn_bps > 0
        || inputs.creator_sold_bps > 0
        || inputs.price_drop_from_peak_bps > 0
    {
        return RugType::UnknownRug;
    }

    RugType::Clean
}

/// Classify the bundle activity from first-slot transactions (G7).
///
/// Follows the NoesisAPI taxonomy: Bundler / Sniper / RatTrader / Organic.
/// The bundler supply percentage is the primary signal — >25% = organized
/// extraction (hard veto in the engine's pre-entry gate).
#[must_use]
pub fn classify_bundle(inputs: &BundleInputs) -> BundleClass {
    // Bundler: >25% of first-slot supply from bundler wallets
    if inputs.bundler_supply_pct_bps >= 2_500 {
        return BundleClass::Bundler;
    }

    // RatTrader: wash-trading detected (coordinated buy/sell cycles)
    if inputs.wash_trading_detected {
        return BundleClass::RatTrader;
    }

    // Sniper: rapid automated buys detected but no bundling
    if inputs.sniper_detected {
        return BundleClass::Sniper;
    }

    BundleClass::Organic
}

/// Whether the bundle classification should trigger a pre-entry veto (G7).
///
/// Returns true when the bundler supply exceeds the rejection threshold
/// (default 25% = 2500 bps). The engine calls this in the gate to veto
/// admission of tokens with organized extraction signatures.
#[must_use]
pub fn bundle_should_veto(inputs: &BundleInputs, rejection_threshold_bps: u32) -> bool {
    inputs.bundler_supply_pct_bps >= rejection_threshold_bps
}

/// The early-warning window in slots for rug detection (G6).
///
/// arXiv:2509.01168 found a 5-minute window effective on TON blockchain.
/// Solana's finality is faster (~400ms slots vs TON's ~3s), so we use
/// 4 ticks × 400ms ≈ 1.6 seconds for the early-warning detection window.
/// This constant is consumed by the engine's exit acceleration logic.
#[must_use]
pub const fn rug_early_warning_slots() -> u64 {
    4
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clean_token() {
        let inputs = RugTypeInputs {
            creator_has_freeze_authority: false,
            freeze_exercised: false,
            liquidity_withdrawn_bps: 0,
            creator_sold_bps: 0,
            price_drop_from_peak_bps: 0,
            slots_since_launch: 100,
            coordinated_pump_detected: false,
        };
        assert_eq!(classify_rug_type(&inputs), RugType::Clean);
    }

    #[test]
    fn test_freeze_authority_abuse() {
        let inputs = RugTypeInputs {
            creator_has_freeze_authority: true,
            freeze_exercised: true,
            ..RugTypeInputs::default()
        };
        assert_eq!(classify_rug_type(&inputs), RugType::FreezeAuthorityAbuse);
    }

    #[test]
    fn test_liquidity_withdrawal() {
        let inputs = RugTypeInputs {
            liquidity_withdrawn_bps: 6_000, // 60% withdrawn
            ..RugTypeInputs::default()
        };
        assert_eq!(classify_rug_type(&inputs), RugType::LiquidityWithdrawal);
    }

    #[test]
    fn test_pump_and_dump() {
        let inputs = RugTypeInputs {
            price_drop_from_peak_bps: 8_000, // 80% drop
            creator_sold_bps: 6_000,         // 60% sold
            slots_since_launch: 1_000,       // within 5000 slots
            ..RugTypeInputs::default()
        };
        assert_eq!(classify_rug_type(&inputs), RugType::PumpAndDump);
    }

    #[test]
    fn test_pump_and_dump_coordinated() {
        let inputs = RugTypeInputs {
            price_drop_from_peak_bps: 5_500, // 55% drop
            creator_sold_bps: 3_500,         // 35% sold
            slots_since_launch: 500,
            coordinated_pump_detected: true,
            ..RugTypeInputs::default()
        };
        assert_eq!(classify_rug_type(&inputs), RugType::PumpAndDump);
    }

    #[test]
    fn test_unknown_rug() {
        let inputs = RugTypeInputs {
            creator_sold_bps: 1_000, // Some selling, but not enough to classify
            ..RugTypeInputs::default()
        };
        assert_eq!(classify_rug_type(&inputs), RugType::UnknownRug);
    }

    #[test]
    fn test_bundle_organic() {
        let inputs = BundleInputs {
            ..BundleInputs::default()
        };
        assert_eq!(classify_bundle(&inputs), BundleClass::Organic);
    }

    #[test]
    fn test_bundle_bundler() {
        let inputs = BundleInputs {
            bundler_supply_pct_bps: 3_000, // 30% > 25% threshold
            ..BundleInputs::default()
        };
        assert_eq!(classify_bundle(&inputs), BundleClass::Bundler);
    }

    #[test]
    fn test_bundle_sniper() {
        let inputs = BundleInputs {
            sniper_detected: true,
            ..BundleInputs::default()
        };
        assert_eq!(classify_bundle(&inputs), BundleClass::Sniper);
    }

    #[test]
    fn test_bundle_rat_trader() {
        let inputs = BundleInputs {
            wash_trading_detected: true,
            ..BundleInputs::default()
        };
        assert_eq!(classify_bundle(&inputs), BundleClass::RatTrader);
    }

    #[test]
    fn test_bundle_veto_threshold() {
        let inputs = BundleInputs {
            bundler_supply_pct_bps: 2_500, // exactly 25%
            ..BundleInputs::default()
        };
        assert!(bundle_should_veto(&inputs, 2_500));
        assert!(!bundle_should_veto(
            &inputs,
            2_501 // just above 25%
        ));
    }

    #[test]
    fn test_early_warning_slots() {
        assert_eq!(rug_early_warning_slots(), 4);
    }
}
