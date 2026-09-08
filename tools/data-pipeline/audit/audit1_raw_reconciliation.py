#!/usr/bin/env python
"""
AUDIT 1: RAW Event Reconciliation — Independent decoder.

Reads RAW .zst files directly (NOT the v3 builder), processes BOTH outer
AND inner instructions, counts every event class, and reconciles against
the capture manifest. Specifically investigates the 10,721→29 migration
discrepancy.

Key hypothesis: v3 builder only processes outer instructions (line 731),
missing CPI/inner instructions where most PumpFun events live.
"""
import os, sys, json, struct, base64, time
import zstandard as zstd
import io
from collections import Counter, defaultdict

RAW_DIR = "D:/repos/mev_bot/tools/stream-capture-rs/grpc-server-only/training-data"
MANIFEST_PATH = os.path.join(RAW_DIR, "pumpfun_laserstream_manifest_v1_20260824_053543_000288.json")

PUMPFUN_PROGRAM = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P"
PUMPSWAP_PROGRAM = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA"
WSOL_MINT = "So11111111111111111111111111111111111111112"

# Instruction discriminators (first 8 bytes)
PUMPFUN_BUY_DISC = bytes([102, 6, 61, 18, 1, 218, 235, 234])
PUMPFUN_SELL_DISC = bytes([51, 230, 133, 164, 1, 127, 131, 173])
PUMPFUN_CREATE_DISC = bytes([24, 189, 36, 95, 124, 13, 21, 134])
PUMPFUN_COMPLETE_DISC = bytes([200, 187, 17, 109, 195, 65, 84, 58])
PUMPFUN_MIGRATE_DISC = bytes([155, 234, 231, 146, 236, 158, 162, 30])

PUMPSWAP_BUY_DISC = bytes([102, 6, 61, 18, 1, 218, 235, 234])
PUMPSWAP_SELL_DISC = bytes([51, 230, 133, 164, 1, 127, 131, 173])
PUMPSWAP_CREATE_POOL_DISC = bytes([233, 146, 209, 142, 207, 104, 64, 188])
PUMPSWAP_DEPOSIT_DISC = bytes([242, 35, 198, 137, 82, 225, 242, 182])
PUMPSWAP_WITHDRAW_DISC = bytes([183, 18, 70, 156, 148, 109, 161, 34])

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

def stream_raw_zst(raw_dir, capture_filter='20260824', max_files=None):
    raw_files = sorted([
        f for f in os.listdir(raw_dir)
        if 'raw' in f and f.endswith('.zst') and capture_filter in f
        and 'part' in f  # only part files, not manifest
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

def run_audit(max_files=None, sample_label="FULL"):
    """Run RAW reconciliation audit."""
    print(f"=== AUDIT 1: RAW Event Reconciliation ({sample_label}) ===")
    print(f"  max_files={max_files}")
    t0 = time.time()

    # Load capture manifest for comparison
    with open(MANIFEST_PATH) as f:
        manifest = json.load(f)
    manifest_counts = manifest.get('counts', {})
    print(f"\n  Capture manifest counts:")
    for k, v in sorted(manifest_counts.items()):
        print(f"    {k}: {v:,}")
    print(f"  total_raw_records: {manifest.get('total_raw_records', 'N/A'):,}")
    print(f"  total_events: {manifest.get('total_events', 'N/A'):,}")

    # Counters
    outer_counts = Counter()  # events from outer instructions only
    inner_counts = Counter()  # events from inner instructions only
    combined_counts = Counter()  # outer + inner
    record_type_counts = Counter()
    tx_total = 0
    tx_success = 0
    tx_failed = 0
    tx_vote = 0

    # Migration-specific tracking
    mig_outer = []
    mig_inner = []
    mig_raw_disc_matches = 0  # raw records matching migration discriminator
    mig_unique_sigs = set()
    mig_unique_mints = set()
    mig_decoded_ok = 0
    mig_rejected = Counter()  # reason -> count

    # Inner instruction presence
    tx_with_inner = 0
    tx_without_inner = 0
    inner_ix_total = 0
    inner_ix_pump = 0  # pump-related inner ixs

    # Unknown discriminators (for debugging)
    unknown_discs_outer = Counter()
    unknown_discs_inner = Counter()

    file_count = 0
    for obj in stream_raw_zst(RAW_DIR, '20260824', max_files):
        rt = obj.get('record_type', '')
        record_type_counts[rt] += 1

        if rt != 'transaction':
            continue

        tx_total += 1
        payload = obj.get('payload', {})
        if payload.get('is_vote', False):
            tx_vote += 1
            continue

        meta = payload.get('meta', {})
        err_is_none = meta.get('err_is_none', True)
        tx_success_flag = err_is_none and meta.get('err_hex') is None
        if not tx_success_flag:
            tx_failed += 1
            continue
        tx_success += 1

        msg = payload.get('message', {})
        account_keys = msg.get('account_keys_b58', [])
        loaded_ro = meta.get('loaded_readonly_addresses_b58', [])
        loaded_rw = meta.get('loaded_writable_addresses_b58', [])
        all_account_keys = account_keys + loaded_ro + loaded_rw

        signature = payload.get('signature_b58', '')
        slot = obj.get('slot', 0)

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
            if cls:
                venue, etype = cls
                outer_counts[f'{venue}_{etype}'] += 1
                combined_counts[f'{venue}_{etype}'] += 1

                if etype == 'migrate':
                    mig_raw_disc_matches += 1
                    mig_unique_sigs.add(signature)
                    # Extract mint from token balances
                    pre_token = meta.get('pre_token_balances', [])
                    mint = None
                    for tb in pre_token:
                        m = tb.get('mint', '')
                        if m and m != WSOL_MINT and 'So111' not in m:
                            mint = m
                            break
                    if mint:
                        mig_unique_mints.add(mint)
                        mig_decoded_ok += 1
                    else:
                        mig_rejected['no_mint_in_token_balances'] += 1
                    mig_outer.append({
                        'sig': signature[:20],
                        'slot': slot,
                        'mint': mint,
                    })
            else:
                # Unknown discriminator from pump program
                if len(ix_data) >= 8:
                    unknown_discs_outer[bytes(ix_data[:8]).hex()] += 1

        # ── Inner instructions ──
        inner_ixs = meta.get('inner_instructions', [])
        if inner_ixs:
            tx_with_inner += 1
        else:
            tx_without_inner += 1

        for inner_group in inner_ixs:
            # inner_group may have 'instructions' list
            inner_list = inner_group.get('instructions', [])
            if not inner_list and isinstance(inner_group, list):
                inner_list = inner_group
            for iix in inner_list:
                inner_ix_total += 1
                prog_idx = iix.get('program_id_index', 0)
                if prog_idx >= len(all_account_keys):
                    continue
                prog_id = all_account_keys[prog_idx]
                if prog_id not in (PUMPFUN_PROGRAM, PUMPSWAP_PROGRAM):
                    continue
                inner_ix_pump += 1
                ix_data_b64 = iix.get('data_b64', '')
                if not ix_data_b64:
                    continue
                ix_data = base64.b64decode(ix_data_b64)
                cls = classify_ix(prog_id, ix_data)
                if cls:
                    venue, etype = cls
                    inner_counts[f'{venue}_{etype}'] += 1
                    combined_counts[f'{venue}_{etype}'] += 1

                    if etype == 'migrate':
                        mig_raw_disc_matches += 1
                        mig_unique_sigs.add(signature)
                        pre_token = meta.get('pre_token_balances', [])
                        mint = None
                        for tb in pre_token:
                            m = tb.get('mint', '')
                            if m and m != WSOL_MINT and 'So111' not in m:
                                mint = m
                                break
                        if mint:
                            mig_unique_mints.add(mint)
                            mig_decoded_ok += 1
                        else:
                            mig_rejected['no_mint_in_token_balances'] += 1
                        mig_inner.append({
                            'sig': signature[:20],
                            'slot': slot,
                            'mint': mint,
                        })
                else:
                    if len(ix_data) >= 8:
                        unknown_discs_inner[bytes(ix_data[:8]).hex()] += 1

    elapsed = time.time() - t0
    print(f"\n  Processed {tx_total:,} transactions in {elapsed:.1f}s")
    print(f"  Success: {tx_success:,}  Failed: {tx_failed:,}  Vote: {tx_vote:,}")
    print(f"  Record types: {dict(record_type_counts)}")
    print(f"  TX with inner_instructions: {tx_with_inner:,}  without: {tx_without_inner:,}")
    print(f"  Total inner ixs: {inner_ix_total:,}  pump-related inner ixs: {inner_ix_pump:,}")

    # ── Reconciliation table ──
    print(f"\n  {'─' * 90}")
    print(f"  {'EVENT CLASS':<25} {'MANIFEST':>12} {'OUTER':>12} {'INNER':>12} {'COMBINED':>12} {'OUTER%':>8} {'COMB%':>8}")
    print(f"  {'─' * 90}")
    all_classes = sorted(set(list(manifest_counts.keys()) + list(combined_counts.keys())))
    for cls in all_classes:
        m = manifest_counts.get(cls, 0)
        o = outer_counts.get(cls, 0)
        i = inner_counts.get(cls, 0)
        c = combined_counts.get(cls, 0)
        op = f"{o/m*100:.1f}" if m > 0 else "N/A"
        cp = f"{c/m*100:.1f}" if m > 0 else "N/A"
        print(f"  {cls:<25} {m:>12,} {o:>12,} {i:>12,} {c:>12,} {op:>8} {cp:>8}")

    # ── Migration forensics ──
    print(f"\n  === MIGRATION FORENSICS ===")
    print(f"  Manifest migrations: {manifest_counts.get('migrations', 0):,}")
    print(f"  Raw disc matches (outer+inner): {mig_raw_disc_matches:,}")
    print(f"  From outer instructions: {len(mig_outer):,}")
    print(f"  From inner instructions: {len(mig_inner):,}")
    print(f"  Unique signatures: {len(mig_unique_sigs):,}")
    print(f"  Unique mints: {len(mig_unique_mints):,}")
    print(f"  Successfully decoded (mint found): {mig_decoded_ok:,}")
    print(f"  Rejected/unresolved: {sum(mig_rejected.values()):,}")
    for reason, cnt in mig_rejected.most_common():
        print(f"    {reason}: {cnt:,}")

    # ── Unknown discriminators (top 10) ──
    print(f"\n  === UNKNOWN DISCRIMINATORS (top 10 each) ===")
    print(f"  Outer unknown discs ({len(unknown_discs_outer)} unique):")
    for disc, cnt in unknown_discs_outer.most_common(10):
        print(f"    {disc}: {cnt:,}")
    print(f"  Inner unknown discs ({len(unknown_discs_inner)} unique):")
    for disc, cnt in unknown_discs_inner.most_common(10):
        print(f"    {disc}: {cnt:,}")

    print(f"\n  === AUDIT 1 COMPLETE ===\n")

if __name__ == '__main__':
    max_files = int(sys.argv[1]) if len(sys.argv) > 1 else None
    label = f"SAMPLE({max_files})" if max_files else "FULL"
    run_audit(max_files, label)
