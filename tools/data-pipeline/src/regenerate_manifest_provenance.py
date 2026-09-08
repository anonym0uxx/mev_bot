#!/usr/bin/env python3
"""
regenerate_manifest_provenance.py — Regenerate manifest provenance fields
without altering any parquet data.

Records:
  - git_sha_at_build: de0cba1 (the tree state when the build RAN — code was untracked/dirty)
  - git_sha_post_commit: 4c0b292 (the commit that captured the exact producing code)
  - source_code_sha256: hash of rebuild_v3_corrected.py at build time
  - audit_code_sha256: hashes of all corrected audit scripts
  - build_tree_state: "dirty (untracked files)"
  - parquet file hashes (recomputed from disk)
"""
import json, os, hashlib, datetime, subprocess

OUTPUT_DIR = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                          'output', 'laserstream_gold_v3')
MANIFEST_PATH = os.path.join(OUTPUT_DIR, 'manifest_laserstream_gold_v3.json')

def file_hash(path):
    h = hashlib.sha256()
    with open(path, 'rb') as f:
        for chunk in iter(lambda: f.read(65536), b''):
            h.update(chunk)
    return h.hexdigest()

def main():
    with open(MANIFEST_PATH, 'r') as f:
        manifest = json.load(f)

    # ─── Git provenance ──────────────────────────────────────────────
    # The build ran with code that was untracked in git at commit de0cba1.
    # The producing code was committed post-hoc as 4c0b292.
    manifest['git_sha_at_build'] = 'de0cba1a98ba374b10f94d96777b15e9fea15947'
    manifest['git_sha_at_build_note'] = 'Build ran with untracked/dirty source files at this commit'
    manifest['git_sha_post_commit'] = '4c0b292f9d1dbc723fff03582aa11546e9267fb5'
    manifest['git_sha_post_commit_note'] = 'Exact producing code committed post-hoc; source matches build-time content'
    manifest['build_tree_state'] = 'dirty (untracked: rebuild_v3_corrected.py, build_laserstream_gold_v3.py, audit/*.py)'

    # Replace the old false git_sha with the truth
    manifest['git_sha'] = manifest['git_sha_post_commit']  # The committed code

    # ─── Source code hashes ─────────────────────────────────────────
    src_dir = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), 'src')
    audit_dir = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), 'audit')

    manifest['source_code_hashes'] = {
        'rebuild_v3_corrected.py': file_hash(os.path.join(src_dir, 'rebuild_v3_corrected.py')),
        'build_laserstream_gold_v3.py': file_hash(os.path.join(src_dir, 'build_laserstream_gold_v3.py')),
    }
    manifest['audit_code_hashes'] = {}
    for fn in sorted(os.listdir(audit_dir)):
        if fn.endswith('.py'):
            manifest['audit_code_hashes'][fn] = file_hash(os.path.join(audit_dir, fn))

    # ─── Parquet file hashes (recomputed from disk) ─────────────────
    manifest['parquet_hashes'] = {}
    for fn in ['l1_pump_state_v3.parquet', 'l2_pump_outcome_v3.parquet',
               'l3_counterfactual_v3.parquet', 'l4_policy_eval_v3.parquet']:
        path = os.path.join(OUTPUT_DIR, fn)
        if os.path.exists(path):
            h = file_hash(path)
            manifest['parquet_hashes'][fn] = h
            # Also set the top-level keys for backwards compat
            layer = fn.split('_')[0]  # l1, l2, l3, l4
            manifest[f'{layer}_sha256'] = h[:16] + '...'

    # ─── Provenance timestamp ───────────────────────────────────────
    manifest['provenance_regenerated_at'] = datetime.datetime.now(datetime.timezone.utc).isoformat()

    # Write back
    def json_default(o):
        if isinstance(o, (int,)): return int(o)
        if isinstance(o, (float,)): return float(o)
        return str(o)

    with open(MANIFEST_PATH, 'w') as f:
        json.dump(manifest, f, indent=2, default=json_default)

    print("Manifest provenance regenerated:")
    print(f"  git_sha_at_build: {manifest['git_sha_at_build'][:12]} (dirty tree)")
    print(f"  git_sha_post_commit: {manifest['git_sha_post_commit'][:12]} (exact producing code)")
    print(f"  source_code_hashes: {len(manifest['source_code_hashes'])} files")
    print(f"  audit_code_hashes: {len(manifest['audit_code_hashes'])} files")
    print(f"  parquet_hashes: {len(manifest['parquet_hashes'])} files")
    print(f"  Provenance NOT attributed to older commit. Data unchanged.")

if __name__ == '__main__':
    main()
