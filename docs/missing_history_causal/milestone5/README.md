# Milestone 5: first rendered, independently compared SOL prompt (session 1, mint 51nH...pump)

Path: raw wire -> production parser -> event decoder + corpus resolver (match by mint+side) -> production DecisionCache
-> rendered `snapshot().user_prompt` at the corpus's own decision clocks (14 clocks, 30 s cadence). Harness:
`rust/crates/pump-quant-junction/examples/slice_prompt.rs`. Reference: the frozen examination prompts in
`/training/v2/candidate_sft_c12_entry/` (same mint, same t_dec_ms) + the tape rows.
Prompt pair: `prompt_rust_1788965530942.txt` / `prompt_corpus_1788965530942.txt`. Field diff (machine-readable, all 14 clocks):
`s1_51nH.field_diff_causal.json` (production config) and `..._allbanded_MEASUREMENT.json` (measurement only).

Result, causal production config: see report; remaining differences are listed per field in the JSON. No claim of parity.
