#!/usr/bin/env python
"""
slinky_gold_v3 — Authoritative Disk-Based Manifest
Scans actual finalized Parquet files on disk. NOT worker counters.
Records: unique path, layer, chunk/flush identity, bytes, row count, schema columns, SHA256.
Disk row totals must equal processed totals exactly.
"""
import os, sys, json, hashlib, glob, re, time
from pathlib import Path
import duckdb

BASE = Path("D:/repos/mev_bot/tools/data-pipeline/output/slinky_gold_v3")
LAYERS = ["pump_state_v3", "pump_outcome_v3", "counterfactual_trade_v3", "policy_eval_v3"]

def compute_sha256(filepath):
    h = hashlib.sha256()
    with open(filepath, "rb") as f:
        while chunk := f.read(65536):
            h.update(chunk)
    return h.hexdigest()

def main():
    t0 = time.time()
    print("=" * 80)
    print("SLINKY_GOLD_V3 — AUTHORITATIVE DISK MANIFEST")
    print("=" * 80)
    
    con = duckdb.connect()
    con.execute("SET threads TO 8;")
    
    manifest = {
        "manifest_type": "disk_authoritative",
        "generated_at": time.strftime("%Y-%m-%dT%H:%M:%S"),
        "base_path": str(BASE),
        "layers": {},
        "total_files": 0,
        "total_bytes": 0,
        "total_rows": 0,
    }
    
    grand_total_files = 0
    grand_total_bytes = 0
    grand_total_rows = 0
    
    for layer in LAYERS:
        layer_dir = BASE / layer
        files = sorted(glob.glob(str(layer_dir / "*.parquet")))
        
        print(f"\n  Scanning {layer}: {len(files)} files...")
        layer_files = []
        layer_bytes = 0
        layer_rows = 0
        
        for i, fpath in enumerate(files):
            fname = os.path.basename(fpath)
            fsize = os.path.getsize(fpath)
            
            # Extract chunk/flush identity from filename
            m = re.search(r'part(\d+)_(\d+)', fname)
            if m:
                part_num = int(m.group(1))
                sub_num = int(m.group(2))
                # Reverse the formula: part = chunk_idx * 51 + flush_idx
                chunk_idx = part_num // 51
                flush_idx = part_num % 51
            else:
                chunk_idx = -1
                flush_idx = -1
            
            # Row count from parquet metadata (fast, no full read)
            try:
                r = con.execute(f"SELECT count(*) FROM read_parquet('{fpath}')").fetchone()
                row_count = r[0]
            except Exception as e:
                row_count = -1
                print(f"    CORRUPT: {fname}: {e}")
            
            # SHA256 (only for files < 50MB to keep runtime reasonable)
            # For large files, use file size as fingerprint
            if fsize < 50 * 1024 * 1024:
                sha = compute_sha256(fpath)
            else:
                # Use first 1MB + last 1MB + size as fingerprint
                h = hashlib.sha256()
                with open(fpath, "rb") as f:
                    h.update(f.read(1024*1024))
                    f.seek(-1024*1024, os.SEEK_END)
                    h.update(f.read(1024*1024))
                    h.update(str(fsize).encode())
                sha = h.hexdigest()
            
            layer_files.append({
                "filename": fname,
                "path": fpath,
                "bytes": fsize,
                "rows": row_count,
                "chunk_idx": chunk_idx,
                "flush_idx": flush_idx,
                "sha256": sha,
            })
            layer_bytes += fsize
            layer_rows += row_count if row_count > 0 else 0
            
            if (i + 1) % 1000 == 0:
                print(f"    {i+1}/{len(files)} files scanned...")
        
        # Schema columns
        if files:
            try:
                cols = con.execute(
                    f"SELECT column_name, column_type FROM (SELECT * FROM read_parquet('{files[0]}') LIMIT 0)"
                ).fetchall()
                schema_cols = [{"name": c[0], "type": c[1]} for c in cols]
            except:
                schema_cols = []
        else:
            schema_cols = []
        
        manifest["layers"][layer] = {
            "file_count": len(files),
            "total_bytes": layer_bytes,
            "total_rows": layer_rows,
            "schema_columns": schema_cols,
            "files": layer_files,
        }
        
        grand_total_files += len(files)
        grand_total_bytes += layer_bytes
        grand_total_rows += layer_rows
        
        print(f"  {layer}: {len(files)} files, {layer_bytes/1e9:.2f} GB, {layer_rows:,} rows")
    
    manifest["total_files"] = grand_total_files
    manifest["total_bytes"] = grand_total_bytes
    manifest["total_rows"] = grand_total_rows
    manifest["runtime_seconds"] = time.time() - t0
    
    # Cross-layer checks
    print("\n  CROSS-LAYER CHECKS:")
    layer_row_counts = {l: manifest["layers"][l]["total_rows"] for l in LAYERS}
    unique_counts = set(layer_row_counts.values())
    
    if len(unique_counts) == 1:
        count = unique_counts.pop()
        print(f"    Cross-layer 1:1:1:1: PASS (all {count:,} rows)")
        manifest["cross_layer_cardinality"] = "PASS"
    else:
        print(f"    Cross-layer 1:1:1:1: FAIL {layer_row_counts}")
        manifest["cross_layer_cardinality"] = f"FAIL: {layer_row_counts}"
    
    # Expected count
    expected = 33581765
    if grand_total_rows // 4 == expected:  # 4 layers
        print(f"    Per-layer count matches expected {expected:,}: PASS")
        manifest["expected_per_layer"] = expected
        manifest["per_layer_match"] = "PASS"
    else:
        print(f"    Per-layer count MISMATCH: expected {expected:,}, got {grand_total_rows // 4:,}")
        manifest["per_layer_match"] = "FAIL"
    
    # Write manifest
    manifest_path = BASE / "disk_manifest_v3.json"
    with open(manifest_path, "w") as f:
        json.dump(manifest, f, indent=2)
    
    print(f"\n  Total: {grand_total_files} files, {grand_total_bytes/1e9:.2f} GB, {grand_total_rows:,} rows")
    print(f"  Manifest written: {manifest_path}")
    print(f"  Runtime: {time.time() - t0:.1f}s")
    
    con.close()

if __name__ == "__main__":
    main()
