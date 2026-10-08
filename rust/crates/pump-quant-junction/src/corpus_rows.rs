//! Corpus-definition trade rows from PROCESSED transaction metadata.
//!
//! # Why this exists
//! The trained flow features (`net_flow_sol_300s`, `smart_net_flow_sol_300s`, `bot_uniform_share_300s`, the
//! price and volume of the state ledger) are computed from the corpus tape (`renormalize_raw.py`), whose
//! `sol_lamports` is the TRADER'S WHOLE-TRANSACTION balance delta (native + WSOL token leg), resolved per
//! corpus-known buy/sell instruction by a sign-pattern rule. It is NOT the swap quantity in the TradeEvent.
//! This module ports that resolver so live rows carry the trained quantity; the swap quantity is retained
//! separately by `curve_trade_events` for quotes/accounting. The two are never interchanged.
//!
//! Inputs are the transaction's static+loaded account keys, SOL balances and token balances: all in the
//! processed-commitment `meta`, so no confirmed-only data is needed.
//!
//! Frozen definition (mirrors `training/code/src/v2/renormalize_raw.py`, the script that produced
//! `renormalized_v7`; verified byte-identical on 265,312 session-1 signatures):
//! * only SUCCESSFUL transactions; instruction list = outer instructions then each inner group, flattened;
//! * a row per pump.fun buy/sell instruction whose 8-byte discriminator is in the corpus table;
//! * trader = an account in the instruction's first 20 accounts whose (native+WSOL) delta and mint token delta
//!   have the buy/sell sign pattern, preferring account index 6; else a whole-transaction `net_position` search
//!   that must conserve the mint's token total (|sum| <= max(1, |best token delta| / 1000));
//! * `sol` = that account's (native + WSOL) delta; rows with zero sol or zero token delta are dropped.

use std::collections::HashSet;

/// One token-balance entry from meta (pre or post), in wire order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokBal {
    pub mint: [u8; 32],
    pub owner: [u8; 32],
    pub amount: u128,
}

/// Everything the resolver needs from one transaction.
#[derive(Clone, Debug)]
pub struct BalanceMeta {
    pub pre_sol: Vec<u64>,
    pub post_sol: Vec<u64>,
    pub pre_tok: Vec<TokBal>,
    pub post_tok: Vec<TokBal>,
}

/// pump.fun buy discriminators in the corpus table.
const BUY_DISCS: [[u8; 8]; 3] = [
    [102, 6, 61, 18, 1, 218, 235, 234],
    [184, 23, 238, 97, 103, 197, 211, 61],
    [56, 252, 116, 8, 158, 223, 205, 95],
];
/// pump.fun sell discriminators in the corpus table.
const SELL_DISCS: [[u8; 8]; 2] = [
    [51, 230, 133, 164, 1, 127, 131, 173],
    [93, 246, 130, 60, 231, 233, 64, 178],
];
/// Account index of the user in a pump.fun buy/sell instruction (tie-break only).
/// Preferred trader account index of a pump.fun buy/sell (the corpus has none for PumpSwap: `SWAP_TRADER_IX = None`).
pub const PUMP_FUN_TRADER_IX: usize = 6;
const WSOL: [u8; 32] = [
    6, 155, 136, 87, 254, 171, 129, 132, 251, 104, 127, 99, 70, 24, 192, 53, 218, 196, 57, 220, 26,
    235, 59, 85, 152, 160, 240, 0, 0, 0, 0, 1,
];

/// PumpSwap buy discriminators in the corpus table (`buy`, `buy_exact_quote_in`).
const SWAP_BUY_DISCS: [[u8; 8]; 2] = [
    [102, 6, 61, 18, 1, 218, 235, 234],
    [198, 46, 21, 82, 180, 217, 232, 112],
];
/// PumpSwap sell discriminator in the corpus table.
const SWAP_SELL_DISCS: [[u8; 8]; 1] = [[51, 230, 133, 164, 1, 127, 131, 173]];

/// `Some(is_buy)` when `data` starts with a corpus-known PumpSwap buy/sell discriminator.
#[must_use]
pub fn corpus_swap_side(data: &[u8]) -> Option<bool> {
    let d = data.get(..8)?;
    if SWAP_BUY_DISCS.iter().any(|x| x[..] == *d) {
        Some(true)
    } else if SWAP_SELL_DISCS.iter().any(|x| x[..] == *d) {
        Some(false)
    } else {
        None
    }
}

/// `Some(is_buy)` when `data` starts with a corpus-known pump.fun buy/sell discriminator.
#[must_use]
pub fn corpus_side(data: &[u8]) -> Option<bool> {
    let d = data.get(..8)?;
    if BUY_DISCS.iter().any(|x| x[..] == *d) {
        Some(true)
    } else if SELL_DISCS.iter().any(|x| x[..] == *d) {
        Some(false)
    } else {
        None
    }
}

/// A resolved corpus row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CorpusRow {
    pub mint: [u8; 32],
    pub trader: [u8; 32],
    pub is_buy: bool,
    /// Trader's native + WSOL balance delta (buy negative, sell positive), exactly the tape's `sol_lamports`.
    pub sol_lamports: i64,
    /// Trader's token delta for `mint` (the tape's `tokens_raw`).
    pub tokens_raw: i128,
    /// 0 = resolved from the instruction's own accounts, 1 = whole-transaction net-position fallback.
    pub via_net_position: bool,
}

fn first_amt(entries: &[TokBal], owner: &[u8; 32], mint: &[u8; 32]) -> i128 {
    // The corpus takes the FIRST matching entry (not a sum): replicated, not "improved".
    entries
        .iter()
        .find(|e| e.owner == *owner && e.mint == *mint)
        .map_or(0, |e| i128::try_from(e.amount).unwrap_or(i128::MAX))
}

fn deltas(m: &BalanceMeta, ai: usize, owner: &[u8; 32], mint: &[u8; 32]) -> Option<(i128, i128)> {
    let pre = i128::from(*m.pre_sol.get(ai)?);
    let post = i128::from(*m.post_sol.get(ai)?);
    // Token amounts are u64 lifted to i128 and SOL balances are u64: these differences cannot overflow i128.
    let wsol =
        first_amt(&m.post_tok, owner, &WSOL).checked_sub(first_amt(&m.pre_tok, owner, &WSOL))?;
    let sd = post.checked_sub(pre)?.checked_add(wsol)?;
    let td = first_amt(&m.post_tok, owner, mint).checked_sub(first_amt(&m.pre_tok, owner, mint))?;
    Some((sd, td))
}

fn pattern_ok(is_buy: bool, sd: i128, td: i128) -> bool {
    if is_buy {
        sd < 0 && td > 0
    } else {
        sd > 0 && td < 0
    }
}

/// Mint candidates in the corpus's order: post-balance mints then pre-balance mints, deduplicated, with
/// the corpus's NOT_A_LAUNCH set (WSOL, system program, token programs) removed.
fn candidate_mints(m: &BalanceMeta, not_launch: &HashSet<[u8; 32]>) -> Vec<[u8; 32]> {
    let mut out: Vec<[u8; 32]> = Vec::new();
    for e in m.post_tok.iter().chain(m.pre_tok.iter()) {
        if !not_launch.contains(&e.mint) && !out.contains(&e.mint) {
            out.push(e.mint);
        }
    }
    out
}

/// Resolve the corpus row for ONE corpus-known pump.fun instruction. `ix_accounts` are the instruction's account
/// indices into `keys`. `None` = the corpus would have rejected it (counted by the caller, never guessed).
pub fn resolve_row_why(
    prefer_ix: Option<usize>,
    is_buy: bool,
    ix_accounts: &[u8],
    keys: &[[u8; 32]],
    invalid_key_idx: &[usize],
    m: &BalanceMeta,
    not_launch: &HashSet<[u8; 32]>,
) -> Result<CorpusRow, RowReject> {
    if m.pre_sol.len() != m.post_sol.len() {
        return Err(RowReject::BalanceLenMismatch);
    }
    let mints = candidate_mints(m, not_launch);
    if mints.is_empty() {
        return Err(RowReject::NoCandidateMint);
    }
    let mut found: Option<([u8; 32], [u8; 32], i128, bool)> = None; // (mint, owner, sd, via_net)
    let mut saw_wide_hits = false;
    let mut conservation_failed = false;
    for mint in &mints {
        // ---- instruction-scoped search (resolve_trader)
        let mut cands: Vec<(usize, [u8; 32], i128, i128)> = Vec::new();
        for &a in ix_accounts.iter().take(20) {
            let ai = usize::from(a);
            if ai >= keys.len() || ai >= m.pre_sol.len() || invalid_key_idx.contains(&ai) {
                continue;
            }
            let ow = keys[ai];
            if let Some((sd, td)) = deltas(m, ai, &ow, mint) {
                if pattern_ok(is_buy, sd, td) {
                    cands.push((ai, ow, td, sd));
                }
            }
        }
        if !cands.is_empty() {
            let mut pick = None;
            if let Some(&want) = prefer_ix.and_then(|i| ix_accounts.get(i)) {
                pick = cands.iter().find(|c| c.0 == usize::from(want)).copied();
            }
            let best = pick.unwrap_or_else(|| {
                // `max(cands, key=|td|)`: first maximum wins, like Python's max().
                let mut b = cands[0];
                for c in &cands[1..] {
                    if c.2.unsigned_abs() > b.2.unsigned_abs() {
                        b = *c;
                    }
                }
                b
            });
            found = Some((*mint, best.1, best.3, false));
            break;
        }
    }
    if found.is_none() {
        // ---- whole-transaction net-position fallback (wide_resolve), per candidate mint
        for mint in &mints {
            let mut hits: Vec<(usize, [u8; 32], i128, i128)> = Vec::new();
            let n = keys.len().min(m.pre_sol.len()).min(m.post_sol.len());
            for (j, ow) in keys.iter().copied().enumerate().take(n) {
                if invalid_key_idx.contains(&j) {
                    continue; // an unparseable key is never an owner, even though its placeholder bytes are zero
                }
                if let Some((sd, td)) = deltas(m, j, &ow, mint) {
                    if pattern_ok(is_buy, sd, td) {
                        hits.push((j, ow, td, sd));
                    }
                }
            }
            if hits.is_empty() {
                continue;
            }
            saw_wide_hits = true;
            let mut owners: Vec<[u8; 32]> = Vec::new();
            for e in m.pre_tok.iter().chain(m.post_tok.iter()) {
                if e.mint == *mint && !owners.contains(&e.owner) {
                    owners.push(e.owner);
                }
            }
            let tot: i128 = owners
                .iter()
                .map(|o| {
                    first_amt(&m.post_tok, o, mint).saturating_sub(first_amt(&m.pre_tok, o, mint))
                })
                .fold(0i128, i128::saturating_add);
            let mut best = hits[0];
            for h in &hits[1..] {
                if h.2.unsigned_abs() > best.2.unsigned_abs() {
                    best = *h;
                }
            }
            if tot.unsigned_abs() > std::cmp::max(1, best.2.unsigned_abs() / 1000) {
                conservation_failed = true;
                continue; // corpus: `return None` for THIS mint, then the next mint is tried
            }
            found = Some((*mint, best.1, best.3, true));
            break;
        }
    }
    let Some((mint, owner, sd, via)) = found else {
        return Err(if conservation_failed {
            RowReject::WideConservationFailed
        } else {
            // With or without wide hits the corpus names the same reason.
            let _ = saw_wide_hits;
            RowReject::NoSignPatternAnywhere
        });
    };
    let tok =
        first_amt(&m.post_tok, &owner, &mint).saturating_sub(first_amt(&m.pre_tok, &owner, &mint));
    if sd == 0 || tok == 0 {
        return Err(RowReject::ZeroSolOrToken);
    }
    let Ok(sol_lamports) = i64::try_from(sd) else {
        return Err(RowReject::SolOverflow);
    };
    Ok(CorpusRow {
        mint,
        trader: owner,
        is_buy,
        sol_lamports,
        tokens_raw: tok,
        via_net_position: via,
    })
}

/// Why the frozen resolver produced no row. Each variant is a rule of `renormalize_raw.py`, not a new filter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RowReject {
    /// pre/post SOL balance vectors differ in length.
    BalanceLenMismatch,
    /// No token mint outside WSOL/system/token programs in the transaction's token balances.
    NoCandidateMint,
    /// No account (instruction-scoped, then whole transaction) shows the buy/sell sign pattern for any candidate mint.
    NoSignPatternAnywhere,
    /// Whole-transaction candidates existed but the mint's token total did not conserve (vault/hop, not a swap).
    WideConservationFailed,
    /// Resolved trader has a zero SOL or zero token delta (the corpus drops it).
    ZeroSolOrToken,
    /// SOL delta does not fit i64.
    SolOverflow,
}

impl RowReject {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BalanceLenMismatch => "balance_len_mismatch",
            Self::NoCandidateMint => "no_candidate_mint",
            Self::NoSignPatternAnywhere => "no_sign_pattern_anywhere",
            Self::WideConservationFailed => "wide_conservation_failed",
            Self::ZeroSolOrToken => "zero_sol_or_token",
            Self::SolOverflow => "sol_overflow",
        }
    }
}

/// [`resolve_row_why`] without the reason.
#[must_use]
pub fn resolve_row(
    trader_ix: Option<usize>,
    is_buy: bool,
    ix_accounts: &[u8],
    keys: &[[u8; 32]],
    invalid_key_idx: &[usize],
    m: &BalanceMeta,
    not_launch: &HashSet<[u8; 32]>,
) -> Option<CorpusRow> {
    resolve_row_why(
        trader_ix,
        is_buy,
        ix_accounts,
        keys,
        invalid_key_idx,
        m,
        not_launch,
    )
    .ok()
}

/// The corpus's NOT_A_LAUNCH mint set (WSOL, system program, SPL token, Token-2022).
#[must_use]
pub fn not_a_launch_set() -> HashSet<[u8; 32]> {
    let mut s = HashSet::new();
    s.insert(WSOL);
    s.insert([0u8; 32]); // 1111...1 (system program)
    s.insert(b58_32("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"));
    s.insert(b58_32("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"));
    s
}

/// Base58 (bitcoin alphabet) -> 32 bytes; all-zero on malformed input (constants only).
fn b58_32(s: &str) -> [u8; 32] {
    use solana_program::pubkey::Pubkey;
    use std::str::FromStr;
    Pubkey::from_str(s)
        .map(|p| p.to_bytes())
        .unwrap_or([0u8; 32])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(b: u8) -> [u8; 32] {
        [b; 32]
    }
    const MINT: u8 = 50;
    const TRADER: u8 = 7;
    const POOL: u8 = 8;

    fn meta(
        pre_sol: Vec<u64>,
        post_sol: Vec<u64>,
        pre: Vec<TokBal>,
        post: Vec<TokBal>,
    ) -> BalanceMeta {
        BalanceMeta {
            pre_sol,
            post_sol,
            pre_tok: pre,
            post_tok: post,
        }
    }
    fn tb(mint: [u8; 32], owner: [u8; 32], amount: u128) -> TokBal {
        TokBal {
            mint,
            owner,
            amount,
        }
    }

    /// keys: 0..=9 ; trader is key 7; buy: trader pays 100 lamports (incl. 5 extra non-swap) and gains tokens.
    #[test]
    fn sol_is_the_traders_whole_transaction_balance_delta_not_the_swap_amount() {
        let keys: Vec<[u8; 32]> = (0u8..10).map(k).collect();
        let m = meta(
            vec![1000; 10],
            vec![1000, 1000, 1000, 1000, 1000, 1000, 1000, 895, 1090, 1000],
            vec![tb(k(MINT), k(TRADER), 0), tb(k(MINT), k(POOL), 500)],
            vec![tb(k(MINT), k(TRADER), 200), tb(k(MINT), k(POOL), 300)],
        );
        let r = resolve_row(
            Some(PUMP_FUN_TRADER_IX),
            true,
            &[0, 1, 2, 3, 4, 5, 7],
            &keys,
            &[],
            &m,
            &not_a_launch_set(),
        )
        .unwrap();
        assert_eq!(r.trader, k(TRADER));
        assert_eq!(
            r.sol_lamports, -105,
            "balance delta (incl. rent/fees), NOT a swap quantity"
        );
        assert_eq!(r.tokens_raw, 200);
        assert!(!r.via_net_position);
    }

    /// Router: the real trader is NOT in the instruction's accounts -> whole-tx net_position, and only when the
    /// mint's token total is conserved.
    #[test]
    fn router_attribution_uses_net_position_only_when_tokens_conserve() {
        let keys: Vec<[u8; 32]> = (0u8..10).map(k).collect();
        let pre = vec![tb(k(MINT), k(TRADER), 0), tb(k(MINT), k(POOL), 500)];
        let post_ok = vec![tb(k(MINT), k(TRADER), 200), tb(k(MINT), k(POOL), 300)];
        let post_leak = vec![tb(k(MINT), k(TRADER), 200), tb(k(MINT), k(POOL), 100)];
        let sol_post = vec![1000, 1000, 1000, 1000, 1000, 1000, 1000, 900, 1100, 1000];
        let m_ok = meta(vec![1000; 10], sol_post.clone(), pre.clone(), post_ok);
        let r = resolve_row(
            Some(PUMP_FUN_TRADER_IX),
            true,
            &[0, 1],
            &keys,
            &[],
            &m_ok,
            &not_a_launch_set(),
        )
        .unwrap();
        assert!(r.via_net_position);
        assert_eq!(r.trader, k(TRADER));
        let m_leak = meta(vec![1000; 10], sol_post, pre, post_leak);
        assert!(
            resolve_row(
                Some(PUMP_FUN_TRADER_IX),
                true,
                &[0, 1],
                &keys,
                &[],
                &m_leak,
                &not_a_launch_set()
            )
            .is_none(),
            "non-conserving token totals are rejected, as the corpus rejects them"
        );
    }

    #[test]
    fn corpus_side_knows_only_the_corpus_discriminators() {
        assert_eq!(
            corpus_side(&[102, 6, 61, 18, 1, 218, 235, 234, 0]),
            Some(true)
        );
        assert_eq!(
            corpus_side(&[93, 246, 130, 60, 231, 233, 64, 178]),
            Some(false)
        );
        // BuyExactQuoteInV2 (c2ab1c46684d5b2f) is NOT in the corpus table: the tape never counted it.
        assert_eq!(
            corpus_side(&[0xc2, 0xab, 0x1c, 0x46, 0x68, 0x4d, 0x5b, 0x2f]),
            None
        );
        assert_eq!(corpus_side(&[1, 2, 3]), None);
    }

    #[test]
    fn mismatched_balance_arrays_and_zero_deltas_are_rejected() {
        let keys: Vec<[u8; 32]> = (0u8..10).map(k).collect();
        let m = meta(vec![1000; 10], vec![1000; 9], vec![], vec![]);
        assert!(resolve_row(
            Some(PUMP_FUN_TRADER_IX),
            true,
            &[7],
            &keys,
            &[],
            &m,
            &not_a_launch_set()
        )
        .is_none());
    }
}
