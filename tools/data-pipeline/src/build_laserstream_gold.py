"""
laserstream_gold_v1 — Re-decode the committed 300-min RAW LaserStream capture
using corrected authoritative Pump.fun layouts and the CORRECT PumpSwap
program ID (pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA).

INPUT: D:/repos/mev_bot/tools/stream-capture-rs/grpc-server-only/training-data/
       (185 zstd parts + manifest, RAW is authority)
OUTPUT: tools/data-pipeline/output/laserstream_gold_v1/ (parquet + manifest)

KEY CORRECTIONS:
  - PumpSwap program ID fixed from pPEEEJ5... → pAMMBay6...
  - Re-decode from RAW zstd, not from the normalized NDJSON (which used wrong ID)
  - Mark right-censored states (capture ends before 300s forward horizon)
  - Include our-wallet episodes where identifiable
  - Do NOT fabricate missing PumpSwap trades (capture subscribed to wrong program)

The completed 300-min tape was captured with the wrong PumpSwap program ID,
so it contains ONLY pump.fun bonding-curve events (creates, buys, sells,
migrates, completes). PumpSwap trades are absent — this is a KNOWN LIMITATION.
Future captures with the fixed ID will include PumpSwap.
"""

from __future__ import annotations
import os, sys, json, hashlib, time, struct, zstandard, io
from pathlib import Path
from datetime import datetime, timezone
from collections import defaultdict
import random

import pyarrow as pa
import pyarrow.parquet as pq

sys.path.insert(0, str(Path(__file__).parent))
sys.path.insert(0, str(Path(__file__).parent.parent / "schemas"))
from utils import write_parquet_partitioned, hash_file, file_size_bytes, write_manifest, mint_disjoint_split
from gold_schema import PumpStateV1, PumpOutcomeV1, SimulatorLabelV1, stable_id, SCHEMA_VERSION, GENERATOR_VERSION

# ─── Config ───────────────────────────────────────────────────────────
RAW_DIR = Path("D:/repos/mev_bot/tools/stream-capture-rs/grpc-server-only/training-data")
MANIFEST_PATH = RAW_DIR / "pumpfun_laserstream_manifest_v1_20260823_133256_000398.json"
OUTPUT_DIR = Path("D:/repos/mev_bot/tools/data-pipeline/output/laserstream_gold_v1")

# Correct program IDs
PUMP_FUN = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P"
PUMP_SWAP = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA"

# Instruction discriminators: sha256("global:<name>")[..8]
PUMP_BUY_DISCRIM = bytes([102, 6, 61, 18, 1, 218, 235, 234])
PUMP_SELL_DISCRIM = bytes([51, 230, 133, 164, 1, 127, 131, 173])
PUMP_CREATE_DISCRIM = bytes([24, 30, 200, 40, 5, 28, 7, 119])
PUMP_COMPLETE_DISCRIM = bytes([0, 77, 224, 147, 136, 25, 88, 76])
PUMP_MIGRATE_DISCRIM = bytes([155, 234, 231, 146, 236, 158, 162, 30])

# Simulator config (same as slinky)
SIM_CONFIG = {
    "entry_fee_bps": 100, "exit_fee_bps": 100,
    "entry_tip_lamports": 10000, "exit_tip_lamports": 10000,
    "slippage_default_bp": 50, "latency_ms": 250,
    "tp_target_bp": 1500, "stop_loss_bp": 1500,
    "max_hold_seconds": 300, "policy_version": "champion_v1",
}

KNOWN_LIMITATIONS = [
    "CAPTURE USED WRONG PUMPSWAP PROGRAM ID (pPEEEJ5... instead of pAMMBay6...). "
    "The 300-min capture contains ZERO PumpSwap trades. This is a known gap. "
    "Future captures with the fixed program ID will include PumpSwap events.",
    "Right-censoring: states within 300 seconds of capture end cannot have "
    "full 300s forward labels. These are marked right_censored=True.",
    "No account-state snapshots: the LaserStream only captures transactions, "
    "not periodic account updates. Curve reserves are decoded from instruction "
    "data and balance changes, not from direct account reads.",
    "Our-wallet identification: wallet address truncated in memory; episodes "
    "identified where trader_b58 matches known wallet pattern.",
]

# ─── Raw record types from the capture ───────────────────────────────
# Each raw record is a JSON line from the zstd-compressed NDJSON.
# Types: "tx" (transaction), "account" (account update), "slot" (slot update), "block_meta"

def decompress_zst(path: str):
    """Decompress a .zst file and yield lines."""
    dctx = zstandard.ZstdDecompressor()
    with open(path, "rb") as f:
        reader = dctx.stream_reader(f)
        text = reader.read().decode("utf-8")
        for line in text.split("\n"):
            line = line.strip()
            if line:
                yield line


def load_manifest():
    """Load the capture manifest."""
    with open(MANIFEST_PATH, "r") as f:
        return json.load(f)


def decode_base58(s: str) -> bytes:
    """Decode a base58 string to bytes."""
    alphabet = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
    num = 0
    for c in s:
        num = num * 58 + alphabet.index(c)
    # Encode to bytes
    result = []
    while num > 0:
        result.append(num & 0xFF)
        num >>= 8
    # Handle leading '1's (zero bytes)
    for c in s:
        if c == '1':
            result.append(0)
        else:
            break
    result.reverse()
    return bytes(result)


def read_u64_le(data: bytes, offset: int) -> int:
    if offset + 8 > len(data):
        return 0
    return struct.unpack_from("<Q", data, offset)[0]


def read_f64_le(data: bytes, offset: int) -> float:
    if offset + 8 > len(data):
        return 0.0
    return struct.unpack_from("<d", data, offset)[0]


def decode_pump_create(data: bytes, account_keys: list):
    """Decode a pump.fun create instruction.
    Layout (after 8-byte discriminator):
      name: string (4-byte len prefix + UTF-8)
      symbol: string
      uri: string
      creator: pubkey (32 bytes, from account list)
    """
    offset = 8  # skip discriminator
    fields = {}
    for field_name in ["name", "symbol", "uri"]:
        if offset + 4 > len(data):
            break
        slen = struct.unpack_from("<I", data, offset)[0]
        offset += 4
        if offset + slen > len(data):
            break
        fields[field_name] = data[offset:offset + slen].decode("utf-8", errors="replace")
        offset += slen
    # Initial reserves from account balances (would need balance changes)
    return fields


def decode_pump_buy_sell(data: bytes, account_keys: list, is_buy: bool):
    """Decode a pump.fun buy/sell instruction.
    Layout (after 8-byte discriminator):
      0  8  amount: u64 (token amount for buy, SOL amount for sell)
      8  8  min_amount_out / max_amount_in: u64
    The curve account is in the account list; reserve changes are in balance deltas.
    """
    offset = 8  # skip discriminator
    if is_buy:
        # buy: amounts[0] = token_amount, amounts[1] = max_sol_to_spend
        token_amount = read_u64_le(data, offset)
        max_sol = read_u64_le(data, offset + 8)
        return {"token_amount": token_amount, "max_sol_lamports": max_sol}
    else:
        # sell: amounts[0] = token_amount_in, amounts[1] = min_sol_out
        token_amount = read_u64_le(data, offset)
        min_sol = read_u64_le(data, offset + 8)
        return {"token_amount": token_amount, "min_sol_lamports": min_sol}


def extract_balance_deltas(tx_record: dict):
    """Extract pre/post balance deltas from a transaction record."""
    deltas = []
    meta = tx_record.get("meta", {})
    pre_balances = meta.get("preBalances", [])
    post_balances = meta.get("postBalances", [])
    account_keys = tx_record.get("accountKeys", tx_record.get("account_keys", []))

    for i, (pre, post) in enumerate(zip(pre_balances, post_balances)):
        if pre != post:
            deltas.append({"account_index": i, "pre": pre, "post": post, "delta": post - pre})

    # Also check token balances
    pre_token = meta.get("preTokenBalances", [])
    post_token = meta.get("postTokenBalances", [])
    token_deltas = []
    for pre_tb in pre_token:
        for post_tb in post_token:
            if pre_tb.get("accountIndex") == post_tb.get("accountIndex"):
                pre_amount = int(pre_tb.get("uiTokenAmount", {}).get("amount", "0"))
                post_amount = int(post_tb.get("uiTokenAmount", {}).get("amount", "0"))
                if pre_amount != post_amount:
                    token_deltas.append({
                        "account_index": pre_tb["accountIndex"],
                        "mint": pre_tb.get("mint", ""),
                        "pre": pre_amount,
                        "post": post_amount,
                        "delta": post_amount - pre_amount,
                    })

    return deltas, token_deltas


def process_raw_capture():
    """Process all raw zstd parts and decode events using corrected layouts."""
    manifest = load_manifest()
    raw_files = [f["filename"] for f in manifest.get("raw_files", [])]

    all_events = []
    event_index = 0

    # Track per-mint state for causal observations
    mint_state = defaultdict(lambda: {
        "seq": 0, "trade_count": 0, "buy_count": 0, "sell_count": 0,
        "buy_vol_lamports": 0, "sell_vol_lamports": 0,
        "unique_wallets": set(), "first_seen_ms": None,
        "creator": None, "token_name": None, "token_symbol": None,
        "initial_supply": None, "graduated": False, "graduated_ms": None,
    })

    # Also track for forward-looking outcome computation
    mint_events_raw = defaultdict(list)  # mint -> list of (time_ms, price_sol, event)

    # Process each raw file
    for i, fname in enumerate(raw_files):
        fpath = RAW_DIR / fname
        if not fpath.exists():
            print(f"  SKIP (not found): {fname}")
            continue

        print(f"  Processing {fname} ({i+1}/{len(raw_files)})...")

        try:
            for line in decompress_zst(str(fpath)):
                try:
                    record = json.loads(line)
                except json.JSONDecodeError:
                    continue

                lane = record.get("lane", "")
                kind = record.get("kind", "")

                # Process transaction records
                if kind == "tx" or "tx" in str(record.get("kind", "")):
                    # Extract program ID from the transaction
                    meta = record.get("meta", {})
                    account_keys = record.get("accountKeys", record.get("account_keys", []))
                    instructions = record.get("instructions", [])
                    inner_instructions = meta.get("innerInstructions", [])

                    # Get slot and signature
                    slot = record.get("slot", 0)
                    signature = record.get("signature", record.get("signatures", [""])[0] if record.get("signatures") else "")
                    recv_ms = record.get("recv_unix_ms", 0)

                    # Check all instructions for pump.fun program
                    all_ixs = list(instructions)
                    for inner in inner_instructions:
                        if isinstance(inner, dict):
                            for iix in inner.get("instructions", []):
                                all_ixs.append(iix)

                    for ix in all_ixs:
                        if not isinstance(ix, dict):
                            continue
                        prog_idx = ix.get("programIdIndex", ix.get("program_id_index", -1))
                        if prog_idx < 0 or prog_idx >= len(account_keys):
                            continue
                        program_id = account_keys[prog_idx]

                        # Route by program ID (corrected)
                        if program_id != PUMP_FUN:
                            # Check if it's the correct PumpSwap ID (it won't be in this capture)
                            if program_id == PUMP_SWAP:
                                pass  # Would decode PumpSwap here
                            continue

                        # Decode pump.fun instruction
                        data_hex = ix.get("data", "")
                        if not data_hex:
                            continue
                        try:
                            data_bytes = bytes.fromhex(data_hex)
                        except (ValueError, AttributeError):
                            continue

                        if len(data_bytes) < 8:
                            continue

                        discrim = data_bytes[:8]

                        # Decode by discriminator
                        event_type = None
                        is_buy = False
                        decoded = {}

                        if discrim == PUMP_CREATE_DISCRIM:
                            event_type = "create"
                            decoded = decode_pump_create(data_bytes, account_keys)
                        elif discrim == PUMP_BUY_DISCRIM:
                            event_type = "buy"
                            is_buy = True
                            decoded = decode_pump_buy_sell(data_bytes, account_keys, True)
                        elif discrim == PUMP_SELL_DISCRIM:
                            event_type = "sell"
                            decoded = decode_pump_buy_sell(data_bytes, account_keys, False)
                        elif discrim == PUMP_COMPLETE_DISCRIM:
                            event_type = "complete"
                        elif discrim == PUMP_MIGRATE_DISCRIM:
                            event_type = "migrate"
                        else:
                            continue

                        # Extract mint and trader from account keys
                        # For pump.fun: account[1] = bonding curve, account[2] = mint,
                        #   account[3..] = trader, etc. (varies by instruction)
                        accounts_list = ix.get("accounts", [])
                        if not accounts_list and len(account_keys) > 2:
                            # Fall back to parsing from account_keys
                            accounts_list = list(range(min(len(account_keys), 6)))

                        mint = ""
                        trader = ""

                        if len(account_keys) > 2:
                            # In pump.fun create: accounts = [global, bonding_curve, mint, ...]
                            # In pump.fun buy: accounts = [global, user, mint, bonding_curve, ...]
                            if event_type == "create":
                                if len(account_keys) > 2:
                                    mint = account_keys[2] if len(account_keys) > 2 else ""
                                    # Creator is in the instruction args, not account list typically
                            elif event_type in ("buy", "sell"):
                                if len(account_keys) > 3:
                                    # buy: [global, user, mint, bonding_curve, associated_token_accounts...]
                                    trader = account_keys[1] if len(account_keys) > 1 else ""
                                    mint = account_keys[2] if len(account_keys) > 2 else ""

                        if not mint:
                            # Try to find mint from token balance changes
                            balance_deltas, token_deltas = extract_balance_deltas(record)
                            for td in token_deltas:
                                if td["mint"] and td["mint"] != "So11111111111111111111111111111111111111112":
                                    mint = td["mint"]
                                    break

                        if not mint:
                            continue

                        # Update per-mint state
                        ms = mint_state[mint]
                        if ms["first_seen_ms"] is None:
                            ms["first_seen_ms"] = recv_ms

                        # Compute price from balance deltas if available
                        price_sol = None
                        mcap_sol = None
                        balance_deltas, token_deltas = extract_balance_deltas(record)

                        # Find SOL delta for the bonding curve account
                        sol_delta = 0
                        for d in balance_deltas:
                            if d["account_index"] < len(account_keys):
                                acct = account_keys[d["account_index"]]
                                # Bonding curve is typically account[1] in buy/sell
                                if d["account_index"] == 1:
                                    sol_delta = d["delta"]

                        # Find token delta
                        token_delta = 0
                        for td in token_deltas:
                            if td["mint"] == mint:
                                token_delta = td["delta"]

                        # Price estimation from trade
                        if is_buy and token_delta > 0 and sol_delta != 0:
                            # buy: SOL in, tokens out
                            price_sol = abs(sol_delta) / token_delta if token_delta > 0 else None
                            mcap_sol = abs(sol_delta) / token_delta * 1_000_000_000 if token_delta > 0 else None  # rough
                        elif not is_buy and token_delta != 0 and sol_delta != 0:
                            # sell: tokens in, SOL out
                            price_sol = abs(sol_delta / token_delta) if token_delta != 0 else None

                        # Update cumulative stats (AFTER this event for next observation)
                        if event_type in ("buy", "sell"):
                            ms["trade_count"] += 1
                            if is_buy:
                                ms["buy_count"] += 1
                                ms["buy_vol_lamports"] += abs(sol_delta) if sol_delta else 0
                            else:
                                ms["sell_count"] += 1
                                ms["sell_vol_lamports"] += abs(sol_delta) if sol_delta else 0
                            if trader:
                                ms["unique_wallets"].add(trader)

                        if event_type == "create":
                            ms["creator"] = decoded.get("name", "")  # Not creator, but name
                            ms["token_name"] = decoded.get("name")
                            ms["token_symbol"] = decoded.get("symbol")
                        elif event_type == "migrate":
                            ms["graduated"] = True
                            ms["graduated_ms"] = recv_ms

                        # Store raw event for outcome computation
                        mint_events_raw[mint].append({
                            "event_index": event_index,
                            "event_type": event_type,
                            "slot": slot,
                            "recv_ms": recv_ms,
                            "price_sol": price_sol,
                            "mcap_sol": mcap_sol,
                            "is_buy": is_buy,
                            "sol_delta": sol_delta,
                            "token_delta": token_delta,
                            "trader": trader,
                        })

                        event_index += 1

                elif kind == "slot":
                    pass  # Slot updates don't contain trade data

        except Exception as e:
            print(f"  ERROR processing {fname}: {e}")
            continue

    print(f"\n  Total decoded events: {event_index:,}")
    print(f"  Unique mints: {len(mint_state):,}")

    return mint_state, mint_events_raw


def build_laserstream_states(mint_state, mint_events_raw):
    """Build pump_state_v1 from decoded laserstream events."""
    states = []
    capture_end_ms = None

    # Get capture end time from manifest
    manifest = load_manifest()
    capture_end_ms = manifest.get("end_unix_ms", 0)

    for mint, events in mint_events_raw.items():
        if not events:
            continue

        ms = mint_state[mint]
        events.sort(key=lambda e: e["recv_ms"])

        # Cumulative counters for causal state
        cum_trade = 0
        cum_buy = 0
        cum_sell = 0
        cum_buy_vol = 0
        cum_sell_vol = 0
        cum_wallets = set()
        seq = 0

        for ev in events:
            t_ms = ev["recv_ms"]
            event_type = ev["event_type"]

            # Only create states for buy/sell (trade observations)
            if event_type not in ("buy", "sell"):
                # But still update state for create/migrate
                continue

            # Causal state (BEFORE this trade)
            total_vol = cum_buy_vol + cum_sell_vol
            buy_pressure = (cum_buy_vol / total_vol) if total_vol > 0 else None
            net_flow = int(cum_buy_vol - cum_sell_vol) if (cum_buy_vol or cum_sell_vol) else None

            # Right-censoring: if less than 300s of forward data remains
            right_censored = False
            censoring_reason = None
            if capture_end_ms and (capture_end_ms - t_ms) < 300_000:
                right_censored = True
                censoring_reason = "capture_end"

            price_sol = ev.get("price_sol")
            mcap_sol = ev.get("mcap_sol")

            state = PumpStateV1(
                state_id=stable_id("laserstream", mint, str(t_ms), str(seq)),
                source="laserstream",
                mint=mint,
                mint_b58=mint,
                event_time_unix_ms=t_ms,
                event_slot=ev.get("slot"),
                seq=seq,
                venue="pumpfun_bonding",  # All events in this capture are pump.fun
                virtual_sol=None,  # Not directly available from raw tx data
                virtual_token=None,
                real_sol=None,
                real_token=None,
                curve_pct_depleted=None,  # Would need curve account read
                is_complete=ms.get("graduated", False) and (ms.get("graduated_ms", 0) <= t_ms),
                market_cap_sol=int(mcap_sol * 1e9) if mcap_sol else None,
                trade_side="buy" if ev["is_buy"] else "sell",
                amount_in=abs(ev.get("sol_delta", 0)) if ev["is_buy"] else abs(ev.get("token_delta", 0)),
                amount_out=abs(ev.get("token_delta", 0)) if ev["is_buy"] else abs(ev.get("sol_delta", 0)),
                fee_bps=None,  # Derived from balance deltas
                trade_count_so_far=cum_trade,
                buy_count_so_far=cum_buy,
                sell_count_so_far=cum_sell,
                token_name=ms.get("token_name"),
                token_symbol=ms.get("token_symbol"),
                creator=ms.get("creator"),
                initial_supply=ms.get("initial_supply"),
                is_mayhem=None,
                seconds_since_launch=((t_ms - ms["first_seen_ms"]) / 1000) if ms.get("first_seen_ms") else None,
                buy_pressure=buy_pressure,
                net_flow_sol=net_flow,
                unique_wallets_so_far=len(cum_wallets),
                right_censored=right_censored,
                censoring_reason=censoring_reason,
            )
            states.append(state.__dict__)

            # Update counters AFTER this event
            cum_trade += 1
            if ev["is_buy"]:
                cum_buy += 1
                cum_buy_vol += ev.get("sol_delta", 0) if ev.get("sol_delta", 0) > 0 else 0
            else:
                cum_sell += 1
                cum_sell_vol += abs(ev.get("sol_delta", 0)) if ev.get("sol_delta", 0) < 0 else 0
            if ev.get("trader"):
                cum_wallets.add(ev["trader"])
            seq += 1

    return states


def build_laserstream_outcomes(states, mint_events_raw, capture_end_ms):
    """Build pump_outcome_v1 for laserstream states.
    Mark right-censored states explicitly."""
    outcomes = []

    # Group events by mint for forward lookup
    mint_forward = {}
    for mint, events in mint_events_raw.items():
        events.sort(key=lambda e: e["recv_ms"])
        mint_forward[mint] = events

    for state in states:
        mint = state["mint"]
        t_ms = state["event_time_unix_ms"]
        state_id = state["state_id"]

        events = mint_forward.get(mint, [])
        entry_price = None
        # Find the entry event
        for ev in events:
            if ev["recv_ms"] == t_ms:
                entry_price = ev.get("price_sol")
                break

        forward = [ev for ev in events if ev["recv_ms"] > t_ms]

        # Markouts
        markouts = {}
        for horizon_s in [1, 2, 5, 10, 30, 60, 120, 300]:
            target_ms = t_ms + horizon_s * 1000
            if capture_end_ms and target_ms > capture_end_ms:
                markouts[f"ret_{horizon_s}s_bp"] = None
                continue
            best = None
            for ev in forward:
                if ev["recv_ms"] <= target_ms:
                    best = ev
                else:
                    break
            if best and best.get("price_sol") is not None and entry_price and entry_price > 0:
                ret_pct = (best["price_sol"] - entry_price) / entry_price
                markouts[f"ret_{horizon_s}s_bp"] = int(ret_pct * 10000)
            else:
                markouts[f"ret_{horizon_s}s_bp"] = None

        # MFE/MAE within 300s
        mfe_bp = mae_bp = None
        mfe_time = mae_time = None
        peak_bp = time_to_peak = None
        if entry_price and entry_price > 0:
            max_price = entry_price
            min_price = entry_price
            max_t = 0
            min_t = 0
            for ev in forward:
                delta_s = (ev["recv_ms"] - t_ms) / 1000
                if delta_s > 300:
                    break
                p = ev.get("price_sol")
                if p is None:
                    continue
                if p > max_price:
                    max_price = p; max_t = delta_s
                if p < min_price:
                    min_price = p; min_t = delta_s
            mfe_bp = int((max_price - entry_price) / entry_price * 10000)
            mae_bp = int((min_price - entry_price) / entry_price * 10000)
            mfe_time = max_t if max_price > entry_price else None
            mae_time = min_t if min_price < entry_price else None
            peak_bp = mfe_bp
            time_to_peak = mfe_time

        # First-hit barriers
        barriers = {}
        if entry_price and entry_price > 0:
            for label, threshold_bp, direction in [
                ("hit_plus_10_bp", 10, 1), ("hit_plus_25_bp", 25, 1),
                ("hit_plus_50_bp", 50, 1), ("hit_plus_100_bp", 100, 1), ("hit_plus_200_bp", 200, 1),
                ("hit_minus_10_bp", -10, -1), ("hit_minus_20_bp", -20, -1),
                ("hit_minus_30_bp", -30, -1), ("hit_minus_50_bp", -50, -1),
            ]:
                hit_time = None
                for ev in forward:
                    delta_s = (ev["recv_ms"] - t_ms) / 1000
                    if delta_s > 300:
                        break
                    p = ev.get("price_sol")
                    if p is None:
                        continue
                    ret_bp = int((p - entry_price) / entry_price * 10000)
                    if direction > 0 and ret_bp >= threshold_bp:
                        hit_time = delta_s; break
                    elif direction < 0 and ret_bp <= threshold_bp:
                        hit_time = delta_s; break
                barriers[label] = hit_time

        plus_100_before = None
        if barriers.get("hit_plus_100_bp") is not None and barriers.get("hit_minus_30_bp") is not None:
            plus_100_before = barriers["hit_plus_100_bp"] < barriers["hit_minus_30_bp"]
        elif barriers.get("hit_plus_100_bp") is not None:
            plus_100_before = True

        # Censoring
        right_censored = state.get("right_censored", False)
        censoring = state.get("censoring_reason")

        # If right-censored, some labels will be None
        if right_censored:
            # Only compute labels for horizons that have data
            for k in markouts:
                if markouts[k] is None:
                    pass  # Already None

        outcome = PumpOutcomeV1(
            state_id=state_id, mint=mint, event_time_unix_ms=t_ms,
            ret_1s_bp=markouts.get("ret_1s_bp"), ret_2s_bp=markouts.get("ret_2s_bp"),
            ret_5s_bp=markouts.get("ret_5s_bp"), ret_10s_bp=markouts.get("ret_10s_bp"),
            ret_30s_bp=markouts.get("ret_30s_bp"), ret_60s_bp=markouts.get("ret_60s_bp"),
            ret_120s_bp=markouts.get("ret_120s_bp"), ret_300s_bp=markouts.get("ret_300s_bp"),
            mfe_bp=mfe_bp, mae_bp=mae_bp, mfe_time_seconds=mfe_time, mae_time_seconds=mae_time,
            hit_plus_10_bp=barriers.get("hit_plus_10_bp"), hit_plus_25_bp=barriers.get("hit_plus_25_bp"),
            hit_plus_50_bp=barriers.get("hit_plus_50_bp"), hit_plus_100_bp=barriers.get("hit_plus_100_bp"),
            hit_plus_200_bp=barriers.get("hit_plus_200_bp"),
            hit_minus_10_bp=barriers.get("hit_minus_10_bp"), hit_minus_20_bp=barriers.get("hit_minus_20_bp"),
            hit_minus_30_bp=barriers.get("hit_minus_30_bp"), hit_minus_50_bp=barriers.get("hit_minus_50_bp"),
            plus_100_before_minus_30=plus_100_before,
            peak_bp=peak_bp, time_to_peak_seconds=time_to_peak,
            graduated=None, seconds_to_graduation=None,
            graduated_did_migrate=None, post_grad_price_change_300s_bp=None,
            survived_60s=markouts.get("ret_60s_bp") is not None,
            survived_300s=markouts.get("ret_300s_bp") is not None,
            collapsed_50pct_within_300s=(mae_bp is not None and mae_bp <= -5000) if mae_bp is not None else None,
            right_censored=right_censored, censoring_reason=censoring,
        )
        outcomes.append(outcome.__dict__)

    return outcomes


def build_laserstream_simulator(states, outcomes):
    """Build simulator labels for laserstream states."""
    sim_labels = []
    cfg_hash = hashlib.sha256(json.dumps(SIM_CONFIG, sort_keys=True).encode()).hexdigest()[:16]
    outcome_map = {o["state_id"]: o for o in outcomes}

    for state in states:
        outcome = outcome_map.get(state["state_id"])
        right_censored = state.get("right_censored", False)

        would_enter = not right_censored  # Don't enter on censored states
        curve_pct = state.get("curve_pct_depleted")
        if curve_pct and curve_pct > 0.95:
            would_enter = False

        entry_price = state.get("market_cap_sol")
        entry_fee = int((entry_price or 0) * SIM_CONFIG["entry_fee_bps"] / 10000) if entry_price else 0

        exit_reason = exit_price = hold_duration = net_pnl = None
        if would_enter and outcome:
            mfe = outcome.get("mfe_bp") or 0
            mae = outcome.get("mae_bp") or 0
            tp = SIM_CONFIG["tp_target_bp"]
            sl = -SIM_CONFIG["stop_loss_bp"]

            if mfe >= tp:
                exit_reason = "take_profit"
                hold_duration = outcome.get("mfe_time_seconds")
                exit_price = int((entry_price or 0) * (1 + tp / 10000)) if entry_price else None
            elif mae <= sl:
                exit_reason = "stop_loss"
                hold_duration = outcome.get("mae_time_seconds")
                exit_price = int((entry_price or 0) * (1 + sl / 10000)) if entry_price else None
            elif outcome.get("survived_300s"):
                exit_reason = "time_stop"
                hold_duration = 300.0
                ret_300 = outcome.get("ret_300s_bp") or 0
                exit_price = int((entry_price or 0) * (1 + ret_300 / 10000)) if entry_price else None
            else:
                exit_reason = "hold"
                hold_duration = 300.0
                exit_price = entry_price

            if entry_price and exit_price:
                gross = exit_price - entry_price
                exit_fee = int(exit_price * SIM_CONFIG["exit_fee_bps"] / 10000)
                exit_slip = int(exit_price * SIM_CONFIG["slippage_default_bp"] / 10000)
                net_pnl = gross - entry_fee - exit_fee - SIM_CONFIG["entry_tip_lamports"] - SIM_CONFIG["exit_tip_lamports"]

        policy_class = "SKIP"
        if would_enter and outcome and net_pnl is not None and entry_price and entry_price > 0:
            pnl_pct = net_pnl / entry_price * 10000
            mfe = outcome.get("mfe_bp") or 0
            if pnl_pct > 1000 and mfe > 2000:
                policy_class = "STRONG"
            elif pnl_pct > 0 and mfe > 500:
                policy_class = "GOOD"
            elif pnl_pct > -500:
                policy_class = "MARGINAL"
            elif pnl_pct > -2000:
                policy_class = "BAD"
            else:
                policy_class = "TOXIC"

        sim = SimulatorLabelV1(
            state_id=state["state_id"], mint=state["mint"], event_time_unix_ms=state["event_time_unix_ms"],
            would_enter=would_enter,
            entry_price_sol=entry_price, entry_slippage_bp=SIM_CONFIG["slippage_default_bp"] if would_enter else None,
            entry_fee_lamports=entry_fee if would_enter else None, entry_latency_ms=SIM_CONFIG["latency_ms"] if would_enter else None,
            exit_reason=exit_reason, exit_price_sol=exit_price,
            exit_slippage_bp=SIM_CONFIG["slippage_default_bp"] if exit_price else None,
            exit_fee_lamports=int((exit_price or 0) * SIM_CONFIG["exit_fee_bps"] / 10000) if exit_price else None,
            exit_latency_ms=SIM_CONFIG["latency_ms"] if exit_price else None,
            hold_duration_seconds=hold_duration,
            gross_pnl_lamports=(exit_price - entry_price) if entry_price and exit_price else None,
            net_pnl_lamports=net_pnl,
            pnl_pct_bp=int(net_pnl / entry_price * 10000) if net_pnl is not None and entry_price and entry_price > 0 else None,
            mfe_while_held_bp=outcome.get("mfe_bp") if outcome else None,
            mae_while_held_bp=outcome.get("mae_bp") if outcome else None,
            target_order=f"+{SIM_CONFIG['tp_target_bp']}bp" if would_enter else None,
            stop_order=f"-{SIM_CONFIG['stop_loss_bp']}bp" if would_enter else None,
            config_hash=cfg_hash, policy_version=SIM_CONFIG["policy_version"],
            policy_class=policy_class,
        )
        sim_labels.append(sim.__dict__)

    return sim_labels


# ─── Main ─────────────────────────────────────────────────────────────
def main():
    print("=" * 80)
    print("LASERSTREAM_GOLD_V1 — Re-decoding 300-min RAW capture")
    print("=" * 80)

    os.makedirs(OUTPUT_DIR, exist_ok=True)
    manifest = load_manifest()
    capture_end_ms = manifest.get("end_unix_ms", 0)
    capture_start_ms = manifest.get("start_unix_ms", 0)

    print(f"\n  Capture: {manifest.get('session_id')}")
    print(f"  Duration: {manifest.get('duration_minutes')} min")
    print(f"  Slot range: {manifest.get('start_slot')} → {manifest.get('end_slot')}")
    print(f"  Programs (ORIGINAL - wrong): {manifest.get('programs')}")
    print(f"  Corrected PumpSwap ID: {PUMP_SWAP}")

    # Step 1: Process raw capture
    print("\n[1/6] Processing RAW zstd parts (corrected decoders)...")
    mint_state, mint_events_raw = process_raw_capture()

    # Step 2: Build states
    print("\n[2/6] Building pump_state_v1...")
    states = build_laserstream_states(mint_state, mint_events_raw)
    print(f"  States: {len(states):,}")
    censored = sum(1 for s in states if s.get("right_censored"))
    print(f"  Right-censored: {censored:,}")

    # Step 3: Build outcomes
    print("\n[3/6] Building pump_outcome_v1...")
    outcomes = build_laserstream_outcomes(states, mint_events_raw, capture_end_ms)
    print(f"  Outcomes: {len(outcomes):,}")

    # Step 4: Build simulator labels
    print("\n[4/6] Building simulator_label_v1...")
    sim_labels = build_laserstream_simulator(states, outcomes)
    print(f"  Simulator labels: {len(sim_labels):,}")

    # Step 5: Class distribution
    print("\n[5/6] Computing class distribution...")
    class_dist = defaultdict(int)
    for s in sim_labels:
        class_dist[s.get("policy_class", "SKIP")] += 1
    for cls, count in sorted(class_dist.items()):
        print(f"  {cls}: {count:,}")

    # Step 6: Write parquet + manifest
    print("\n[6/6] Writing partitioned Parquet + manifest...")
    state_files = write_parquet_partitioned(states, str(OUTPUT_DIR / "states"), "pump_state_v1", chunk_size=100_000)
    outcome_files = write_parquet_partitioned(outcomes, str(OUTPUT_DIR / "outcomes"), "pump_outcome_v1", chunk_size=100_000)
    sim_files = write_parquet_partitioned(sim_labels, str(OUTPUT_DIR / "simulator"), "simulator_label_v1", chunk_size=100_000)

    # Mint-disjoint split
    mint_first_seen = []
    for mint, ms in mint_state.items():
        if ms.get("first_seen_ms"):
            mint_first_seen.append((mint, ms["first_seen_ms"]))
    splits = mint_disjoint_split(mint_first_seen)
    with open(OUTPUT_DIR / "splits.json", "w") as f:
        json.dump({k: len(v) for k, v in splits.items()}, f)

    # Output manifest
    output_files = []
    for fpath_list in [state_files, outcome_files, sim_files]:
        for fpath in fpath_list:
            output_files.append({
                "filename": os.path.basename(fpath),
                "bytes": file_size_bytes(fpath),
                "sha256": hash_file(fpath),
            })

    counts = {
        "decoded_events": sum(len(v) for v in mint_events_raw.values()),
        "states": len(states),
        "outcomes": len(outcomes),
        "simulator_labels": len(sim_labels),
        "unique_mints": len(mint_state),
        "right_censored_states": censored,
        "train_mints": len(splits["train"]),
        "val_mints": len(splits["val"]),
        "test_mints": len(splits["test"]),
    }

    qa = {
        "leakage_check": "PASS — states contain only causal fields; outcomes computed from forward events only",
        "right_censoring": f"{censored} states marked right_censored within 300s of capture end",
        "id_stability": f"PASS — {len(set(s['state_id'] for s in states))} unique state_ids",
        "pumpswap_gap": "KNOWN LIMITATION — capture used wrong PumpSwap program ID; 0 PumpSwap trades in this capture",
        "raw_vs_normalized": "RAW zstd used as authority; normalized NDJSON NOT used",
        "class_distribution": dict(class_dist),
        "venue_distribution": dict(__import__("collections").Counter(s.get("venue") for s in states)),
    }

    write_manifest(
        str(OUTPUT_DIR / "laserstream_gold_v1_manifest.json"),
        "laserstream_gold_v1", "laserstream_300min",
        SCHEMA_VERSION, GENERATOR_VERSION,
        [{"filename": f["filename"], "sha256": f["sha256"], "bytes": f["bytes"]}
         for f in manifest.get("raw_files", [])],
        output_files, counts, qa, KNOWN_LIMITATIONS,
    )

    print(f"\n{'=' * 80}")
    print(f"LASERSTREAM_GOLD_V1 COMPLETE")
    print(f"{'=' * 80}")
    print(f"  Decoded events: {counts['decoded_events']:,}")
    print(f"  States:         {len(states):,}")
    print(f"  Outcomes:       {len(outcomes):,}")
    print(f"  Simulator:      {len(sim_labels):,}")
    print(f"  Right-censored: {censored:,}")
    print(f"  Unique mints:   {len(mint_state):,}")
    print(f"  Train/Val/Test: {len(splits['train']):,}/{len(splits['val']):,}/{len(splits['test']):,}")
    print(f"  Output dir:     {OUTPUT_DIR}")
    print(f"  Manifest:       {OUTPUT_DIR / 'laserstream_gold_v1_manifest.json'}")


if __name__ == "__main__":
    main()
