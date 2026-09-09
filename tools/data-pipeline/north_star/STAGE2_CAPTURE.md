# North Star — Stage 2: Market & Narrative Capture

> **Historical record, not current completion evidence.** Stage numbering and several
> status/semantic claims below were superseded by the master-DAG audit. Consult
> `00_HOLISTIC_CONTEXT.md`, `BUILD_PLAN.md`, and task receipts before implementation.
> In particular, pseudo-tests, heuristic GOLD/EX_ANTE labels, and old PASS reports
> do not certify North Star admission. Original content is retained for traceability.

Status: **plan set; capture initiated.** Date: 2026-09-07.

Goal: acquire the raw, provenance-stamped data to close the coverage gaps (D11
decisions, D04 capacity, D06 holders, D08/D09 propagation/meta) so Stage 3–4 can build
the balanced BUY/WATCH/SKIP/SELL episode set. Every capture records `first_seen_ms`
(our observation) vs `publish_time_ms` (original) to preserve the ex-ante boundary.

---

## Gap → capture → source map

| Gap | What we need | Capture source | Current state |
|---|---|---|---|
| **D11 decisions** | ex-ante decision-shaped signals (entry/exit intent) | on-chain L3 counterfactual economics + qualified/graduation-candidate events | ● partial (L3 exists); ○ human decisions absent |
| **D04 capacity** | size-dependent quotes, reserve depth, exit routes, congestion | L1 curve/pool reserves + L3 `capacity_constrained` + fresh LaserStream | ● partial |
| **D06 holders** | qualified holder data, creator/connected-wallet activity, prior skill | Helius RPC holder queries (Token-2022 `getProgramAccounts` memcmp on mint) | ○ gap |
| **D08 propagation** | independent attention, repost chains, source diversity | Firecrawl narrative (Telegram web, DexScreener, pump.fun) | ◐ thin |
| **D09 meta/rotations** | emerging/weakening themes, capital rotation | narrative_state_v1 + recurring crawl | ◐ thin (1183 states, 97 cards) |

## Concrete actions

1. **Narrative capture (D08/D09/D11 context).**
   - Start Firecrawl Docker (currently down: Docker Desktop daemon offline).
   - Run `acquire_narrative_web_v1.py --sources all`, then rebuild the 5 gold layers
     (content → claim → state → validation → strategy_card) fail-closed.
   - Target: raise `strategy_card_v1` well above 97 and re-resolve `temporal_class` on
     the 490 ambiguous trajectories.

2. **Holder enrichment (D06).**
   - Token-2022 holder snapshots via Helius RPC for each qualified candidate mint
     (`getProgramAccounts` + memcmp offset 0 = mint; NO dataSize filter).
   - Creator/connected-wallet activity + open-loser history per mint.

3. **Capacity confirmation (D04).**
   - Cross-check L1 reserve fields against L3 `capacity_constrained` to produce
     size-dependent executable-quote truth.

4. **BUY-label source expansion (D11).**
   - Broaden L3 counterfactual coverage toward qualified/graduation candidates where
     genuine profitable entries occur (the V6 lesson: buys are rare in raw data — do
     NOT pad; upweight genuine entries and expand collection toward them).

## Provenance & leakage invariants (unchanged)

- Ex-ante only; no post-`t` fields as inputs.
- `first_seen_ms` is observation metadata, never a causal feature.
- Public source ≠ licensed; resolve rights before admission.
- No paid/external social APIs; unavailable = coverage gap, not zero signal.

## Next (Stage 3)

Canonicalization/accounting: exact lamport-rounded fills, fees/tips/slippage,
real-fill vs modeled-exit separation — the numeric truth layer the episode builder and
Astra review sit on.
