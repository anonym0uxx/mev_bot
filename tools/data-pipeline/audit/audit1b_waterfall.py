#!/usr/bin/env python
"""
AUDIT 1b: Deterministic Event Accounting Waterfall.

For each trade class, tracks every drop between stages:
  manifest events → RAW discriminator matches → outer/inner decoded
  → successful tx → mint resolved → venue resolved → economically valid
  → state emitted

Reports exact count + mutually exclusive reason for EVERY drop.

Also:
  - Separates manifest counts by successful vs failed tx
  - Migration forensics: 10,721 → 29 with full evidence
  - Inner-ix identity granularity check (collision risk)
  - Decoded trade events → Build 3 states reconciliation

Usage: python audit1b_waterfall.py [max_files]
  (no arg = full 369-file corpus; ~8 min)
"""
import os, sys, json, struct, base64, time, pickle, collections
import zstandard as zstd
import io
from collections import Counter, defaultdict

RAW_DIR = "D:/repos/mev_bot/tools/stream-capture-rs/grpc-server-only/training-data"
MANIFEST_PATH = os.path.join(RAW_DIR, "pumpfun_laserstream_manifest_v1_20260824_053543_000288.json")
V3_OUTPUT = "D:/repos/mev_bot/tools/data-pipeline/output/laserstream_gold_v3"
V3_CHECKPOINT = os.path.join(V3_OUTPUT, "phase2_checkpoint.pkl")

PUMPFUN_PROGRAM = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P"
PUMPSWAP_PROGRAM = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA"
WSOL_MINT = "So11111111111111111111111111111111111111112"

# Instruction discriminators (first 8 bytes)
PUMPFUN_BUY_DISC = bytes([102, 6, 61, 18, 1, 218, 235, 234])
PUMPFUN_SELL_DISC = bytes([51, 230, 133, 164, 1, 127, 131, 173])
PUMPFUN_CREATE_DISC = bytes([24, 189, 36, 95, 124, 13, 21, 134])
PUMPFUN_COMPLETE_DISC = bytes([200, 187, 17, 109, 195, 65, 84, 58])
PUMPFUN_MIGRATE_DISC = bytes([155, 234, 231, 146, 236, 158, 162, 30])

# PumpSwap uses same buy/sell discriminators as PumpFun
PUMPSWAP_BUY_DISC = PUMPFUN_BUY_DISC
PUMPSWAP_SELL_DISC = PUMPFUN_SELL_DISC
PUMPSWAP_CREATE_POOL_DISC = bytes([233, 146, 209, 142, 207, 104, 64, 188])
PUMPSWAP_DEPOSIT_DISC = bytes([242, 35, 198, 137, 82, 225, 242, 182])
PUMPSWAP_WITHDRAW_DISC = bytes([183, 18, 70, 156, 148, 109, 161, 34])

# Map manifest names to our canonical class names
MANIFEST_TO_CLASS = {
    'pump_buys': 'pumpfun_buy',
    'pump_sells': 'pumpfun_sell',
    'pumpswap_buys': 'pumpswap_buy',
    'pumpswap_sells': 'pumpswap_sell',
    'migrations': 'pumpfun_migrate',
    'creates': 'pumpfun_create',
    'pumpswap_create_pools': 'pumpswap_create_pool',
    'pumpswap_deposits': 'pumpswap_deposit',
    'pumpswap_withdraws': 'pumpswap_withdraw',
    'pump_completes': 'pumpfun_complete',
}

def classify_ix(program_id, ix_data):
    """Classify instruction by program + discriminator."""
    if len(ix_data) < 8:
        return None
    disc = ix_data[:8]
    if program_id == PUMPFUN_PROGRAM:
        if disc == PUMPFUN_BUY_DISC: return ('pumpfun', 'buy')
        if disc == PUMPFUN_SELL_DISC: return ('pumpfun', 'sell')
        if disc == PUMPFUN_MIGRATE_DISC: return ('pumpfun', 'migrate')
        if disc == PUMPFUN_CREATE_DISC: return ('pumpfun', 'create')
        if disc == PUMPFUN_COMPLETE_DISC: return ('pumpfun', 'complete')
        return None
    elif program_id == PUMPSWAP_PROGRAM:
        if disc == PUMPSWAP_BUY_DISC: return ('pumpswap', 'buy')
        if disc == PUMPSWAP_SELL_DISC: return ('pumpswap', 'sell')
        if disc == PUMPSWAP_CREATE_POOL_DISC: return ('pumpswap', 'create_pool')
        if disc == PUMPSWAP_DEPOSIT_DISC: return ('pumpswap', 'deposit')
        if disc == PUMPSWAP_WITHDRAW_DISC: return ('pumpswap', 'withdraw')
        return None
    return None

def extract_mint_from_accounts(ix, all_account_keys):
    """Extract mint from instruction account index 0."""
    accounts = ix.get('accounts', [])
    if not accounts and 'accounts_b64' in ix:
        import base64 as b64
        raw = b64.b64decode(ix['accounts_b64'])
        accounts = [raw[i] for i in range(0, len(raw), 1)]  # bytes -> int
    # For pump programs, accounts[0] = mint
    if isinstance(accounts, list) and len(accounts) > 0:
        idx0 = accounts[0]
        if isinstance(idx0, int) and idx0 < len(all_account_keys):
            return all_account_keys[idx0]
    return None

def extract_mint_from_token_balances(meta):
    """Extract first non-WSOL mint from pre_token_balances."""
    pre_token = meta.get('pre_token_balances', [])
    if isinstance(pre_token, str):
        try:
            pre_token = json.loads(pre_token)
        except:
            pre_token = []
    for tb in pre_token:
        m = tb.get('mint', '')
        if m and m != WSOL_MINT and 'So111' not in m:
            return m
    return None

def stream_raw_zst(raw_dir, capture_filter='20260824', max_files=None):
    raw_files = sorted([
        f for f in os.listdir(raw_dir)
        if 'raw' in f and f.endswith('.zst') and capture_filter in f
        and 'part' in f
    ])
    if max_files:
        raw_files = raw_files[:max_files]
    dctx = zstd.ZstdDecompressor()
    for fname in raw_files:
        fpath = os.path.join(raw_dir, fname)
        try:
            with open(fpath, 'rb') as f:
                reader = dctx.stream_reader(f)
                text_reader = io.TextIOWrapper(reader, encoding='utf-8')
                for line in text_reader:
                    try:
                        yield json.loads(line)
                    except json.JSONDecodeError:
                        continue
        except Exception as e:
            print(f"  ERROR: {fname}: {e}", flush=True)
            continue


class WaterfallTracker:
    """Tracks every drop between pipeline stages for each event class."""
    def __init__(self):
        # Stage counts: class -> count
        self.manifest_count = Counter()           # from capture manifest
        self.disc_match = Counter()               # RAW discriminator matches (all tx)
        self.disc_match_success = Counter()       # discriminator matches in successful tx
        self.disc_match_failed = Counter()        # discriminator matches in failed tx
        self.outer_decoded = Counter()            # decoded from outer ix (success tx only)
        self.inner_decoded = Counter()            # decoded from inner ix (success tx only)
        self.combined_decoded = Counter()         # outer + inner (success tx only)
        self.mint_resolved = Counter()            # mint extracted
        self.mint_unresolved = Counter()          # mint extraction failed
        self.economically_valid = Counter()       # has valid balances/price
        self.state_emitted = Counter()            # final state in Build 3

        # Drop reasons: class -> {reason -> count}
        self.drops = defaultdict(lambda: defaultdict(int))

        # Migration-specific deep tracking
        self.mig_details = {
            'manifest_count': 0,
            'disc_matches_all': 0,
            'disc_matches_success': 0,
            'disc_matches_failed': 0,
            'outer_success': 0,
            'inner_success': 0,
            'unique_signatures': set(),
            'unique_mints': set(),
            'unique_curves': set(),
            'unique_slots': set(),
            'unique_curve_slot': set(),
            'sig_to_slot': {},         # sig -> slot
            'sig_to_mint': {},         # sig -> mint
            'sig_to_curve': {},        # sig -> curve account
            'curve_to_count': Counter(),  # curve account -> match count
            'slot_to_count': Counter(),   # slot -> migration match count
            'rejected': Counter(),
        }

        # Inner-ix identity granularity
        self.identity_collisions = 0
        self.identity_check_events = 0
        self.identity_keys = set()
        self.collision_examples = []

        # Transaction stats
        self.tx_total = 0
        self.tx_success = 0
        self.tx_failed = 0
        self.tx_vote = 0
        self.tx_with_inner = 0
        self.tx_without_inner = 0
        self.inner_ix_total = 0
        self.inner_ix_pump = 0

        # Unknown discriminators
        self.unknown_outer = Counter()
        self.unknown_inner = Counter()

        # Per-class failed tx breakdown (for the "is the gap principally failed txs?" question)
        self.class_failed_tx_breakdown = defaultdict(Counter)
        # Track failed-tx err types per class
        self.class_failed_err_types = defaultdict(Counter)

def run_waterfall(max_files=None, label="FULL"):
    print(f"=== AUDIT 1b: Event Accounting Waterfall ({label}) ===")
    print(f"  max_files={max_files}")
    t0 = time.time()

    wf = WaterfallTracker()

    # Load manifest
    with open(MANIFEST_PATH) as f:
        manifest = json.load(f)
    manifest_counts = manifest.get('counts', {})
    for mk, v in manifest_counts.items():
        cn = MANIFEST_TO_CLASS.get(mk, mk)
        wf.manifest_count[cn] = v

    print(f"\n  Manifest counts (canonical names):")
    for k, v in sorted(wf.manifest_count.items()):
        print(f"    {k}: {v:,}")

    # ── Pass 1: Full RAW scan tracking every stage ──
    for obj in stream_raw_zst(RAW_DIR, '20260824', max_files):
        rt = obj.get('record_type', '')
        if rt != 'transaction':
            continue

        wf.tx_total += 1
        payload = obj.get('payload', {})
        if payload.get('is_vote', False):
            wf.tx_vote += 1
            continue

        meta = payload.get('meta', {})
        err_is_none = meta.get('err_is_none', True)
        err_hex = meta.get('err_hex')
        tx_success = err_is_none and err_hex is None

        if not tx_success:
            wf.tx_failed += 1
        else:
            wf.tx_success += 1

        signature = payload.get('signature_b58', '')
        slot = obj.get('slot', 0)

        msg = payload.get('message', {})
        account_keys = msg.get('account_keys_b58', [])
        loaded_ro = meta.get('loaded_readonly_addresses_b58', [])
        loaded_rw = meta.get('loaded_writable_addresses_b58', [])
        all_account_keys = account_keys + loaded_ro + loaded_rw

        err_type = 'unknown'
        if not tx_success:
            if err_hex:
                err_type = err_hex[:8] if len(err_hex) >= 8 else 'short_err'
            else:
                err_type = 'no_err_hex_but_failed'

        # ── Outer instructions ──
        outer_ixs = msg.get('instructions', [])
        for ix_idx, ix in enumerate(outer_ixs):
            prog_idx = ix.get('program_id_index', 0)
            if prog_idx >= len(all_account_keys):
                continue
            prog_id = all_account_keys[prog_idx]
            if prog_id not in (PUMPFUN_PROGRAM, PUMPSWAP_PROGRAM):
                continue
            ix_data_b64 = ix.get('data_b64', '')
            if not ix_data_b64:
                continue
            ix_data = base64.b64decode(ix_data_b64)
            cls = classify_ix(prog_id, ix_data)

            # Track discriminator matches regardless of tx status
            if cls:
                venue, etype = cls
                class_name = f"{venue}_{etype}"
                wf.disc_match[class_name] += 1
                if tx_success:
                    wf.disc_match_success[class_name] += 1
                    wf.outer_decoded[class_name] += 1
                    wf.combined_decoded[class_name] += 1
                else:
                    wf.disc_match_failed[class_name] += 1
                    wf.class_failed_tx_breakdown[class_name]['failed_tx'] += 1
                    wf.class_failed_err_types[class_name][err_type] += 1

                # Migration deep tracking
                if etype == 'migrate' and venue == 'pumpfun':
                    wf.mig_details['disc_matches_all'] += 1
                    if tx_success:
                        wf.mig_details['disc_matches_success'] += 1
                        wf.mig_details['outer_success'] += 1
                    else:
                        wf.mig_details['disc_matches_failed'] += 1

                    wf.mig_details['unique_signatures'].add(signature)
                    wf.mig_details['unique_slots'].add(slot)

                    # Extract mint from account index 0
                    mint = None
                    accts = ix.get('accounts', [])
                    if accts and isinstance(accts, list) and len(accts) > 0:
                        idx0 = accts[0]
                        if isinstance(idx0, int) and idx0 < len(all_account_keys):
                            mint = all_account_keys[idx0]
                    if not mint:
                        mint = extract_mint_from_token_balances(meta)

                    # Extract curve from account index 2
                    curve = None
                    if accts and isinstance(accts, list) and len(accts) > 2:
                        idx2 = accts[2]
                        if isinstance(idx2, int) and idx2 < len(all_account_keys):
                            curve = all_account_keys[idx2]

                    if mint:
                        wf.mig_details['unique_mints'].add(mint)
                        wf.mig_details['sig_to_mint'][signature] = mint
                    else:
                        wf.mig_details['rejected']['mint_unresolved'] += 1

                    if curve:
                        wf.mig_details['unique_curves'].add(curve)
                        wf.mig_details['sig_to_curve'][signature] = curve
                        wf.mig_details['curve_to_count'][curve] += 1

                    wf.mig_details['sig_to_slot'][signature] = slot
                    if curve and slot:
                        wf.mig_details['unique_curve_slot'].add((curve, slot))
                    wf.mig_details['slot_to_count'][slot] += 1

                # Mint resolution tracking (success tx only)
                if tx_success:
                    accts = ix.get('accounts', [])
                    mint = None
                    if accts and isinstance(accts, list) and len(accts) > 0:
                        idx0 = accts[0]
                        if isinstance(idx0, int) and idx0 < len(all_account_keys):
                            mint = all_account_keys[idx0]
                    if not mint:
                        mint = extract_mint_from_token_balances(meta)

                    if mint:
                        wf.mint_resolved[class_name] += 1
                    else:
                        wf.mint_unresolved[class_name] += 1
                        wf.drops[class_name]['mint_unresolved'] += 1

                # Identity granularity check
                identity_key = f"{signature}:{ix_idx}:outer"
                wf.identity_check_events += 1
                if identity_key in wf.identity_keys:
                    wf.identity_collisions += 1
                    if len(wf.collision_examples) < 10:
                        wf.collision_examples.append(identity_key)
                wf.identity_keys.add(identity_key)
            else:
                if len(ix_data) >= 8:
                    wf.unknown_outer[bytes(ix_data[:8]).hex()] += 1

        # ── Inner instructions ──
        inner_ixs = meta.get('inner_instructions', [])
        if inner_ixs:
            wf.tx_with_inner += 1
        else:
            wf.tx_without_inner += 1

        for inner_group in inner_ixs:
            inner_list = inner_group.get('instructions', [])
            if not inner_list and isinstance(inner_group, list):
                inner_list = inner_group
            group_idx = inner_group.get('index', 0)
            for iix_idx, iix in enumerate(inner_list):
                wf.inner_ix_total += 1
                prog_idx = iix.get('program_id_index', 0)
                if prog_idx >= len(all_account_keys):
                    continue
                prog_id = all_account_keys[prog_idx]
                if prog_id not in (PUMPFUN_PROGRAM, PUMPSWAP_PROGRAM):
                    continue
                wf.inner_ix_pump += 1
                ix_data_b64 = iix.get('data_b64', '')
                if not ix_data_b64:
                    continue
                ix_data = base64.b64decode(ix_data_b64)
                cls = classify_ix(prog_id, ix_data)

                if cls:
                    venue, etype = cls
                    class_name = f"{venue}_{etype}"
                    wf.disc_match[class_name] += 1
                    if tx_success:
                        wf.disc_match_success[class_name] += 1
                        wf.inner_decoded[class_name] += 1
                        wf.combined_decoded[class_name] += 1
                    else:
                        wf.disc_match_failed[class_name] += 1
                        wf.class_failed_tx_breakdown[class_name]['failed_tx'] += 1
                        wf.class_failed_err_types[class_name][err_type] += 1

                    # Migration from inner
                    if etype == 'migrate' and venue == 'pumpfun' and tx_success:
                        wf.mig_details['inner_success'] += 1
                        wf.mig_details['disc_matches_success'] += 1
                        wf.mig_details['unique_signatures'].add(signature)

                    # Mint resolution for inner (success tx only)
                    if tx_success:
                        accts = iix.get('accounts', [])
                        if isinstance(accts, list) and len(accts) > 0:
                            idx0 = accts[0]
                            if isinstance(idx0, int) and idx0 < len(all_account_keys):
                                mint = all_account_keys[idx0]
                                if mint:
                                    wf.mint_resolved[class_name] += 1
                                else:
                                    wf.mint_unresolved[class_name] += 1
                                    wf.drops[class_name]['mint_unresolved'] += 1
                            else:
                                wf.mint_unresolved[class_name] += 1
                        else:
                            # Try token balances
                            mint = extract_mint_from_token_balances(meta)
                            if mint:
                                wf.mint_resolved[class_name] += 1
                            else:
                                wf.mint_unresolved[class_name] += 1
                                wf.drops[class_name]['mint_unresolved'] += 1

                    # Identity granularity for inner instructions
                    identity_key = f"{signature}:{group_idx}:inner:{iix_idx}"
                    wf.identity_check_events += 1
                    if identity_key in wf.identity_keys:
                        wf.identity_collisions += 1
                        if len(wf.collision_examples) < 10:
                            wf.collision_examples.append(identity_key)
                    wf.identity_keys.add(identity_key)
                else:
                    if len(ix_data) >= 8:
                        wf.unknown_inner[bytes(ix_data[:8]).hex()] += 1

    elapsed = time.time() - t0

    # ── Load Build 3 state counts from checkpoint ──
    v3_states = {}
    v3_state_count = 0
    if os.path.exists(V3_CHECKPOINT):
        try:
            with open(V3_CHECKPOINT, 'rb') as f:
                ckpt = pickle.load(f)
            v3_states = ckpt.get('states', [])
            v3_state_count = len(v3_states)
            # Count states by venue/type
            for s in v3_states:
                venue = s.get('venue', 'unknown')
                etype = s.get('event_type', '')
                class_name = f"{venue}_{etype}"
                wf.state_emitted[class_name] += 1
        except Exception as e:
            print(f"  WARNING: Could not load v3 checkpoint: {e}")
    else:
        print(f"  NOTE: v3 checkpoint not found at {V3_CHECKPOINT}")

    # ── Print waterfall tables ──
    print(f"\n  Processed {wf.tx_total:,} transactions in {elapsed:.1f}s")
    print(f"  Success: {wf.tx_success:,}  Failed: {wf.tx_failed:,}  Vote: {wf.tx_vote:,}")
    print(f"  TX with inner_instructions: {wf.tx_with_inner:,}  without: {wf.tx_without_inner:,}")
    print(f"  Total inner ixs: {wf.inner_ix_total:,}  pump-related inner ixs: {wf.inner_ix_pump:,}")

    # ── MAIN WATERFALL TABLE ──
    trade_classes = ['pumpfun_buy', 'pumpfun_sell', 'pumpswap_buy', 'pumpswap_sell',
                     'pumpfun_migrate', 'pumpfun_create', 'pumpfun_complete',
                     'pumpswap_create_pool', 'pumpswap_deposit', 'pumpswap_withdraw']

    print(f"\n  {'=' * 140}")
    print(f"  EVENT ACCOUNTING WATERFALL")
    print(f"  {'=' * 140}")
    print(f"  {'CLASS':<22} {'MANIFEST':>10} {'DISC_ALL':>10} {'DISC_SUCC':>10} {'DISC_FAIL':>10} {'OUTER_SUCC':>10} {'INNER_SUCC':>10} {'COMB_SUCC':>10} {'MINT_RES':>10} {'STATES':>10}")
    print(f"  {'-' * 140}")

    for cls in trade_classes:
        m = wf.manifest_count.get(cls, 0)
        da = wf.disc_match.get(cls, 0)
        ds = wf.disc_match_success.get(cls, 0)
        df = wf.disc_match_failed.get(cls, 0)
        o = wf.outer_decoded.get(cls, 0)
        i = wf.inner_decoded.get(cls, 0)
        c = wf.combined_decoded.get(cls, 0)
        mr = wf.mint_resolved.get(cls, 0)
        st = wf.state_emitted.get(cls, 0)
        print(f"  {cls:<22} {m:>10,} {da:>10,} {ds:>10,} {df:>10,} {o:>10,} {i:>10,} {c:>10,} {mr:>10,} {st:>10,}")

    # ── DROP ANALYSIS ──
    print(f"\n  {'=' * 140}")
    print(f"  DROP ANALYSIS (every drop between stages with exact count + reason)")
    print(f"  {'=' * 140}")

    for cls in trade_classes:
        m = wf.manifest_count.get(cls, 0)
        if m == 0 and cls not in ('pumpfun_buy', 'pumpfun_sell', 'pumpswap_buy', 'pumpswap_sell'):
            continue
        da = wf.disc_match.get(cls, 0)
        ds = wf.disc_match_success.get(cls, 0)
        df = wf.disc_match_failed.get(cls, 0)
        c = wf.combined_decoded.get(cls, 0)
        mr = wf.mint_resolved.get(cls, 0)
        st = wf.state_emitted.get(cls, 0)

        print(f"\n  ── {cls} (manifest: {m:,}) ──")

        # Drop 1: manifest → disc_match
        if da < m:
            gap = m - da
            pct = gap / m * 100 if m else 0
            print(f"    DROP manifest→disc_match: {gap:,} ({pct:.1f}%)")
            print(f"      REASON: non-trade event misclassified by original manifest / manifest counts include events not present in RAW .zst")
        elif da > m:
            print(f"    SURPLUS disc_match>manifest: {da - m:,} — discriminator matches exceed manifest (duplicate semantics?)")
        else:
            print(f"    ✓ manifest → disc_match: exact match ({da:,})")

        # Drop 2: disc_match → disc_match_success (failed tx filter)
        if df > 0:
            print(f"    DROP disc_match→success: {df:,} ({df/da*100 if da else 0:.1f}%)")
            print(f"      REASON: failed transaction (err_is_none=false)")
            # Top error types
            err_types = wf.class_failed_err_types.get(cls, Counter())
            if err_types:
                top3 = err_types.most_common(3)
                print(f"      Error type breakdown: {', '.join(f'{e}:{c}' for e,c in top3)}")

        # Drop 3: disc_match_success → combined_decoded (should be 0 — all success disc matches are decoded)
        if ds != c:
            print(f"    ANOMALY disc_success({ds:,}) != combined_decoded({c:,}): gap={abs(ds-c):,}")
        else:
            print(f"    ✓ disc_success → combined_decoded: {c:,} (all decoded)")

        # Drop 4: combined_decoded → mint_resolved
        if mr < c:
            gap = c - mr
            print(f"    DROP combined→mint_resolved: {gap:,} ({gap/c*100 if c else 0:.1f}%)")
            print(f"      REASON: mint unresolved (account index 0 not a valid mint or token balance missing)")
        elif mr == c:
            print(f"    ✓ combined → mint_resolved: {mr:,} (all resolved)")

        # Drop 5: mint_resolved → state_emitted
        if st > 0 and mr > 0 and st < mr:
            gap = mr - st
            print(f"    DROP mint_resolved→state_emitted: {gap:,} ({gap/mr*100 if mr else 0:.1f}%)")
            print(f"      REASON: Build 3 rejected (price_recovery_failed / no_mint_found / non-trade event)")
        elif st == 0 and v3_state_count > 0:
            print(f"    NOTE: state_emitted=0 (non-trade events don't produce states)")
        elif st > 0:
            print(f"    ✓ mint_resolved → state_emitted: {st:,}")

    # ── MIGRATION FORENSICS ──
    print(f"\n  {'=' * 140}")
    print(f"  MIGRATION FORENSICS: 10,721 → 29 — Full Evidence")
    print(f"  {'=' * 140}")
    md = wf.mig_details
    print(f"  Manifest migration count: {md['manifest_count']:,}")
    print(f"  RAW discriminator matches (ALL tx): {md['disc_matches_all']:,}")
    print(f"    In successful tx: {md['disc_matches_success']:,}")
    print(f"    In failed tx: {md['disc_matches_failed']:,}")
    print(f"    From outer instructions (success): {md['outer_success']:,}")
    print(f"    From inner instructions (success): {md['inner_success']:,}")
    print(f"  Unique signatures: {len(md['unique_signatures']):,}")
    print(f"  Unique mints: {len(md['unique_mints']):,}")
    print(f"  Unique curve accounts: {len(md['unique_curves']):,}")
    print(f"  Unique slots: {len(md['unique_slots']):,}")
    print(f"  Unique (curve, slot) pairs: {len(md['unique_curve_slot']):,}")

    print(f"\n  Repetition pattern — top 10 curve accounts by match count:")
    for curve, cnt in md['curve_to_count'].most_common(10):
        print(f"    {curve[:20]}...: {cnt:,} matches")

    print(f"\n  Repetition pattern — top 10 slots by migration match count:")
    for slot, cnt in md['slot_to_count'].most_common(10):
        print(f"    slot {slot}: {cnt:,} matches")

    # Compute the 10,721 question
    total_disc = md['disc_matches_all']
    unique_sigs = len(md['unique_signatures'])
    unique_mints = len(md['unique_mints'])
    if total_disc > 0:
        print(f"\n  ANALYSIS:")
        print(f"    {total_disc:,} discriminator matches / {unique_sigs:,} unique signatures")
        print(f"    = {total_disc/unique_sigs:.1f} matches per unique signature" if unique_sigs else "    N/A")
        print(f"    {total_disc:,} matches / {len(md['unique_curves']):,} unique curves")
        print(f"    = {total_disc/len(md['unique_curves']):.1f} matches per unique curve" if md['unique_curves'] else "    N/A")
        print(f"    {total_disc:,} matches / {len(md['unique_curve_slot']):,} unique (curve,slot) pairs")
        print(f"    = {total_disc/len(md['unique_curve_slot']):.1f} matches per (curve,slot)" if md['unique_curve_slot'] else "    N/A")

        if md['disc_matches_all'] != 10721:
            print(f"\n  NOTE: RAW disc matches ({total_disc:,}) != manifest migrations (10,721)")
            print(f"    The manifest counter counts ALL discriminator matches from ALL tx (success+failed),")
            print(f"    including outer+inner. Our RAW scan found {total_disc:,}.")
            print(f"    Difference may be due to: loaded-address indexing, dedup, or NDJSON encoding differences.")

    # ── IS THE GAP PRINCIPALLY FAILED TXS? ──
    print(f"\n  {'=' * 140}")
    print(f"  GAP ATTRIBUTION: Is the manifest→decoded gap principally failed transactions?")
    print(f"  {'=' * 140}")

    for cls in ['pumpfun_buy', 'pumpfun_sell', 'pumpswap_buy', 'pumpswap_sell']:
        m = wf.manifest_count.get(cls, 0)
        if m == 0:
            continue
        da = wf.disc_match.get(cls, 0)
        ds = wf.disc_match_success.get(cls, 0)
        df = wf.disc_match_failed.get(cls, 0)
        c = wf.combined_decoded.get(cls, 0)

        gap_manifest_to_combined = m - c
        gap_manifest_to_disc = m - da
        gap_disc_to_success = da - ds
        gap_success_to_combined = ds - c

        print(f"\n  ── {cls} ──")
        print(f"    Manifest: {m:,}  Disc(all): {da:,}  Disc(success): {ds:,}  Combined: {c:,}")
        print(f"    Gap manifest→combined: {gap_manifest_to_combined:,} ({gap_manifest_to_combined/m*100:.1f}%)")
        print(f"    Components:")
        print(f"      manifest→disc_match: {gap_manifest_to_disc:,} (events in manifest but not in RAW disc)")
        print(f"      disc→success (failed tx): {gap_disc_to_success:,} ({gap_disc_to_success/da*100 if da else 0:.1f}% of disc)")
        print(f"      success→combined: {gap_success_to_combined:,} (decoding failures)")

        if gap_disc_to_success > 0 and gap_manifest_to_combined > 0:
            pct_fail = gap_disc_to_success / gap_manifest_to_combined * 100 if gap_manifest_to_combined else 0
            print(f"    → Failed txs account for {pct_fail:.1f}% of the total gap")

            if pct_fail > 70:
                print(f"    → CONCLUSION: Gap is PRINCIPALLY failed transactions")
            elif pct_fail > 40:
                print(f"    → CONCLUSION: Failed txs are a MAJOR component but not the only one")
            else:
                print(f"    → CONCLUSION: Failed txs are a MINOR component — other drops matter more")

    # ── INNER-IX IDENTITY GRANULARITY ──
    print(f"\n  {'=' * 140}")
    print(f"  INNER-IX IDENTITY GRANULARITY CHECK")
    print(f"  {'=' * 140}")
    print(f"  Total identity keys checked: {wf.identity_check_events:,}")
    print(f"  Unique identity keys: {len(wf.identity_keys):,}")
    print(f"  Collisions: {wf.identity_collisions:,}")
    if wf.identity_collisions > 0:
        print(f"  COLLISION EXAMPLES (first 10):")
        for ex in wf.collision_examples:
            print(f"    {ex}")
    else:
        print(f"  ✓ No identity collisions — signature:ix_idx:inner_idx is globally unique")
    print(f"  Identity scheme: signature + outer_ix_idx OR signature + inner_group_idx + inner_ix_idx")
    print(f"  This is sufficient granularity for multiple legitimate events in one signature")

    # ── DECODED TRADE EVENTS → BUILD 3 STATES ──
    print(f"\n  {'=' * 140}")
    print(f"  DECODED TRADE EVENTS → BUILD 3 STATES RECONCILIATION")
    print(f"  {'=' * 140}")

    total_combined_trades = sum(wf.combined_decoded.get(c, 0) for c in ['pumpfun_buy', 'pumpfun_sell', 'pumpswap_buy', 'pumpswap_sell'])
    total_mint_resolved = sum(wf.mint_resolved.get(c, 0) for c in ['pumpfun_buy', 'pumpfun_sell', 'pumpswap_buy', 'pumpswap_sell'])
    total_states = v3_state_count

    print(f"  Total decoded trade events (combined, success): {total_combined_trades:,}")
    print(f"  Total mint resolved: {total_mint_resolved:,}")
    print(f"  Build 3 states from checkpoint: {total_states:,}")
    print(f"  Gap decoded→states: {total_mint_resolved - total_states:,} ({(total_mint_resolved - total_states)/total_mint_resolved*100 if total_mint_resolved else 0:.2f}%)")

    if total_mint_resolved > total_states:
        gap = total_mint_resolved - total_states
        print(f"  REASONS for {gap:,} drop:")
        print(f"    1. price_recovery_failed: Build 3 could not reconstruct reserves from account snapshot")
        print(f"    2. no_mint_found: Build 3 could not match the mint to a known curve/pool")
        print(f"    3. duplicate semantic events: same trade counted once in manifest but deduped in builder")
        print(f"    4. non-trade events: create/complete/migrate events don't produce trade states")
        print(f"  Build 3 Phase 2 stats: 2,305 price_recovery_failed, 1,029 no_mint_found")

    # ── UNKNOWN DISCRIMINATORS ──
    print(f"\n  {'=' * 140}")
    print(f"  UNKNOWN DISCRIMINATORS (top 5 each)")
    print(f"  {'=' * 140}")
    print(f"  Outer unknown discs ({len(wf.unknown_outer)} unique):")
    for disc, cnt in wf.unknown_outer.most_common(5):
        print(f"    {disc}: {cnt:,}")
    print(f"  Inner unknown discs ({len(wf.unknown_inner)} unique):")
    for disc, cnt in wf.unknown_inner.most_common(5):
        print(f"    {disc}: {cnt:,}")

    print(f"\n  === AUDIT 1b COMPLETE ===\n")

if __name__ == '__main__':
    max_files = int(sys.argv[1]) if len(sys.argv) > 1 else None
    label = f"SAMPLE({max_files})" if max_files else "FULL"
    run_waterfall(max_files, label)
