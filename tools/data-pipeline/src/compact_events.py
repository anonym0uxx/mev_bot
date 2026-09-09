#!/usr/bin/env python
"""compact_events.py — compress a day's pump_event_v1 events .ndjson.zst -> Parquet.

Usage: python compact_events.py <events.ndjson.zst> <output.parquet>

Streams the zstd NDJSON written by pq-laserstream-grpc (schema = PumpEventV1 in
normalizer.rs), projects the scalar fields into a typed Arrow schema, serializes the
complex Vec/JSON fields as JSON strings, and writes zstd + dictionary-encoded Parquet.
Batched to bound memory on multi-million-event days.

This is the daily compaction step of the tiered storage plan: raw (ring buffer) ->
events .ndjson.zst (streaming buffer) -> this -> Parquet (the queryable product).
"""
import sys, json, io
import zstandard as zstd
import pyarrow as pa
import pyarrow.parquet as pq

BATCH = 200_000

# Scalar fields (name -> Arrow type), mirroring PumpEventV1 in normalizer.rs.
SCALARS = {
    "event_index": pa.uint64(),
    "event_type": pa.string(),
    "venue": pa.string(),
    "slot": pa.uint64(),
    "tx_index": pa.uint64(),
    "signature_b58": pa.string(),
    "raw_hash": pa.string(),
    "recv_unix_ms": pa.uint64(),
    "is_live": pa.bool_(),
    "mint_b58": pa.string(),
    "trader_b58": pa.string(),
    "creator_b58": pa.string(),
    "curve_account_b58": pa.string(),
    "virtual_sol": pa.uint64(),
    "virtual_token": pa.uint64(),
    "real_sol": pa.uint64(),
    "real_token": pa.uint64(),
    "curve_complete": pa.bool_(),
    "mayhem": pa.bool_(),
    "cashback": pa.bool_(),
    "pool_account_b58": pa.string(),
    "base_reserve": pa.uint64(),
    "quote_reserve": pa.uint64(),
    "lp_supply": pa.uint64(),
    "amount_in": pa.uint64(),
    "amount_out": pa.uint64(),
    "min_amount_out": pa.uint64(),
    "max_amount_in": pa.uint64(),
    "fee_bps": pa.uint32(),
    "trade_side": pa.string(),
    "initial_supply": pa.uint64(),
    "initial_virtual_sol": pa.uint64(),
    "initial_virtual_token": pa.uint64(),
    "fee_lamports": pa.uint64(),
    "cu_consumed": pa.uint64(),
    "tx_status": pa.string(),
    "err_hex": pa.string(),
    "token_name": pa.string(),
    "token_symbol": pa.string(),
    "token_uri": pa.string(),
    "decimals": pa.uint32(),
    "is_our_wallet": pa.bool_(),
}

# Complex fields serialized to JSON strings (kept as opaque strings, recoverable).
# NOTE: these are ~90% of the uncompressed event volume (log_messages ~36%, token
# balances ~28%, inner instructions ~15%, account keys ~8%) and are REDUNDANT with
# the raw lossless capture. They are excluded by default; pass --full to include
# them when deep reconstruction from events (not raw) is required.
JSON_FIELDS = [
    "pre_sol_balances",
    "post_sol_balances",
    "pre_token_balances_json",
    "post_token_balances_json",
    "inner_instructions_json",
    "log_messages",
    "account_keys_b58",
]


def iter_events(path):
    """Yield dicts from a zstd-compressed NDJSON file (streaming)."""
    with open(path, "rb") as fh:
        dctx = zstd.ZstdDecompressor()
        with dctx.stream_reader(fh) as reader:
            buf = io.TextIOWrapper(reader, encoding="utf-8")
            for line in buf:
                line = line.strip()
                if line:
                    yield json.loads(line)


def build_schema(include_json):
    cols = [(name, typ) for name, typ in SCALARS.items()]
    if include_json:
        cols += [(name, pa.string()) for name in JSON_FIELDS]
    return pa.schema(cols)


def row(event, include_json):
    """Project one event into a schema-aligned dict."""
    r = {name: event.get(name) for name in SCALARS}
    if include_json:
        for name in JSON_FIELDS:
            r[name] = json.dumps(event.get(name)) if event.get(name) is not None else None
    return r


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    include_json = "--full" in sys.argv
    src, dst = args[0], args[1]
    schema = build_schema(include_json)
    writer = None
    batch = []
    total = 0

    for ev in iter_events(src):
        batch.append(row(ev, include_json))
        total += 1
        if len(batch) >= BATCH:
            table = pa.Table.from_pylist(batch, schema=schema)
            if writer is None:
                writer = pq.ParquetWriter(
                    dst, schema, compression="zstd", compression_level=3,
                    use_dictionary=True,
                )
            writer.write_table(table)
            batch = []

    if batch:
        table = pa.Table.from_pylist(batch, schema=schema)
        if writer is None:
            writer = pq.ParquetWriter(
                dst, schema, compression="zstd", compression_level=3,
                use_dictionary=True,
            )
        writer.write_table(table)

    if writer is not None:
        writer.close()
    else:
        # empty input: still write an empty Parquet with the schema
        pq.write_table(pa.Table.from_pylist([], schema=schema), dst,
                       compression="zstd")

    print(f"compacted {total} events ({'full' if include_json else 'scalar-only'}) -> {dst}")


if __name__ == "__main__":
    main()
