# Engineering acceptance: `task/integ-combined`

Scope: engineering behaviour on fixtures. It is **not** model acceptance, paper-performance evidence, or
authorization to trade.

## Accepted source and identities
- Accepted engineering head: `905acd2d9911a0106a0f449c8e711d58458a41c9` (the last code commit; later commits on the
  branch change documentation only, see "Documentation-only commits").
- Hosted CI: `rust-ci` run 38108376458 on `905acd2d`: success (portable gate, fmt, clippy `-D warnings`, build, test).
- Local workspace suite at `905acd2d`: 4,069 passed / 0 failed / 3 ignored (rustc 1.99.0).
- Release package (built from `905acd2d`, `--release --offline --locked`):
  - `pq-daemon` sha256 `f7704f937b2a483b62ce0c3918970b4d60ad6a5484ead434b0a4c13d79cf508b`
  - `pq-sell` sha256 `feb399a82dbb15be1c6e44645aba5c51f9958130a57fd782cc3894c054fd4661`
  - `pq-watchdog` sha256 `55c889e29bb005b5127282520eabfc011046fcff04250a286ccce82443fbde3a`
  - packaged `CHAMPION_CONFIG.txt` sha256 `d9c8836a9e1186ea111e08a044c808184fe947c5b81a4b981274e6f0624cf5b7`
    (`paper_rent_basis = rent_v1_s455400973`, `txcost_version = 1`)
- Rollback: `main` before alignment = `9ba157bf2d063e0062507f974602976f9a783b99`. Existing tags are kept:
  `rollback/pre-consolidation-959cee8c`, `tested/resource-budget-5ef32954`.

## Packaged handoff → final report → restore drill (paper, stub inputs)
Fixture: a recorded wire capture (159,532 lines), barrier hold at 117, `paper_fill_v2_shadow`, stub model/ws/stream.
Fresh run directories; the package is SHA256SUMS-verified before every run.
- Handoff: 6 positions held, valid protective-handoff acknowledgement accepted, final report printed, process exited.
  The sealed capture matches the post-report ledger (generation 204, same decision clock).
- Restore (wire continuing): **all 11 checks pass.** Books preserved, restored pending orders equal to the
  post-report set, realized cash unchanged, no sells booked, nothing force-closed, curve bindings restored,
  config loaded (not compiled defaults), restored ATA deposits equal on the pinned basis, launch recovery applied
  with no lost coverage, and no named restore refusal.
- One post-restart protective order (HardStop, id 9) is proven new engine activity, not reconstruction. It was
  created 249 ms after the restart instant, from a post-restart curve observation at price 48,194 ≤ the hard-stop
  level 48,383 (entry 53,759, 1,000 bp). Its quantity is the full inventory, with no reservation and no booking.
- Restore-only run (no post-restart input): the restored ledger equals the post-report ledger (inventory 6/6,
  pending empty, sequence unchanged, deposits unchanged). The harness checkpoint is not published without input, so
  its checkpoint-based checks are recorded as not evaluable, not as passes.

## Defects fixed during acceptance (both predate the fee changes)
1. **Stop-path capture seal (production durability).** The controlled shutdown persisted the held ledger at a new
   generation without sealing the capture, so a restart from a handed-off stop refused launch recovery
   (`capture_seal_behind_restart`). Fixed in `905acd2d`: the capture is sealed after the shutdown write, before the
   stop gate can exit. Source pin test: `stop_seal_pin`. The refusal logic is unchanged.
2. **Scheduler-dependent test.** `model_path_e2e::a_slow_model_never_blocks_the_engine_and_a_late_answer_creates_no_order`
   depended on sleeps (it failed 10/10 when pinned to one CPU). Fixed in `2ff6fbcd` with explicit synchronisation:
   the model source blocks inside inference until released, and both waits are bounded. Now 20/20 pass on 1, 2 and
   all CPUs. A mutant (accept abandoned verdicts) is caught.

## Remaining limitations (not closed by this acceptance)
- Model decision quality: no candidate has passed the quality gates; none promoted.
- Engine-side timely usability ≥ 95 %: not demonstrated.
- Live BUY: disabled; `deposit_unknown` until a coherent on-chain state fetcher exists; deployed program versions
  unverified.
- Token-program coverage: on recorded captures, a token program was proven for 188/194, 134/139 and 198/206 traded
  mints. The rest refuse as `deposit_unknown` (counted per mint, not per entry request).
- Windows serving parity/latency, AMM sell economics, and the forward paper run: open.

## Documentation-only commits after `905acd2d`
README rewrite, this record and the branch cleanup record. No code, config or test changes, so the engineering
evidence above applies unchanged.
