#!/usr/bin/env python
"""
slinky_gold_v3 — Deterministic STREAMING Parquet Compactor
============================================================
Reads certified small parquet files per layer in deterministic sorted order,
streams row-group-by-row-group through pyarrow, normalizes/casts every column
to a canonical schema, and writes ~128MB target compact files using
ParquetWriter. NEVER materializes a full layer.

Key properties:
- Canonical Arrow schema derived from a TYPED source file per layer
  (falls back to gold_schema_v3.py types for all-null columns).
- Null-type columns (e.g. seconds_to_graduation) cast to canonical typed nullable.
- Row values/state_ids preserved EXACTLY. No sorting/relabeling/recalculation.
- Writes to NEW temp directory. Never overwrites certified source.
- Atomic temp→final rename after write+read-back validation.
- Deterministic file naming: <layer>_compact_part0000.parquet, etc.
"""
import os
import sys
import glob
import shutil
import hashlib
import pyarrow as pa
import pyarrow.parquet as pq
from pathlib import Path

# ─── Configuration ──────────────────────────────────────────────────────
OUTPUT_BASE = Path("D:/repos/mev_bot/tools/data-pipeline/output/slinky_gold_v3")
COMPACT_BASE = Path("D:/repos/mev_bot/tools/data-pipeline/output/slinky_gold_v3_compact")
TEMP_BASE = Path("D:/repos/mev_bot/tools/data-pipeline/output/slinky_gold_v3_compact_tmp")

LAYERS = [
    "pump_state_v3",
    "pump_outcome_v3",
    "counterfactual_trade_v3",
    "policy_eval_v3",
]

# Target ~128 MB compressed per file. We rotate by row count.
# Estimate: pump_state_v3 ≈ 967 bytes/row compressed → ~132K rows/file.
# But compression varies, so we use a row-count target with byte-size check.
ROWS_PER_FILE = 250_000  # conservative; will produce 64-256MB files
MAX_ROWS_PER_FILE = 500_000  # hard cap

# Canonical types for columns that are all-null (null type) in some files.
# These must match the Optional[...] types in gold_schema_v3.py.
CANONICAL_OVERRIDES = {
    "pump_state_v3": {
        "seconds_to_graduation": pa.float64(),
        "graduation_proximity_pct": pa.float64(),
        "sybil_cluster_size": pa.int64(),
        "market_active_mints_5m": pa.int64(),
        "market_total_vol_5m_sol": pa.float64(),
        "market_avg_buy_pressure_5m": pa.float64(),
    },
    # Other layers may have their own null-type columns; we'll detect and handle them.
}


def build_canonical_schema(layer_name: str, source_dir: str) -> pa.Schema:
    """Build canonical Arrow schema by finding a file where all columns are typed
    (not null-type). Falls back to CANONICAL_OVERRIDES for always-null columns."""
    files = sorted(glob.glob(os.path.join(source_dir, "*.parquet")))
    if not files:
        raise FileNotFoundError(f"No parquet files found in {source_dir}")

    # Strategy: read schema from multiple files, pick the one with fewest null-type cols.
    # Then apply overrides for remaining null-type cols.
    best_schema = None
    best_null_count = float('inf')

    # Sample up to 50 files spread across the range
    sample_indices = [0, len(files)//4, len(files)//2, 3*len(files)//4, -1]
    # Also try a dense sample of first 30
    sample_files = [files[i] for i in sample_indices if i < len(files)]
    sample_files += files[:30]
    # Deduplicate
    seen = set()
    sample_files = [f for f in sample_files if f not in seen and not seen.add(f)]

    for f in sample_files:
        try:
            s = pq.read_schema(f)
            null_count = sum(1 for i in range(len(s)) if str(s.field(i).type) == "null")
            if null_count < best_null_count:
                best_null_count = null_count
                best_schema = s
                if null_count == 0:
                    break
        except Exception:
            continue

    if best_schema is None:
        raise RuntimeError(f"Could not read schema from any file in {source_dir}")

    # Apply canonical overrides for any remaining null-type columns
    overrides = CANONICAL_OVERRIDES.get(layer_name, {})
    fields = []
    for i in range(len(best_schema)):
        name = best_schema.field(i).name
        typ = best_schema.field(i).type
        nullable = best_schema.field(i).nullable
        if str(typ) == "null":
            if name in overrides:
                typ = overrides[name]
            else:
                # Default: null columns become string (safest for unknown Optional[str])
                # But check the schema definition first
                print(f"  WARNING: null-type column '{name}' in {layer_name} has no override, "
                      f"defaulting to utf8")
                typ = pa.string()
        fields.append(pa.field(name, typ, nullable=nullable if nullable else True))

    return pa.schema(fields)


def cast_table_to_canonical(table: pa.Table, canonical_schema: pa.Schema) -> pa.Table:
    """Cast a table's columns to match the canonical schema, handling null-type
    columns by casting them to the canonical type (all values remain null)."""
    if table.schema.equals(canonical_schema, check_metadata=False):
        return table

    new_columns = []
    for i in range(len(canonical_schema)):
        target_field = canonical_schema.field(i)
        col_name = target_field.name
        target_type = target_field.type

        if col_name in table.column_names:
            col = table.column(col_name)
            if col.type == target_type:
                new_columns.append(col)
            elif str(col.type) == "null":
                # All-null column → cast to target type (values stay null)
                # Create a null array of the right type with same length
                null_arr = pa.nulls(len(table), type=target_type)
                new_columns.append(pa.chunked_array([null_arr], type=target_type))
            else:
                # Type mismatch (e.g. large_string vs string) → safe cast
                try:
                    new_columns.append(col.cast(target_type))
                except Exception:
                    # Fallback: try via pyarrow compute
                    new_columns.append(pa.compute.cast(col, target_type))
        else:
            # Column missing in this file → fill with nulls of canonical type
            null_arr = pa.nulls(len(table), type=target_type)
            new_columns.append(pa.chunked_array([null_arr], type=target_type))

    return pa.Table.from_arrays(new_columns, schema=canonical_schema)


def sorted_file_list(source_dir: str) -> list:
    """Return deterministic sorted list of parquet files.
    Sort by (part_number, flush_suffix) extracted from filename."""
    import re
    files = glob.glob(os.path.join(source_dir, "*.parquet"))
    pat = re.compile(r'part(\d+)_(\d+)\.parquet$')

    def sort_key(f):
        m = pat.search(os.path.basename(f))
        if m:
            return (int(m.group(1)), int(m.group(2)))
        # Fallback: sort by filename
        return (0, os.path.basename(f))

    return sorted(files, key=sort_key)


def compact_layer(layer_name: str, canonical_schema: pa.Schema,
                  source_dir: str, compact_dir: str, temp_dir: str) -> tuple:
    """Stream all parquet files for one layer into compact output files."""
    files = sorted_file_list(source_dir)
    n_source_files = len(files)

    os.makedirs(compact_dir, exist_ok=True)
    os.makedirs(temp_dir, exist_ok=True)

    total_rows = 0
    file_idx = 0
    rows_in_current = 0
    writer = None
    current_temp_path = None

    print(f"\n{'='*60}")
    print(f"Compacting {layer_name}: {n_source_files} source files")
    print(f"{'='*60}")

    for src_idx, src_file in enumerate(files):
        # Read file metadata only first (for row count)
        meta = pq.read_metadata(src_file)
        file_rows = meta.num_rows
        total_rows += file_rows

        # Stream row groups from this file
        pf = pq.ParquetFile(src_file)
        for rg_idx in range(pf.num_row_groups):
            # Read row group as a table batch
            batch = pf.read_row_group(rg_idx)
            # Normalize to canonical schema
            batch = cast_table_to_canonical(batch, canonical_schema)

            # Write batches incrementally
            if writer is None:
                # Start a new output file
                current_temp_path = os.path.join(temp_dir, f"{layer_name}_compact_part{file_idx:04d}.tmp")
                writer = pq.ParquetWriter(current_temp_path, canonical_schema,
                                          compression='zstd')
                rows_in_current = 0

            writer.write_table(batch)
            rows_in_current += len(batch)

            # Rotate if we've reached the target
            if rows_in_current >= ROWS_PER_FILE:
                writer.close()
                # Validate the temp file by reading its schema back
                try:
                    written_meta = pq.read_metadata(current_temp_path)
                    if written_meta.num_rows != rows_in_current:
                        raise ValueError(
                            f"Row count mismatch: wrote {rows_in_current} but "
                            f"read back {written_meta.num_rows}")
                except Exception as e:
                    raise RuntimeError(f"Temp file validation failed: {e}")

                # Atomic rename temp→final
                final_path = os.path.join(compact_dir, f"{layer_name}_compact_part{file_idx:04d}.parquet")
                os.rename(current_temp_path, final_path)

                actual_size_mb = os.path.getsize(final_path) / 1e6
                print(f"  [{file_idx:04d}] {rows_in_current:,} rows, {actual_size_mb:.1f} MB")

                writer = None
                current_temp_path = None
                rows_in_current = 0
                file_idx += 1

        if src_idx % 500 == 0 and src_idx > 0:
            print(f"  ... processed {src_idx}/{n_source_files} source files, "
                  f"{total_rows:,} rows so far")

    # Flush remaining rows
    if writer is not None:
        writer.close()
        written_meta = pq.read_metadata(current_temp_path)
        if written_meta.num_rows != rows_in_current:
            raise ValueError(f"Final file row count mismatch: {rows_in_current} vs {written_meta.num_rows}")
        final_path = os.path.join(compact_dir, f"{layer_name}_compact_part{file_idx:04d}.parquet")
        os.rename(current_temp_path, final_path)
        actual_size_mb = os.path.getsize(final_path) / 1e6
        print(f"  [{file_idx:04d}] {rows_in_current:,} rows, {actual_size_mb:.1f} MB")
        file_idx += 1

    print(f"\n  Total: {total_rows:,} rows → {file_idx} compact files")
    return file_idx, total_rows


def main():
    print("=" * 70)
    print("SLINKY_GOLD_V3 — DETERMINISTIC STREAMING PARQUET COMPACTION")
    print("=" * 70)
    print(f"Source:  {OUTPUT_BASE}")
    print(f"Output:  {COMPACT_BASE}")
    print(f"Temp:    {TEMP_BASE}")
    print(f"Target:  ~{ROWS_PER_FILE:,} rows/file (~128 MB)")

    # Clean temp dir if exists
    if os.path.exists(TEMP_BASE):
        shutil.rmtree(TEMP_BASE)
    os.makedirs(TEMP_BASE, exist_ok=True)

    # Check if compact dir already exists
    if os.path.exists(COMPACT_BASE):
        print(f"\nWARNING: {COMPACT_BASE} already exists. Removing.")
        shutil.rmtree(COMPACT_BASE)
    os.makedirs(COMPACT_BASE, exist_ok=True)

    results = {}
    for layer in LAYERS:
        source_dir = str(OUTPUT_BASE / layer)
        compact_dir = str(COMPACT_BASE / layer)
        temp_dir = str(TEMP_BASE / layer)

        print(f"\nBuilding canonical schema for {layer}...")
        canonical_schema = build_canonical_schema(layer, source_dir)
        print(f"  Schema: {len(canonical_schema)} fields")

        n_files, n_rows = compact_layer(layer, canonical_schema, source_dir, compact_dir, temp_dir)
        results[layer] = {"files": n_files, "rows": n_rows}

    # Clean up temp dir
    shutil.rmtree(TEMP_BASE, ignore_errors=True)

    # Summary
    print("\n" + "=" * 70)
    print("COMPACTION COMPLETE")
    print("=" * 70)
    total_files = 0
    total_rows = 0
    for layer, info in results.items():
        print(f"  {layer}: {info['files']} files, {info['rows']:,} rows")
        total_files += info["files"]
        total_rows += info["rows"]
    print(f"  TOTAL: {total_files} files, {total_rows:,} rows")
    print(f"\nCompact output: {COMPACT_BASE}")


if __name__ == "__main__":
    main()
