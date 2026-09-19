//! The decision seam: Qwen's answer → the position lifecycle the executor already owns.
//!
//! Two things live here, and nothing else:
//!
//! * **Size suggestion handling.** The entry family is trained to emit a size *tier*
//!   (`SIZE: SMALL|MID|FULL`), and the corpus fixes what each tier means: a fraction of the
//!   traded notional (0.25 / 0.50 / 1.00 of the canonical 1.0 SOL), never of the
//!   account-with-fee-buffer — deploying the buffer leaves zero free cash and drives cash
//!   negative on the priority fee. The tier is the model's suggestion; production turns it
//!   into an executable clip here, bounded by the cash actually free.
//! * **Route mapping.** Which existing lifecycle entry point an action goes to
//!   (`open` / `scale_in` / `close_at`). The management family carries no size line, so the
//!   trained magnitudes are the corpus's own: `ADD` adds 50% of inventory (bounded by free
//!   cash), `REDUCE` trims 50%, `EXIT` closes all.
//!
//! This module is deliberately free of I/O and of the app: P6 wires it into the engine
//! behind the format-parity gate.

use crate::Action;

/// The entry-family size tiers, exactly as the corpus trains them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeTier {
    /// 0.25 x the traded notional.
    Small,
    /// 0.50 x the traded notional.
    Mid,
    /// 1.00 x the traded notional.
    Full,
}

impl SizeTier {
    /// Parse a bare tier token. Strict: only the three trained tokens.
    pub fn parse(s: &str) -> Option<SizeTier> {
        match s.trim() {
            "SMALL" => Some(SizeTier::Small),
            "MID" => Some(SizeTier::Mid),
            "FULL" => Some(SizeTier::Full),
            _ => None,
        }
    }

    /// The token exactly as it appears in the corpus.
    pub fn as_str(self) -> &'static str {
        match self {
            SizeTier::Small => "SMALL",
            SizeTier::Mid => "MID",
            SizeTier::Full => "FULL",
        }
    }

    /// Fraction of the traded notional, in basis points (`size_dimension.SIZE_FRACTIONS`).
    /// Integer on purpose: this crate never puts a float in a money path (§22).
    pub fn fraction_bps(self) -> u32 {
        match self {
            SizeTier::Small => 2_500,
            SizeTier::Mid => 5_000,
            SizeTier::Full => 10_000,
        }
    }
}

/// A parsed model answer: the action, the entry size suggestion (BUY only), and the entry
/// price limit (BUY only).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Decision {
    /// The trained action token.
    pub action: Action,
    /// The size suggestion the model returned, if it returned one.
    pub size: Option<SizeTier>,
    /// The price limit the model returned on a BUY (lamports per raw token).
    pub price_limit: Option<f64>,
}

/// Extract the first `KEY: value` line with the given key.
fn field<'a>(completion: &'a str, key: &str) -> Option<&'a str> {
    let prefix = format!("{key}:");
    completion
        .lines()
        .map(str::trim)
        .find_map(|l| l.strip_prefix(prefix.as_str()).map(str::trim))
}

/// Every way a completion can be off-contract, in one closed set.
///
/// WHY THIS IS AN ENUM AND NOT A STRING. A prompt-drift alarm is only useful if it can be
/// counted per cause: "4 BUYs came back without a size" and "40 completions were garbage"
/// are different incidents with different fixes. Classifying at the parse site — the one
/// place that already knows exactly what was wrong — means telemetry never has to
/// re-parse the text, and a new failure mode is a compile error at every match site
/// rather than a silent new string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OffContract {
    /// No `DECISION:` line anywhere in the completion.
    NoDecisionLine,
    /// A `DECISION:` token outside the trained seven (includes `SELL`).
    UntrainedAction,
    /// `BUY` with no `SIZE:` line — the case the drift alarm exists for.
    BuyWithoutSize,
    /// `BUY` with a size token that was never trained (`NONE`, `HUGE`, `0.75 SOL`, ...).
    BuyWithUntrainedSize,
    /// `BUY` with a `PRICE LIMIT` that is absent-but-present-unparseable or non-positive.
    BuyWithUnusablePriceLimit,
    /// A non-BUY action carrying a size — off-distribution; honoring it would let a `HOLD`
    /// size a position.
    SizeOnNonBuy,
}

impl OffContract {
    /// Every variant, for telemetry registration and exhaustive iteration.
    pub const ALL: [OffContract; 6] = [
        OffContract::NoDecisionLine,
        OffContract::UntrainedAction,
        OffContract::BuyWithoutSize,
        OffContract::BuyWithUntrainedSize,
        OffContract::BuyWithUnusablePriceLimit,
        OffContract::SizeOnNonBuy,
    ];

    /// The stable wire/log label (never reword — dashboards key on it).
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            OffContract::NoDecisionLine => "no_decision_line",
            OffContract::UntrainedAction => "untrained_action",
            OffContract::BuyWithoutSize => "buy_without_size",
            OffContract::BuyWithUntrainedSize => "buy_with_untrained_size",
            OffContract::BuyWithUnusablePriceLimit => "buy_with_unusable_price_limit",
            OffContract::SizeOnNonBuy => "size_on_non_buy",
        }
    }

    fn slot(&self) -> usize {
        match self {
            OffContract::NoDecisionLine => 0,
            OffContract::UntrainedAction => 1,
            OffContract::BuyWithoutSize => 2,
            OffContract::BuyWithUntrainedSize => 3,
            OffContract::BuyWithUnusablePriceLimit => 4,
            OffContract::SizeOnNonBuy => 5,
        }
    }
}

/// A completion that failed the trained-format contract, with its cause.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayloadError {
    /// Which contract clause was violated.
    pub kind: OffContract,
    /// The offending text, for the log line (bounded by the caller's log policy).
    pub detail: String,
}

impl PayloadError {
    fn new(kind: OffContract, detail: String) -> Self {
        Self { kind, detail }
    }
}

impl std::fmt::Display for PayloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "off-contract completion ({}): {}",
            self.kind.as_str(),
            self.detail
        )
    }
}

impl std::error::Error for PayloadError {}

/// Counts off-contract completions by cause and turns them into a rate the caller can alarm on.
///
/// This is the G4 hook for the one drift the corpus audit proved must never happen: a BUY
/// that comes back without a size. The corpus asked for a size on all 13,376 trained BUYs and
/// got one every time, so any `buy_without_size` at all is a signal that the prompt contract
/// broke upstream — not a tier to guess at. `rate_bp` is per 10,000 **accepted** decisions, so
/// it is comparable across traffic levels.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DriftLedger {
    counts: [u64; 6],
    accepted: u64,
}

impl DriftLedger {
    /// An empty ledger.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one accepted (in-contract) decision — the alarm's denominator.
    pub fn record_accepted(&mut self) {
        self.accepted += 1;
    }

    /// Record one off-contract completion.
    pub fn record(&mut self, kind: OffContract) {
        self.counts[kind.slot()] += 1;
    }

    /// Record a parse failure directly from its error.
    pub fn record_error(&mut self, err: &PayloadError) {
        self.record(err.kind);
    }

    /// Count for one cause.
    #[must_use]
    pub fn count(&self, kind: OffContract) -> u64 {
        self.counts[kind.slot()]
    }

    /// Total off-contract completions seen.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.counts.iter().sum()
    }

    /// Accepted (in-contract) decisions seen.
    #[must_use]
    pub fn accepted(&self) -> u64 {
        self.accepted
    }

    /// Off-contract completions per 10,000 accepted decisions (0 when nothing accepted yet).
    #[must_use]
    pub fn rate_bp(&self) -> u32 {
        if self.accepted == 0 {
            return 0;
        }
        let bp = (u128::from(self.total()) * 10_000) / u128::from(self.accepted);
        bp.min(u128::from(u32::MAX)) as u32
    }

    /// Whether the rate has breached a threshold with enough samples to mean anything.
    ///
    /// `min_accepted` guards the denominator: two failures out of three completions is not a
    /// 6,666 bp drift, it is a cold start.
    #[must_use]
    pub fn alarm(&self, min_accepted: u64, threshold_bp: u32) -> bool {
        self.accepted >= min_accepted && self.rate_bp() > threshold_bp
    }

    /// Whether the never-acceptable cause has fired at all (a size-less BUY).
    #[must_use]
    pub fn buy_without_size_seen(&self) -> bool {
        self.count(OffContract::BuyWithoutSize) > 0
    }
}

/// Parse a full model answer, fail-closed.
///
/// The corpus contract is `DECISION: <ACTION>`, then `SIZE: <SMALL|MID|FULL>` **on BUY
/// only**; every other action carries `SIZE: NONE`. `PRICE LIMIT` appears on BUY when the
/// row carries one (68% of trained BUYs carry none — it is optional, not required). A BUY
/// without a usable size is an error — a size the model never chose is exactly the
/// fabricated supervision the corpus exists to prevent. A size on a non-BUY is also an
/// error: it means the completion has drifted off-distribution, and honoring it would let a
/// `HOLD` size a position.
pub fn parse_decision_payload(completion: &str) -> Result<Decision, PayloadError> {
    let raw_action = field(completion, "DECISION")
        .ok_or_else(|| PayloadError::new(OffContract::NoDecisionLine, completion.to_string()))?;
    let action = Action::parse(raw_action)
        .ok_or_else(|| PayloadError::new(OffContract::UntrainedAction, raw_action.to_string()))?;

    let size_raw = field(completion, "SIZE");
    if action == Action::Buy {
        let tok = size_raw.ok_or_else(|| {
            PayloadError::new(
                OffContract::BuyWithoutSize,
                format!("BUY without a SIZE line: {completion}"),
            )
        })?;
        let size = SizeTier::parse(tok).ok_or_else(|| {
            PayloadError::new(
                OffContract::BuyWithUntrainedSize,
                format!("BUY with an untrained size `{tok}`"),
            )
        })?;
        // PRICE LIMIT is OPTIONAL in the corpus: 68% of the trained BUY rows carry none (it
        // was dropped upstream when the price field was renamed), so requiring it would
        // refuse most valid BUYs. When present it must still be a usable number.
        let price = match field(completion, "PRICE LIMIT") {
            None => None,
            Some(tok) => {
                let p = tok.parse::<f64>().map_err(|_| {
                    PayloadError::new(
                        OffContract::BuyWithUnusablePriceLimit,
                        format!("BUY with an unparseable price limit `{tok}`"),
                    )
                })?;
                if !(p.is_finite() && p > 0.0) {
                    return Err(PayloadError::new(
                        OffContract::BuyWithUnusablePriceLimit,
                        format!("BUY with a non-positive price limit `{tok}`"),
                    ));
                }
                Some(p)
            }
        };
        return Ok(Decision {
            action,
            size: Some(size),
            price_limit: price,
        });
    }

    if let Some(tok) = size_raw {
        if tok != "NONE" {
            return Err(PayloadError::new(
                OffContract::SizeOnNonBuy,
                format!(
                    "`{}` carries a size (`{tok}`) — off-distribution completion",
                    action.as_str()
                ),
            ));
        }
    }
    Ok(Decision {
        action,
        size: None,
        price_limit: None,
    })
}

/// The execution-relevant prefix of a completion: the action, and on `BUY` the size
/// tier. Everything after the headline (`INVALIDATION`, `EVIDENCE`, `COUNTEREVIDENCE`,
/// the optional `PRICE LIMIT`, and the citations) is reasoning the journal keeps but the
/// route never reads, so a streaming client may act on this the instant it arrives and
/// let the tail finish in the background.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Headline {
    /// The trained action token.
    pub action: Action,
    /// The size tier on `BUY` (`None` otherwise).
    pub size: Option<SizeTier>,
}

/// Parse the headline out of a *partial* completion, for streaming early-fire.
///
/// Returns:
/// * `Some(Ok(headline))` once the action — and, on `BUY`, the size — has fully arrived;
/// * `Some(Err(kind))` when a definitely-untrained token is already visible;
/// * `None` while the headline is still incomplete (the caller keeps streaming).
///
/// Deliberately narrower than [`parse_decision_payload`]: it never waits for the
/// optional `PRICE LIMIT` or any reasoning line, because the execution path does not
/// read them (the clip resolves from the tier, not the price limit). The full contract
/// is still enforced on the *drained* completion by [`parse_decision_payload`], which
/// feeds the [`DriftLedger`] — early fire changes only *when* the action is taken, never
/// what the journal judges.
#[must_use]
pub fn parse_headline(text: &str) -> Option<Result<Headline, OffContract>> {
    let action_line = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("DECISION:").map(str::trim))?;

    let action = match Action::parse(action_line) {
        Some(a) => a,
        None => return Some(Err(OffContract::UntrainedAction)),
    };

    if action == Action::Buy {
        let size_line = text
            .lines()
            .find_map(|l| l.trim().strip_prefix("SIZE:").map(str::trim))?;
        let size = match SizeTier::parse(size_line) {
            Some(s) => s,
            None => return Some(Err(OffContract::BuyWithUntrainedSize)),
        };
        Some(Ok(Headline {
            action,
            size: Some(size),
        }))
    } else {
        // A non-BUY action is routable the moment the action token lands: its magnitude
        // is corpus-fixed (`route` needs only the action), so there is nothing else to
        // wait for. The full parse still refuses a shifted `SIZE` on the drained text.
        Some(Ok(Headline { action, size: None }))
    }
}

/// Where an action goes in the existing position lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// `ScalpLifecycle::open` — deploy the resolved clip.
    Open,
    /// `ScalpLifecycle::scale_in` — add 50% of **ACCOUNT CAPITAL**, bounded by free
    /// cash. That is the corpus base (see [`ManagementBase`]); sizing it off
    /// inventory is the ~4x defect the parity test pins.
    AddToPosition,
    /// `ScalpLifecycle::close_at` with a 50% trim.
    ReducePosition,
    /// `ScalpLifecycle::close_at`, full size.
    ClosePosition,
    /// Register the mint on the watchlist; no capital.
    Watch,
    /// No-op (HOLD on an empty book, SKIP).
    NoOp,
}

/// The corpus's own management magnitude: `ADD` adds 50% of inventory, `REDUCE` trims 50%,
/// `EXIT` closes all. `None` for every other action.
pub fn management_fraction_bps(action: Action) -> Option<u32> {
    match action {
        Action::Add => Some(5_000),
        Action::Reduce => Some(5_000),
        Action::Exit => Some(10_000),
        _ => None,
    }
}

/// What a management action's magnitude is a FRACTION OF.
///
/// The corpus (the single label authority) sizes the two management families off
/// different bases, and conflating them is a ~4x error on the highest-stakes action:
///
/// - `ADD`    — the ACCOUNT's capital: `min(max(cash,0), add_fraction * capital_sol)`
/// - `REDUCE` — the CURRENT position:  `reduce_fraction * qty`
/// - `EXIT`   — the CURRENT position:  all of it
///
/// Worked example (0.25 SOL position, 0.76 cash, 1.01 account): the corpus ADD is
/// 0.505 SOL; sizing ADD off inventory gives 0.125 SOL — 4.04x smaller. A policy
/// cannot learn sizing if serving shrinks the bet it is graded on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagementBase {
    /// `ADD` — a fraction of the account's capital, capped by free cash.
    AccountCapital,
    /// `REDUCE`/`EXIT` — a fraction of the position actually held.
    Inventory,
}

/// The base [`management_fraction_bps`] is a fraction OF, per action.
///
/// `None` for actions that are not management magnitudes, mirroring
/// [`management_fraction_bps`].
#[must_use]
pub fn management_base(action: Action) -> Option<ManagementBase> {
    match action {
        Action::Add => Some(ManagementBase::AccountCapital),
        Action::Reduce | Action::Exit => Some(ManagementBase::Inventory),
        _ => None,
    }
}

/// Resolve the lamport magnitude of a management action the way the CORPUS does.
///
/// The serving-side counterpart of the label authority: given the same inputs the
/// simulator sees, it must return the same number — otherwise the policy is graded
/// on one bet and executes another. Integer-only and saturating.
///
/// Returns `None` for `BUY`/`WATCH`/`HOLD`/`SKIP`: those are not management
/// magnitudes, and inventing one would be the engine deciding the bet.
#[must_use]
pub fn resolve_management_clip_lamports(
    action: Action,
    inventory_value_lamports: u64,
    account_capital_lamports: u64,
    free_cash_lamports: u64,
) -> Option<u64> {
    let bps = u128::from(management_fraction_bps(action)?);
    let clip = match management_base(action)? {
        ManagementBase::AccountCapital => {
            let by_fraction = u128::from(account_capital_lamports) * bps / 10_000;
            // The corpus caps by the cash actually on hand (negative cash clamps to 0).
            by_fraction.min(u128::from(free_cash_lamports))
        }
        ManagementBase::Inventory => u128::from(inventory_value_lamports) * bps / 10_000,
    };
    Some(u64::try_from(clip).unwrap_or(u64::MAX))
}

/// Map an action to its lifecycle route.
pub fn route(action: Action) -> Route {
    match action {
        Action::Buy => Route::Open,
        Action::Add => Route::AddToPosition,
        Action::Reduce => Route::ReducePosition,
        Action::Exit => Route::ClosePosition,
        Action::Watch => Route::Watch,
        Action::Hold | Action::Skip => Route::NoOp,
    }
}

/// Why an entry clip could not be resolved. Every variant means **do not trade**.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeError {
    /// Free cash, after the fee buffer, cannot fund the smallest viable clip.
    InsufficientFreeCash,
    /// The resolved clip is below the venue/notional minimum.
    BelowMinimum,
    /// The inputs are not a usable market (non-finite or non-positive notional/cash).
    InvalidInput,
}

/// The canonical traded notional the tiers are fractions of (`exit_mechanics.
/// DEPLOY_SOL_CANONICAL`, 1 SOL), in lamports. A tier is **never** a fraction of the
/// account: the buffer stays free so the priority fee can be paid.
pub const DEPLOY_LAMPORTS_CANONICAL: u64 = 1_000_000_000;

/// Reserved so the priority fee can still be paid after the clip is deployed (0.05 SOL).
pub const FEE_BUFFER_LAMPORTS: u64 = 50_000_000;

/// Smallest clip worth sending (0.01 SOL).
pub const MIN_CLIP_LAMPORTS: u64 = 10_000_000;

/// Turn the model's size suggestion into the lamport clip production will actually deploy.
///
/// `tier x canonical notional`, capped by what the account can really fund
/// (`free_cash - fee_buffer`), and refused outright when even the smallest viable clip cannot
/// be funded. Capping and refusing are the whole point: the model decides *what* to trade and
/// how big it wants to be; the account decides whether that is payable. Integer lamports
/// throughout — a float here would be a rounding error in money.
pub fn resolve_entry_clip_lamports(
    tier: SizeTier,
    free_cash_lamports: u64,
    notional_lamports: u64,
    fee_buffer_lamports: u64,
) -> Result<u64, SizeError> {
    if notional_lamports == 0 {
        return Err(SizeError::InvalidInput);
    }
    let want = (u128::from(notional_lamports) * u128::from(tier.fraction_bps()) / 10_000) as u64;
    let payable = free_cash_lamports.saturating_sub(fee_buffer_lamports);
    if payable == 0 {
        return Err(SizeError::InsufficientFreeCash);
    }
    let clip = want.min(payable);
    if clip < MIN_CLIP_LAMPORTS {
        return Err(SizeError::BelowMinimum);
    }
    Ok(clip)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUY: &str = "DECISION: BUY\nSIZE: FULL\nPRICE LIMIT: 0.02445740498411998\n\
INVALIDATION: exit if net_flow_lamports turns negative or last_trade_age_s > 3.0\n\
EVIDENCE: round-trip cost floor 92 bp must be cleared; sustained outflow of -139374563544 \
lamports with 1146 buys vs 113 sells; top1 share 0.18098 (distributed)\n\
COUNTEREVIDENCE: single large seller can exhaust the book; memecoin reversals are common\n\
ENRICHMENT CITATION: holders_at_t=968 top1_float_share=0.682561 holder_hhi=0.566464 \
bundle_wallets=1024 wash_ratio=None creator_past_launches=0 creator_known=True\n\
SIZE_BASIS: creator_past_launches=0 holders_at_t=968 top1_float_share=0.682561 -> tier FULL \
(measured stratum expectancy +0.225 at lambda=0.0888)\nEVIDENCE_STATUS: complete";

    #[test]
    fn parses_a_real_corpus_buy() {
        let d = parse_decision_payload(BUY).unwrap();
        assert_eq!(d.action, Action::Buy);
        assert_eq!(d.size, Some(SizeTier::Full));
        assert_eq!(d.price_limit, Some(0.02445740498411998));
    }

    #[test]
    fn parses_every_tier_and_the_none_form() {
        for (tok, want) in [
            ("SMALL", SizeTier::Small),
            ("MID", SizeTier::Mid),
            ("FULL", SizeTier::Full),
        ] {
            let c = format!("DECISION: BUY\nSIZE: {tok}\nPRICE LIMIT: 1.5\n");
            assert_eq!(parse_decision_payload(&c).unwrap().size, Some(want));
        }
        let w = parse_decision_payload("DECISION: WATCH\nSIZE: NONE\nEVIDENCE: x").unwrap();
        assert_eq!(w.action, Action::Watch);
        assert_eq!(w.size, None);
    }

    #[test]
    fn a_buy_without_a_size_is_refused_not_defaulted() {
        assert!(parse_decision_payload("DECISION: BUY\nPRICE LIMIT: 1.0").is_err());
        assert!(parse_decision_payload("DECISION: BUY\nSIZE: NONE\nPRICE LIMIT: 1.0").is_err());
        assert!(parse_decision_payload("DECISION: BUY\nSIZE: HUGE\nPRICE LIMIT: 1.0").is_err());
        assert!(parse_decision_payload("DECISION: BUY\nSIZE: FULL\nPRICE LIMIT: 0").is_err());
        assert!(parse_decision_payload("DECISION: BUY\nSIZE: FULL\nPRICE LIMIT: abc").is_err());
    }

    #[test]
    fn a_price_limit_is_optional_because_68pct_of_trained_buys_lack_one() {
        let d = parse_decision_payload("DECISION: BUY\nSIZE: FULL\nINVALIDATION: x").unwrap();
        assert_eq!(d.size, Some(SizeTier::Full));
        assert_eq!(d.price_limit, None);
    }

    #[test]
    fn a_size_on_a_non_buy_is_refused() {
        assert!(parse_decision_payload("DECISION: HOLD\nSIZE: FULL\nPOSITION: x").is_err());
        assert!(parse_decision_payload("DECISION: SELL\nSIZE: NONE").is_err());
    }

    #[test]
    fn management_actions_parse_with_their_corpus_magnitudes() {
        let add = "DECISION: ADD\nPOSITION: entry 0.023292756397 -> mark 0.0245422000546 \
(536.4 bp), held 120s\nEVIDENCE: unrealized 536.4 bp; MFE 19351.6 bp\n\
COUNTEREVIDENCE: a single observed path is one sample and may not repeat\n\
INVALIDATION: execution cost: round trip 66 bp\nEVIDENCE_STATUS: complete";
        assert_eq!(parse_decision_payload(add).unwrap().action, Action::Add);
        assert_eq!(management_fraction_bps(Action::Add), Some(5_000));
        assert_eq!(management_fraction_bps(Action::Reduce), Some(5_000));
        assert_eq!(management_fraction_bps(Action::Exit), Some(10_000));
        assert_eq!(management_fraction_bps(Action::Buy), None);
    }

    #[test]
    fn every_action_has_a_route_and_sell_is_not_an_action() {
        for a in [
            Action::Buy,
            Action::Watch,
            Action::Skip,
            Action::Hold,
            Action::Add,
            Action::Reduce,
            Action::Exit,
        ] {
            let _ = route(a);
        }
        assert_eq!(route(Action::Buy), Route::Open);
        assert_eq!(route(Action::Add), Route::AddToPosition);
        assert_eq!(route(Action::Reduce), Route::ReducePosition);
        assert_eq!(route(Action::Exit), Route::ClosePosition);
        assert_eq!(route(Action::Watch), Route::Watch);
        assert_eq!(route(Action::Skip), Route::NoOp);
        assert_eq!(Action::parse("SELL"), None);
    }

    #[test]
    fn a_tier_is_a_fraction_of_notional_never_of_the_account() {
        let one = DEPLOY_LAMPORTS_CANONICAL;
        assert_eq!(
            resolve_entry_clip_lamports(SizeTier::Full, 10 * one, one, FEE_BUFFER_LAMPORTS)
                .unwrap(),
            one
        );
        assert_eq!(
            resolve_entry_clip_lamports(SizeTier::Small, 10 * one, one, FEE_BUFFER_LAMPORTS)
                .unwrap(),
            250_000_000
        );
        assert_eq!(
            resolve_entry_clip_lamports(SizeTier::Mid, 10 * one, one, FEE_BUFFER_LAMPORTS).unwrap(),
            500_000_000
        );
    }

    #[test]
    fn a_clip_is_capped_by_free_cash_and_refused_when_unpayable() {
        let one = DEPLOY_LAMPORTS_CANONICAL;
        // 0.60 SOL free, 0.05 buffer -> 0.55 payable, so FULL (1.0) caps to 0.55
        assert_eq!(
            resolve_entry_clip_lamports(SizeTier::Full, 600_000_000, one, FEE_BUFFER_LAMPORTS)
                .unwrap(),
            550_000_000
        );
        // exactly the buffer: nothing is payable
        assert_eq!(
            resolve_entry_clip_lamports(
                SizeTier::Full,
                FEE_BUFFER_LAMPORTS,
                one,
                FEE_BUFFER_LAMPORTS
            ),
            Err(SizeError::InsufficientFreeCash)
        );
        // payable, but below the minimum viable clip
        assert_eq!(
            resolve_entry_clip_lamports(SizeTier::Small, 55_000_000, one, FEE_BUFFER_LAMPORTS),
            Err(SizeError::BelowMinimum)
        );
        // a zero notional is not a market
        assert_eq!(
            resolve_entry_clip_lamports(SizeTier::Full, one, 0, FEE_BUFFER_LAMPORTS),
            Err(SizeError::InvalidInput)
        );
    }

    #[test]
    fn the_drift_ledger_counts_by_cause_and_alarms_on_rate() {
        let mut l = DriftLedger::new();
        for _ in 0..1_000 {
            l.record_accepted();
        }
        assert!(!l.alarm(100, 10), "clean traffic must not alarm");
        l.record(OffContract::BuyWithoutSize);
        assert!(l.buy_without_size_seen());
        assert_eq!(l.total(), 1);
        assert_eq!(l.count(OffContract::BuyWithoutSize), 1);
        assert_eq!(l.rate_bp(), 10);
        assert!(l.alarm(1_000, 5));
        assert!(
            !l.alarm(1_001, 5),
            "one short of the sample floor must stay quiet"
        );
    }

    #[test]
    fn cold_start_never_alarms_on_a_single_bad_completion() {
        let mut l = DriftLedger::new();
        l.record(OffContract::NoDecisionLine);
        assert_eq!(l.rate_bp(), 0, "no denominator yet");
        assert!(!l.alarm(100, 10));
    }

    #[test]
    fn every_off_contract_cause_is_reachable_from_the_parser() {
        let t = |s: &str| parse_decision_payload(s).unwrap_err().kind;
        assert_eq!(t("garbage"), OffContract::NoDecisionLine);
        assert_eq!(t("DECISION: SELL"), OffContract::UntrainedAction);
        assert_eq!(t("DECISION: BUY"), OffContract::BuyWithoutSize);
        assert_eq!(
            t("DECISION: BUY\nSIZE: NONE"),
            OffContract::BuyWithUntrainedSize
        );
        assert_eq!(
            t("DECISION: BUY\nSIZE: FULL\nPRICE LIMIT: 0"),
            OffContract::BuyWithUnusablePriceLimit
        );
        assert_eq!(t("DECISION: HOLD\nSIZE: FULL"), OffContract::SizeOnNonBuy);
        for k in OffContract::ALL {
            assert!(!k.as_str().is_empty());
        }
    }

    #[test]
    fn headline_fires_non_buy_on_the_action_alone_and_waits_for_buy_size() {
        // non-BUY: the action token alone is enough to route, so it fires immediately
        assert_eq!(
            parse_headline("DECISION: WATCH").unwrap().unwrap(),
            Headline {
                action: Action::Watch,
                size: None
            }
        );
        assert_eq!(
            parse_headline("DECISION: EXIT").unwrap().unwrap(),
            Headline {
                action: Action::Exit,
                size: None
            }
        );
        // BUY: the size is required before the headline is complete
        assert!(parse_headline("DECISION: BUY").is_none());
    }

    #[test]
    fn headline_streams_against_partial_text() {
        // no DECISION line yet
        assert!(parse_headline("").is_none());
        assert!(parse_headline("DECIS").is_none());
        // a leading blank line / indentation does not break the scan
        assert_eq!(
            parse_headline("\n  DECISION: BUY\nSIZE: FULL")
                .unwrap()
                .unwrap(),
            Headline {
                action: Action::Buy,
                size: Some(SizeTier::Full)
            }
        );
        // an untrained action errors immediately (nothing to wait for)
        assert_eq!(
            parse_headline("DECISION: SELL").unwrap().unwrap_err(),
            OffContract::UntrainedAction
        );
        // an untrained size errors immediately
        assert_eq!(
            parse_headline("DECISION: BUY\nSIZE: HUGE")
                .unwrap()
                .unwrap_err(),
            OffContract::BuyWithUntrainedSize
        );
    }

    #[test]
    fn headline_never_waits_for_the_optional_price_limit_or_reasoning() {
        // The execution path reads only action + size; the optional PRICE LIMIT and the
        // reasoning lines are journal-only, so a BUY headline is complete without them.
        let hl = parse_headline("DECISION: BUY\nSIZE: SMALL")
            .unwrap()
            .unwrap();
        assert_eq!(
            hl,
            Headline {
                action: Action::Buy,
                size: Some(SizeTier::Small)
            }
        );
    }
}
