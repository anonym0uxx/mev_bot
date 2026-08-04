# Amendment A-14 Proposal — §41 Extension (PROMOTION GATE AND CONSTRUCTION PARITY AS KEY-CUSTODY PRECONDITIONS)

**Proposed:** 2026-08-02  
**Status:** PROPOSED ONLY — not applied. Requires operator approval per §68/criterion 111.  
**Author:** Hermes conductor (proposing authority only)

## The Defect This Amendment Addresses

§41 currently bounds key custody by mechanism (DPAPI, CNG, non-exportable keys, signing-service isolation) and by policy (permitted programs, size caps, destination rules, wallet floor). It does not explicitly require that **the promotion gate has refused or been evaluated** before the signing boundary is unlocked, nor that **the instruction builder's account layout has been verified against real mainnet transactions** before any key is loaded for live use.

The gap: a conductor could follow §41 literally — isolate the key, enforce the signing policy, bound the wallet — while arming live capital on a paper session that produced zero closed positions, or while loading a builder whose account list is one account short of what the chain requires. Both defects are already preventable by existing modules (`ex_promotion_gate`, criterion 77's Construction Validation Gate), but §41 does not name them as key-custody preconditions. A future conductor that lacks the current session's context could skip them.

## What This Amendment Adds (exhaustive)

**(1) Promotion gate evaluation is a precondition for key loading.** Before the signing boundary is unlocked for live trading, the conductor MUST produce a `PromotionReport` from the most recent paper session and record its verdict. If the verdict is `Refuse`, the signing boundary stays closed. This is not a new gate — `ex_promotion_gate::evaluate()` already exists and is a pure function of `PaperEvidence`. This amendment makes its verdict a **key-custody precondition**, not merely a recommendation.

**Exception:** A `Refuse` verdict from a connectivity-validation session (zero closed positions, zero entries attempted) does not block key loading for **infrastructure testing** (connection validation, blockhash fetching, account subscription) — only for **trade submission**. The distinction is enforced by `LiveEnvelope::closed()`, which the promotion gate returns on refusal.

**(2) On-chain layout parity is a precondition for live builder use.** Before any instruction builder is used with live keys, its account-list output MUST be verified against real mainnet transactions of the same instruction type. The verification method: query Helius RPC for recent successful transactions, decode the instruction's account list positionally, and diff against the builder's output. A builder whose account count or writable/signer flags differ from chain reality is quarantined under the existing builder-quarantine mechanism (§36/criterion 78) and may not be used with live keys until corrected.

This is not a new requirement — criterion 77 already requires "fixture parity + live-state simulation + micro-verification." This amendment specifies that **fixture parity includes on-chain account-layout parity** against real transactions, not just against an IDL or a synthetic fixture.

**(3) Both preconditions are recorded, not merely asserted.** The promotion report and the on-chain layout verification artifact are written to `docs/` and referenced in the session's evidence store. A conductor that claims "promotion gate passed" or "builder matches chain" without producing the artifact has not satisfied the precondition.

## What This Amendment Does NOT Change

- **Key custody mechanism:** DPAPI/CNG/non-exportable key requirements (criterion 52) are unchanged.
- **Signing policy:** Permitted programs, size caps, destination rules, wallet floor — all unchanged.
- **Amendment A-12 scope:** The operator's key-custody election (hardcoded keys in a private repo) is untouched. This amendment adds preconditions, not permissions.
- **§64 wallet funding/defunding:** Remains operator-only. This amendment does not authorize the conductor to fund or sweep.
- **Gate autonomy:** The promotion gate and construction parity check are not new human-approval layers. They are deterministic evaluations that the conductor runs autonomously; their verdicts bind the conductor, not the operator.

## Evidence Reference

- **Promotion gate refusal:** `docs/PROMOTION_REPORT_20260802.md` — `PromotionVerdict::Refuse(SampleTooSmall { closed: 0, required: 100 })` from the 900s paper session.
- **On-chain layout mismatch:** `docs/TASK_ONCHAIN_LAYOUT_VERIFICATION_20260802.md` — pump.fun buy builder produces 17 accounts, real mainnet transactions have 18 (extra writable fee-program PDA at tail). PumpSwap buy builder produces 23, real transactions have 25-27.

## Proposed Text (to be inserted after §41/criterion 52's existing text, before the A-12 amendment)

> **§41.1 — Promotion gate and construction parity as key-custody preconditions.**
>
> (a) Before the signing boundary is unlocked for live trade submission, the conductor MUST evaluate `ex_promotion_gate::evaluate()` against the most recent paper session's `PaperEvidence` and record the resulting `PromotionReport` as an artifact in `docs/`. A `Refuse` verdict keeps the signing boundary closed for trade submission. A connectivity-validation session that produced zero closed positions may still load keys for infrastructure testing (connection validation, blockhash fetching, account subscription) but not for trade submission.
>
> (b) Before any instruction builder is used with live keys, its account-list output MUST be verified against real mainnet transactions of the same instruction type. The verification: query a Solana RPC for recent successful transactions targeting the program, decode the instruction's account list positionally (address, writable flag, signer flag), and diff against the builder's output. A builder whose account count, writable flags, or signer flags differ from chain reality is quarantined and may not be used with live keys until corrected. The verification artifact is recorded in `docs/`.
>
> (c) Both preconditions are recorded artifacts, not assertions. A conductor that claims satisfaction without producing the artifact has not satisfied the precondition.

## Scope Freeze

This amendment is frozen at this text per §68/criterion 111. The conductor may not widen it, may not cite it to justify skipping the promotion gate or construction parity check, and may not use it to authorize any egress, transfer, or key access beyond what §41 and A-12 already permit. Widening this amendment requires the operator.
