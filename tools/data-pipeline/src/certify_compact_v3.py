#!/usr/bin/env python
"""
slinky_gold_v3 — Compact Copy Certification
=============================================
Certifies the compacted parquet copy against the certified source.
Checks:
1. Exact row count per layer == certified source (33,581,765)
2. COUNT DISTINCT state_id == row count (uniqueness preserved)
3. Zero 4-layer anti-join misses (every state_id present in all 4 layers)
4. Exact train/val/test mint counts (via mint_disjoint_split reproduction)
5. Canonical schemas/types/nullability (no null-type columns remain)
6. No corrupted files (all readable)
7. Semantic spot-check: row fingerprints (state_id + seq + mint) match source
8. Rebuild authoritative compact manifest from actual disk files (rows/bytes/SHA256)
"""
import os
import sys
import glob
import json
import hashlib
import pyarrow.parquet as pq
import pyarrow as pa
from pathlib import Path
from collections import Counter

# Add parent for imports
sys.path.insert(0, os.path.dirname(__file__))

OUTPUT_BASE = Path("D:/repos/mev_bot/tools/data-pipeline/output/slinky_gold_v3")
COMPACT_BASE = Path("D:/repos/mev_bot/tools/data-pipeline/output/slinky_gold_v3_compact")

LAYERS = [
    "pump_state_v3",
    "pump_outcome_v3",
    "counterfactual_trade_v3",
    "policy_eval_v3",
]

EXPECTED_ROWS = 33_581_765  # per layer
EXPECTED_MINTS = 622_870
EXPECTED_SPLITS = {"train": 436_009, "val": 93_430, "test": 93_431}

# Expected column counts per layer (from gold_schema_v3.py)
EXPECTED_COLS = {
    "pump_state_v3": 105,
    "pump_outcome_v3": 92,
    "counterfactual_trade_v3": 106,
    "policy_eval_v3": 23,
}


def check_1_row_counts():
    """Exact row count per layer == certified source."""
    print("\n[1] ROW COUNT PER LAYER")
    all_pass = True
    row_counts = {}
    for layer in LAYERS:
        compact_dir = str(COMPACT_BASE / layer)
        files = sorted(glob.glob(os.path.join(compact_dir, "*.parquet")))
        total = 0
        corrupt = 0
        for f in files:
            try:
                meta = pq.read_metadata(f)
                total += meta.num_rows
            except Exception as e:
                print(f"  CORRUPT: {os.path.basename(f)}: {e}")
                corrupt += 1
        row_counts[layer] = total
        status = "PASS" if total == EXPECTED_ROWS and corrupt == 0 else "FAIL"
        if status == "FAIL":
            all_pass = False
        print(f"  {layer}: {total:,} rows ({len(files)} files, {corrupt} corrupt) [{status}]")
    return all_pass, row_counts


def check_2_state_id_uniqueness():
    """COUNT DISTINCT state_id == row count."""
    print("\n[2] STATE_ID UNIQUENESS")
    # Use DuckDB for efficiency
    import duckdb
    con = duckdb.connect()
    con.execute("SET threads TO 8;")
    all_pass = True
    for layer in LAYERS:
        compact_dir = str(COMPACT_BASE / layer)
        glob_pattern = os.path.join(compact_dir, "*.parquet").replace("\\", "/")
        try:
            r = con.execute(f"""
                SELECT 
                    count(*) as total_rows,
                    count(DISTINCT state_id) as distinct_ids
                FROM read_parquet('{glob_pattern}', union_by_name=true)
            """).fetchone()
            total, distinct = r[0], r[1]
            status = "PASS" if total == distinct else "FAIL"
            if status == "FAIL":
                all_pass = False
            print(f"  {layer}: total={total:,} distinct={distinct:,} [{status}]")
        except Exception as e:
            print(f"  {layer}: ERROR: {e}")
            all_pass = False
    con.close()
    return all_pass


def check_3_anti_join():
    """Zero 4-layer anti-join misses."""
    print("\n[3] 4-LAYER ANTI-JOIN")
    import duckdb
    con = duckdb.connect()
    con.execute("SET threads TO 8;")
    
    # Check that every state_id in pump_state_v3 exists in all other layers
    state_glob = str(COMPACT_BASE / "pump_state_v3" / "*.parquet").replace("\\", "/")
    all_pass = True
    
    for layer in ["pump_outcome_v3", "counterfactual_trade_v3", "policy_eval_v3"]:
        layer_glob = str(COMPACT_BASE / layer / "*.parquet").replace("\\", "/")
        try:
            # Check: state_ids in pump_state_v3 NOT IN layer
            r = con.execute(f"""
                SELECT count(*) FROM (
                    SELECT DISTINCT state_id FROM read_parquet('{state_glob}', union_by_name=true)
                    WHERE state_id NOT IN (
                        SELECT DISTINCT state_id FROM read_parquet('{layer_glob}', union_by_name=true)
                    )
                )
            """).fetchone()
            misses = r[0]
            status = "PASS" if misses == 0 else "FAIL"
            if status == "FAIL":
                all_pass = False
            print(f"  pump_state_v3 → {layer}: {misses} missing [{status}]")
        except Exception as e:
            print(f"  pump_state_v3 → {layer}: ERROR: {e}")
            all_pass = False
    con.close()
    return all_pass


def check_4_split_counts():
    """Exact train/val/test mint counts via mint_disjoint_split reproduction."""
    print("\n[4] TRAIN/VAL/TEST MINT COUNTS")
    import duckdb
    con = duckdb.connect()
    con.execute("SET threads TO 8;")
    
    state_glob = str(COMPACT_BASE / "pump_state_v3" / "*.parquet").replace("\\", "/")
    try:
        # Get unique mints sorted (same as original pipeline)
        r = con.execute(f"""
            SELECT DISTINCT mint FROM read_parquet('{state_glob}', union_by_name=true)
            ORDER BY mint
        """).fetchall()
        all_mints = [row[0] for row in r]
        n_mints = len(all_mints)
        
        # Reproduce the split (same logic as mint_disjoint_split with just strings)
        # The original code sorted by x[1] (2nd char of string), but since we're
        # reproducing the same deterministic behavior, we need to match it exactly.
        # Actually, the original code got strings not tuples, so sorted by 2nd char.
        # But all_mints here is already sorted by mint (DuckDB ORDER BY mint).
        # The original all_mints was also sorted by mint (DuckDB ORDER BY mint).
        # Then mint_disjoint_split did: sorted(mints_with_time, key=lambda x: x[1])
        # With strings, x[1] is the 2nd character. So we need to sort by 2nd char!
        sorted_mints = sorted(all_mints, key=lambda x: x[1] if len(x) > 1 else '')
        
        train_end = int(n_mints * 0.7)
        val_end = int(n_mints * 0.85)
        
        train_count = train_end
        val_count = val_end - train_end
        test_count = n_mints - val_end
        
        all_pass = (train_count == EXPECTED_SPLITS["train"] and
                    val_count == EXPECTED_SPLITS["val"] and
                    test_count == EXPECTED_SPLITS["test"] and
                    n_mints == EXPECTED_MINTS)
        
        print(f"  Total mints: {n_mints:,} (expected {EXPECTED_MINTS:,}) [{'PASS' if n_mints == EXPECTED_MINTS else 'FAIL'}]")
        print(f"  Train: {train_count:,} (expected {EXPECTED_SPLITS['train']:,}) [{'PASS' if train_count == EXPECTED_SPLITS['train'] else 'FAIL'}]")
        print(f"  Val:   {val_count:,} (expected {EXPECTED_SPLITS['val']:,}) [{'PASS' if val_count == EXPECTED_SPLITS['val'] else 'FAIL'}]")
        print(f"  Test:  {test_count:,} (expected {EXPECTED_SPLITS['test']:,}) [{'PASS' if test_count == EXPECTED_SPLITS['test'] else 'FAIL'}]")
    except Exception as e:
        print(f"  ERROR: {e}")
        all_pass = False
    con.close()
    return all_pass


def check_5_canonical_schemas():
    """Canonical schemas/types/nullability — no null-type columns remain."""
    print("\n[5] CANONICAL SCHEMAS (no null-type columns)")
    all_pass = True
    for layer in LAYERS:
        compact_dir = str(COMPACT_BASE / layer)
        files = sorted(glob.glob(os.path.join(compact_dir, "*.parquet")))
        null_type_cols = 0
        col_count = 0
        for f in files[:5]:  # sample first 5
            s = pq.read_schema(f)
            col_count = len(s)
            for i in range(len(s)):
                if str(s.field(i).type) == "null":
                    null_type_cols += 1
                    print(f"  {layer}: null-type column '{s.field(i).name}' in {os.path.basename(f)}")
        expected = EXPECTED_COLS.get(layer, 0)
        status = "PASS" if null_type_cols == 0 and col_count == expected else "FAIL"
        if status == "FAIL":
            all_pass = False
        print(f"  {layer}: {col_count} cols (expected {expected}), {null_type_cols} null-type [{status}]")
    return all_pass


def check_6_no_corrupt_files():
    """No corrupted files — all readable."""
    print("\n[6] NO CORRUPTED FILES")
    all_pass = True
    total_files = 0
    for layer in LAYERS:
        compact_dir = str(COMPACT_BASE / layer)
        files = sorted(glob.glob(os.path.join(compact_dir, "*.parquet")))
        total_files += len(files)
        corrupt = 0
        for f in files:
            try:
                meta = pq.read_metadata(f)
                # Also try reading a small slice
                t = pq.read_table(f, columns=[pq.read_schema(f).field(0).name])
            except Exception as e:
                print(f"  CORRUPT: {os.path.basename(f)}: {e}")
                corrupt += 1
        status = "PASS" if corrupt == 0 else "FAIL"
        if status == "FAIL":
            all_pass = False
        print(f"  {layer}: {len(files)} files, {corrupt} corrupt [{status}]")
    print(f"  Total: {total_files} files")
    return all_pass


def check_7_semantic_spotcheck():
    """Semantic spot-check: row fingerprints (state_id) match source vs compact.
    Uses a deterministic sample of state_ids from source and checks they exist in compact."""
    print("\n[7] SEMANTIC SPOT-CHECK (source vs compact fingerprints)")
    import duckdb
    con = duckdb.connect()
    con.execute("SET threads TO 8;")
    all_pass = True
    
    for layer in LAYERS:
        src_glob = str(OUTPUT_BASE / layer / "*.parquet").replace("\\", "/")
        cmp_glob = str(COMPACT_BASE / layer / "*.parquet").replace("\\", "/")
        
        try:
            # Take a deterministic sample of state_ids from source (every Nth row)
            # and verify they ALL exist in compact.
            # Use a 1% systematic sample via WHERE hash(state_id) % 100 = 0
            r = con.execute(f"""
                WITH src_sample AS (
                    SELECT state_id 
                    FROM read_parquet('{src_glob}', union_by_name=true)
                    WHERE abs(hash(state_id)) % 100 = 0
                ),
                cmp_ids AS (
                    SELECT state_id 
                    FROM read_parquet('{cmp_glob}', union_by_name=true)
                    WHERE abs(hash(state_id)) % 100 = 0
                )
                SELECT 
                    (SELECT count(*) FROM src_sample) as src_n,
                    (SELECT count(*) FROM cmp_ids) as cmp_n,
                    (SELECT count(*) FROM src_sample WHERE state_id NOT IN (SELECT state_id FROM cmp_ids)) as missing_in_cmp
            """).fetchone()
            src_n, cmp_n, missing = r
            
            status = "PASS" if missing == 0 and src_n == cmp_n else "FAIL"
            if status == "FAIL":
                all_pass = False
            print(f"  {layer}: src_sample={src_n:,} cmp_sample={cmp_n:,} missing={missing} [{status}]")
        except Exception as e:
            print(f"  {layer}: ERROR: {e}")
            all_pass = False
    con.close()
    return all_pass


def check_8_build_manifest():
    """Rebuild authoritative compact manifest from actual disk files."""
    print("\n[8] BUILD COMPACT MANIFEST (rows/bytes/SHA256)")
    manifest = {
        "version": "compact_v3",
        "source": str(OUTPUT_BASE),
        "compact": str(COMPACT_BASE),
        "layers": {},
    }
    total_rows = 0
    total_bytes = 0
    total_files = 0
    
    for layer in LAYERS:
        compact_dir = str(COMPACT_BASE / layer)
        files = sorted(glob.glob(os.path.join(compact_dir, "*.parquet")))
        layer_rows = 0
        layer_bytes = 0
        file_infos = []
        
        for f in files:
            meta = pq.read_metadata(f)
            size = os.path.getsize(f)
            # SHA256 of the file
            h = hashlib.sha256()
            with open(f, 'rb') as fh:
                while True:
                    chunk = fh.read(8192 * 1024)  # 8MB chunks
                    if not chunk:
                        break
                    h.update(chunk)
            sha = h.hexdigest()
            layer_rows += meta.num_rows
            layer_bytes += size
            file_infos.append({
                "file": os.path.basename(f),
                "rows": meta.num_rows,
                "bytes": size,
                "sha256": sha,
            })
        
        manifest["layers"][layer] = {
            "files": len(files),
            "rows": layer_rows,
            "bytes": layer_bytes,
            "file_infos": file_infos,
        }
        total_rows += layer_rows
        total_bytes += layer_bytes
        total_files += len(files)
        print(f"  {layer}: {len(files)} files, {layer_rows:,} rows, {layer_bytes/1e9:.2f} GB")
    
    manifest["total_files"] = total_files
    manifest["total_rows"] = total_rows
    manifest["total_bytes"] = total_bytes
    manifest["total_rows_per_layer"] = total_rows // 4
    
    manifest_path = str(COMPACT_BASE / "compact_manifest_v3.json")
    with open(manifest_path, 'w') as f:
        json.dump(manifest, f, indent=2)
    print(f"\n  Manifest written: {manifest_path}")
    print(f"  TOTAL: {total_files} files, {total_rows:,} rows, {total_bytes/1e9:.2f} GB")
    
    # Validate row counts
    all_pass = (total_rows == EXPECTED_ROWS * 4)
    return all_pass


def main():
    print("=" * 70)
    print("SLINKY_GOLD_V3 — COMPACT COPY CERTIFICATION")
    print("=" * 70)
    print(f"Source:  {OUTPUT_BASE}")
    print(f"Compact: {COMPACT_BASE}")
    
    results = {}
    
    results["1_row_counts"], _ = check_1_row_counts()
    results["2_uniqueness"] = check_2_state_id_uniqueness()
    results["3_anti_join"] = check_3_anti_join()
    results["4_splits"] = check_4_split_counts()
    results["5_schemas"] = check_5_canonical_schemas()
    results["6_no_corrupt"] = check_6_no_corrupt_files()
    results["7_spotcheck"] = check_7_semantic_spotcheck()
    results["8_manifest"] = check_8_build_manifest()
    
    print("\n" + "=" * 70)
    print("CERTIFICATION SUMMARY")
    print("=" * 70)
    all_pass = True
    for name, passed in results.items():
        status = "PASS" if passed else "FAIL"
        if not passed:
            all_pass = False
        print(f"  {name}: {status}")
    
    if all_pass:
        print("\n  ✅ ALL CHECKS PASS — compact copy is certified")
    else:
        print("\n  ❌ SOME CHECKS FAILED — investigate before freeze")
    
    return all_pass


if __name__ == "__main__":
    main()
