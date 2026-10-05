#!/usr/bin/env python3
"""Re-normalize RAW LaserStream captures into corrected causal events.

Why this exists: the compiled encoder on this box is stale in two ways --
its discriminator table lacks the V2 entrypoints (create_v2 / buy_v2 /
sell_v2 / migrate_v2 / complete_v2), and its `account_keys_b58` omits
address-table-lookup keys, so indices into pre/post balance arrays are
misaligned. The consequence was that launches and V2 trades fell into
`unknown_events`, and the SOL leg of a swap could not be recovered.

The raw capture is the preserved ground truth, so we can re-derive correct
events from it without rebuilding Rust.

Both legs of a trade come from balance deltas (never from a single-sided
`amount_in`/`amount_out` bound, which is NOT the SOL leg -- measured
agreement with the balance delta was 1.4%).

Read-only over the source files.
"""
import base64
import json
import subprocess
import sys
from pathlib import Path

PUMP_FUN = '6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P'
PUMP_SWAP = 'pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA'

# sha256("global:<name>")[..8]; verified against captured instruction bytes.
DISC = {
    (PUMP_FUN, bytes([102, 6, 61, 18, 1, 218, 235, 234])): ('buy', 'buy'),
    (PUMP_FUN, bytes([51, 230, 133, 164, 1, 127, 131, 173])): ('sell', 'sell'),
    (PUMP_FUN, bytes([24, 30, 200, 40, 5, 28, 7, 119])): ('create', None),
    (PUMP_FUN, bytes([0, 77, 224, 147, 136, 25, 88, 76])): ('complete', None),
    (PUMP_FUN, bytes([155, 234, 231, 146, 236, 158, 162, 30])): ('migrate', None),
    (PUMP_FUN, bytes([214, 144, 76, 236, 95, 139, 49, 180])): ('create', None),
    (PUMP_FUN, bytes([184, 23, 238, 97, 103, 197, 211, 61])): ('buy', 'buy'),
    (PUMP_FUN, bytes([93, 246, 130, 60, 231, 233, 64, 178])): ('sell', 'sell'),
    (PUMP_FUN, bytes([187, 203, 18, 31, 206, 237, 254, 41])): ('migrate', None),
    (PUMP_FUN, bytes([77, 202, 226, 2, 186, 4, 169, 18])): ('complete', None),
    (PUMP_SWAP, bytes([102, 6, 61, 18, 1, 218, 235, 234])): ('buy', 'buy'),
    (PUMP_SWAP, bytes([51, 230, 133, 164, 1, 127, 131, 173])): ('sell', 'sell'),
    # "exact in" buy paths. Both programs have one and neither was in the table,
    # so these buys were dropped as unclassified -- in one sample
    # buy_exact_sol_in was the single most common pump.fun instruction seen.
    (PUMP_FUN, bytes([56, 252, 116, 8, 158, 223, 205, 95])): ('buy', 'buy'),
    (PUMP_SWAP, bytes([198, 46, 21, 82, 180, 217, 232, 112])): ('buy', 'buy'),
    # Creator-fee distribution: not a swap, recorded so it is not miscounted.
    (PUMP_FUN, bytes([165, 114, 103, 0, 121, 206, 247, 81])): ('fee', None),
    (PUMP_SWAP, bytes([233, 146, 209, 142, 207, 104, 64, 188])): ('create_pool', None),
    (PUMP_SWAP, bytes([242, 35, 198, 137, 82, 225, 242, 182])): ('deposit', None),
    (PUMP_SWAP, bytes([183, 18, 70, 156, 148, 109, 161, 34])): ('withdraw', None),
}

NOT_A_LAUNCH = {
    'So11111111111111111111111111111111111111112',
    '11111111111111111111111111111111',
    'TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA',
    'TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb',
}


# Known account-layout position of the swapper, used only as a tie-break once
# the sign pattern has already validated the candidate. pump.fun places the user
# at index 6; pumpswap's layout is left unset so the sign+size test decides.
PUMP_FUN_TRADER_IX = 6
SWAP_TRADER_IX = None

# Wrapped SOL. On AMM venues the quote leg settles as this TOKEN rather than as
# native lamports, so the value side of a swap must include it.
WSOL = 'So11111111111111111111111111111111111111112'


def stream(path):
    p = subprocess.Popen(['zstd', '-dc', str(path)],
                         stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    for line in p.stdout:
        if len(line) < 3:
            continue
        try:
            yield json.loads(line)
        except Exception:
            continue
    p.stdout.close()
    p.wait()


def full_keys(msg, meta):
    keys = list(msg.get('account_keys_b58') or [])
    keys += list(meta.get('loaded_writable_addresses_b58') or [])
    keys += list(meta.get('loaded_readonly_addresses_b58') or [])
    return keys


def token_delta(entries_pre, entries_post, owner, mint):
    """Raw token delta for (owner, mint) from pre/post token balance lists."""
    def amt(entries):
        for e in entries or []:
            if e.get('owner') == owner and e.get('mint') == mint:
                try:
                    return int((e.get('ui_token_amount') or {}).get('amount') or 0)
                except Exception:
                    return 0
        return 0
    return amt(entries_post) - amt(entries_pre)


def find_token_mover(entries_pre, entries_post, mint, prefer=None):
    """Owner whose token balance for `mint` changed.

    The trader must be identified from the token leg, not assumed to be
    keys[0]. In a swap two accounts move -- the trader and the pool -- so when
    both move we prefer the fee payer (`prefer`) if it is one of them, else the
    smaller-magnitude mover (the pool position is typically the larger one).

    Returns None when nothing moved, which means this is not a real swap.
    """
    def amt(entries, owner):
        for e in entries or []:
            if e.get('owner') == owner and e.get('mint') == mint:
                try:
                    return int((e.get('ui_token_amount') or {}).get('amount') or 0)
                except Exception:
                    return 0
        return 0

    owners = set()
    for e in list(entries_pre or []) + list(entries_post or []):
        if e.get('mint') == mint and e.get('owner'):
            owners.add(e.get('owner'))

    movers = {}
    for o in owners:
        d = amt(entries_post, o) - amt(entries_pre, o)
        if d != 0:
            movers[o] = d
    if not movers:
        return None
    if prefer in movers:
        return prefer
    return min(movers.items(), key=lambda kv: abs(kv[1]))[0]


def _tok_amt(entries, owner, mint):
    for e in entries or []:
        if e.get('owner') == owner and e.get('mint') == mint:
            try:
                return int((e.get('ui_token_amount') or {}).get('amount') or 0)
            except Exception:
                return 0
    return 0


def resolve_trader(accts, keys, side, mint, pre_sol, post_sol, pre_tok, post_tok,
                   prefer_idx=None):
    """The account in THIS instruction that actually performed the swap.

    A real swapper has an unambiguous sign pattern:
        buy  -> SOL delta < 0 (pays)     and token delta > 0 (receives)
        sell -> SOL delta > 0 (receives) and token delta < 0 (gives)

    Fee recipients, LP/pool accounts and rent destinations fail one or both
    legs, so requiring the pattern excludes them structurally rather than by a
    proximity heuristic. Among survivors the known layout index is preferred;
    otherwise the largest token move wins, because a fee skim is always smaller
    than the swap that generated it.
    """
    if len(pre_sol) != len(post_sol):
        return None
    cands = []
    for ai in accts[:20]:
        if ai >= len(keys) or ai >= len(pre_sol):
            continue
        ow = keys[ai]
        # pumpswap's quote asset is WSOL -- a TOKEN, not native SOL -- so the
        # swapper's native balance frequently does not move at all (the value
        # settles in a wrapped-SOL token account). Summing both measures the
        # value leg wherever it actually landed; native-only made the sign test
        # fail on the majority of pumpswap swaps.
        sd = (post_sol[ai] - pre_sol[ai]) + (
            _tok_amt(post_tok, ow, WSOL) - _tok_amt(pre_tok, ow, WSOL))
        td = _tok_amt(post_tok, ow, mint) - _tok_amt(pre_tok, ow, mint)
        if side == 'buy' and sd < 0 and td > 0:
            cands.append((ai, ow, td, sd))
        elif side == 'sell' and sd > 0 and td < 0:
            cands.append((ai, ow, td, sd))
    if not cands:
        return None
    if prefer_idx is not None and prefer_idx < len(accts):
        want = accts[prefer_idx]
        for ai, ow, _td, sd in cands:
            if ai == want:
                return ow, sd
    best = max(cands, key=lambda c: abs(c[2]))
    return best[1], best[3]


def resolve_pool(keys, side, mint, pre_sol, post_sol, pre_tok, post_tok):
    """Pool/vault side of the swap, from the POOL'S OWN balance deltas.

    The trader's SOL delta spans intermediate hops on router routes, which
    makes trader-derived prices meaningless there. The pool's own legs cannot
    be obscured by routing: tokens and value settle at the vaults. The pool
    takes the opposite side of the trader, so on a buy it gains SOL and loses
    tokens; on a sell the reverse. The largest such token move is the pool.

    Returns (pool_owner, token_delta_signed, sol_delta_signed) or None.
    """
    if len(pre_sol) != len(post_sol):
        return None
    cands = []
    for j in range(min(len(keys), len(pre_sol), len(post_sol))):
        ow = keys[j]
        sd = ((post_sol[j] - pre_sol[j])
              + (_tok_amt(post_tok, ow, WSOL) - _tok_amt(pre_tok, ow, WSOL)))
        td = _tok_amt(post_tok, ow, mint) - _tok_amt(pre_tok, ow, mint)
        if side == 'buy' and td < 0 and sd > 0:
            cands.append((ow, td, sd))
        elif side == 'sell' and td > 0 and sd < 0:
            cands.append((ow, td, sd))
    if not cands:
        return None
    return max(cands, key=lambda c: abs(c[1]))


def wide_resolve(keys, side, mint, pre_sol, post_sol, pre_tok, post_tok):
    """Net-position fallback: search EVERY account in the transaction.

    On an aggregator/ router route the economic actor can sit outside the swap
    instruction's account list, so the instruction-scoped search fails even
    though the trade is perfectly well formed. Measured on a 100-file sample
    that lost 188,230 swaps: this recovers 79% of them, 99.6% of which pass the
    conservation test below.

    Tokens are neither created nor destroyed in a swap, so the sum of every
    account's delta for the mint must net to zero. A candidate whose counterpart
    set does not conserve is not a real trade (it is usually a vault or an
    intermediate hop), so it is rejected rather than guessed at.

    Returns (owner, value_delta) or None.
    """
    if len(pre_sol) != len(post_sol):
        return None
    hits = []
    for j in range(min(len(keys), len(pre_sol), len(post_sol))):
        ow = keys[j]
        sd = ((post_sol[j] - pre_sol[j])
              + (_tok_amt(post_tok, ow, WSOL) - _tok_amt(pre_tok, ow, WSOL)))
        td = _tok_amt(post_tok, ow, mint) - _tok_amt(pre_tok, ow, mint)
        if side == 'buy' and sd < 0 and td > 0:
            hits.append((j, ow, td, sd))
        elif side == 'sell' and sd > 0 and td < 0:
            hits.append((j, ow, td, sd))
    if not hits:
        return None
    owners = {e.get('owner') for e in list(pre_tok or []) + list(post_tok or [])
              if e.get('mint') == mint and e.get('owner')}
    tot = 0
    for ow in owners:
        tot += _tok_amt(post_tok, ow, mint) - _tok_amt(pre_tok, ow, mint)
    best = max(hits, key=lambda h: abs(h[2]))
    if abs(tot) > max(1, abs(best[2]) // 1000):
        return None
    return best[1], best[3]


def main():
    out_dir = Path(sys.argv[1])
    files = [Path(a) for a in sys.argv[2:]]
    out_dir.mkdir(parents=True, exist_ok=True)
    launches = (out_dir / 'launches.jsonl').open('w')
    trades = (out_dir / 'trades.jsonl').open('w')

    n_launch = n_trade = n_unknown_ix = n_no_trader = n_failed_tx = n_wide = 0
    seen_launch = set()
    for i, f in enumerate(files, 1):
        for o in stream(f):
            if o.get('record_type') != 'transaction':
                continue
            p = o.get('payload') or {}
            msg = p.get('message') or {}
            meta = p.get('meta') or {}
            if p.get('is_vote'):
                continue
            slot = o.get('slot')
            recv = o.get('recv_unix_ms')
            sig = p.get('signature_b58')
            keys = full_keys(msg, meta)
            pre_sol = meta.get('pre_balances') or []
            post_sol = meta.get('post_balances') or []
            pre_tok = meta.get('pre_token_balances') or []
            post_tok = meta.get('post_token_balances') or []
            fee = meta.get('fee')
            cu = meta.get('compute_units_consumed')
            status = 'failed' if not meta.get('err_is_none', True) else 'success'
            if status == 'failed':
                # A failed tx moves no balances, so no swap inside it is
                # resolvable -- and it is not a trade. Counting these as
                # rejections overstated the coverage gap by ~7x.
                n_failed_tx += 1
                continue
            trader = keys[0] if keys else None

            # Swap instructions invoked by a router arrive as CPIs, i.e. in
            # inner_instructions, not the outer message. Scanning only the outer
            # list silently discards every aggregator-routed trade.
            inner = []
            for grp in (meta.get('inner_instructions') or []):
                inner.extend(grp.get('instructions') or [])
            for ix in list(msg.get('instructions') or []) + inner:
                pi = ix.get('program_id_index')
                d = ix.get('data_b64')
                if pi is None or d is None or pi >= len(keys):
                    continue
                prog = keys[pi]
                if prog not in (PUMP_FUN, PUMP_SWAP):
                    continue
                try:
                    raw = base64.b64decode(d)
                except Exception:
                    continue
                if len(raw) < 8:
                    continue
                kind = DISC.get((prog, raw[:8]))
                if kind is None:
                    n_unknown_ix += 1
                    continue
                etype, side = kind

                if etype == 'create':
                    mint = None
                    for e in post_tok:
                        m = e.get('mint')
                        if m and m not in NOT_A_LAUNCH:
                            mint = m
                            break
                    if mint and mint not in seen_launch:
                        seen_launch.add(mint)
                        n_launch += 1
                        launches.write(json.dumps({
                            'mint': mint, 'creator': trader, 'slot': slot,
                            'recv_unix_ms': recv, 'signature': sig,
                            'failed': status == 'failed',
                        }) + '\n')
                    continue

                if side is None:
                    continue

                # Try EVERY candidate mint, not just the first one encountered.
                # A pumpswap pool touches both a base and a quote mint, so
                # resolving against the wrong one fails the sign test and
                # silently discards a perfectly good swap.
                mints = []
                for e in list(post_tok or []) + list(pre_tok or []):
                    m = e.get('mint')
                    if m and m not in NOT_A_LAUNCH and m not in mints:
                        mints.append(m)
                if not mints:
                    continue
                # Deterministic trader resolution. Both legs must come from one
                # account, and that account must show the swap's sign pattern --
                # so fee recipients, LP/pool accounts and rent destinations are
                # excluded structurally. Earlier versions used keys[0] (the fee
                # payer) or a smallest-mover heuristic, both of which could pick
                # an account that never traded and produce impossible prices.
                try:
                    accts = list(base64.b64decode(ix.get('accounts_b64') or ''))
                except Exception:
                    accts = []
                prefer = PUMP_FUN_TRADER_IX if prog == PUMP_FUN else SWAP_TRADER_IX
                owner = mint = sol = None
                how = None
                for cand_mint in mints:
                    res = resolve_trader(accts, keys, side, cand_mint,
                                         pre_sol, post_sol, pre_tok, post_tok,
                                         prefer_idx=prefer)
                    if res is not None:
                        owner, sol = res
                        mint = cand_mint
                        how = 'instruction_accounts'
                        break
                if owner is None:
                    # Router/aggregator route: the economic actor is not in
                    # this instruction's account list. Fall back to a whole-tx
                    # net-position search, which must conserve or be rejected.
                    for cand_mint in mints:
                        res = wide_resolve(keys, side, cand_mint,
                                           pre_sol, post_sol, pre_tok, post_tok)
                        if res is not None:
                            owner, sol = res
                            mint = cand_mint
                            how = 'net_position'
                            n_wide += 1
                            break
                if owner is None or mint is None:
                    n_no_trader += 1
                    continue
                tok = token_delta(pre_tok, post_tok, owner, mint)
                if not sol or not tok:
                    n_no_trader += 1
                    continue
                n_trade += 1
                pool = resolve_pool(keys, side, mint, pre_sol, post_sol, pre_tok, post_tok)
                ptok = pool[1] if pool else None
                psol = pool[2] if pool else None
                trades.write(json.dumps({
                    'mint': mint, 'trader': owner, 'side': side,
                    'venue': 'pumpswap' if prog == PUMP_SWAP else 'pumpfun',
                    'slot': slot, 'recv_unix_ms': recv, 'signature': sig,
                    'sol_lamports': sol, 'tokens_raw': tok,
                    'pool_sol_lamports': psol, 'pool_tokens_raw': ptok,
                    'price_basis': 'pool' if (psol and ptok) else 'trader',
                    'fee_lamports': fee, 'cu_consumed': cu, 'status': status,
                    'resolution': how,
                }) + '\n')

        print(json.dumps({'file': f.name, 'i': i, 'of': len(files),
                          'launches': n_launch, 'trades': n_trade}),
              flush=True)

    launches.close()
    trades.close()
    print(json.dumps({'schema': 'north_star_renormalized_v1',
                      'files': len(files), 'distinct_launches': n_launch,
                      'trades': n_trade, 'unknown_instructions': n_unknown_ix,
                      'rejected_no_valid_trader': n_no_trader,
                      'skipped_failed_tx': n_failed_tx,
                      'resolved_by_net_position': n_wide,
                      'out': str(out_dir)}, indent=2))


if __name__ == '__main__':
    main()
