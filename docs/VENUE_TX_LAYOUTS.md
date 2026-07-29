# VENUE TRANSACTION LAYOUTS — the authoritative account-order reference

**Status:** written 2026-07-29, immediately before `legacy/` was removed from the repo.
**Scope:** pump.fun bonding curve (`6EF8rrec…`) and PumpSwap AMM (`pAMMBay6…`) `buy`/`sell`.
**Constitution:** §18.2 (fail closed), §102 (named constants with citations), criterion 77 /
criterion 113 (construction validation gate), §18.2 registry discipline ("never accept a
program or PDA because a model, website, or social post claims relevance — verify through
raw on-chain relationships").

---

## 0. Why this file exists, and the one thing to read if you read nothing else

`legacy/` was deleted from the repo on 2026-07-29. Before deletion it held the **only
account-meta layouts anywhere in the working tree**. The live workspace has the instruction
*discriminators* and the instruction *data* encoding (`pump-quant-protocol::ix`), the
*decoders* (`pump-quant-protocol::pumpswap_ix`, `::pumpswap_event`), and a construction
gate that emits a **synthetic three-account placeholder**
(`pump-quant-execution::ex_construction_gate::build_ix`). It has no real account list for
any venue. That was true before this deletion and is still true after it; deleting `legacy/`
removed a reference, not a capability.

**The single most important finding of the pre-deletion audit:** the legacy pump.fun
bonding-curve builder that looked most like a drop-in reference —
`legacy/src/mev/pump-tx-builder.ts` — carried a **fabricated `Global` address**. It used
`4wTV81ej3eDXFRv9dFGc3bJBFNHqEMWCeUhFpEsLWEMZ`. The real Global PDA, re-derived here from
first principles, is `4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf`. The two share a
9-character prefix and diverge after it, which is the signature of a hallucinated base58
string rather than a typo. Its fee recipient diverges from the canonical one in the same
way. A Phase-B engineer who copied that file — the obvious thing to do, since it is short,
self-contained, and reads as authoritative — would have had **every buy fail on-chain** with
a constraint error, and would have spent the debugging budget on the account *order* (which
is a genuine but different problem) rather than on a bad constant.

This is the A-13(5) chase-the-falsification obligation applied to a deletion: the layouts are
preserved below, but only after each constant was re-derived, and the ones that failed
derivation are recorded as failures rather than quietly corrected and forgotten.

---

## 1. What prod builds today: nothing, and it says so

| Site | What it is | What it is not |
|---|---|---|
| `pump-quant-protocol::ix::{build_buy_ix, build_sell_ix}` | The real 24-byte data blob for the bonding curve: discriminator ++ `amount` u64 LE ++ (`max_sol_cost` \| `min_sol_output`) u64 LE. **Correct.** | Not an instruction. It emits `data` only; the module doc says so explicitly. |
| `pump-quant-protocol::pumpswap_ix::decode_*` | Decoders for observing *other people's* swaps in the shred/log stream. Prefix-tolerant by design (§18.2): trailing optional args and appended accounts decode as `None`. **Correct for its purpose.** | Not a builder. Nothing here ever signs. |
| `pump-quant-execution::ex_construction_gate::build_ix` | A deterministic three-account, fixture-able instruction so criterion 113's gate can exist from authoring time. Its own doc comment calls it minimal and "NOT a signing / submission path." | Not a chain layout. Its `PUMPFUN_PROGRAM_ID = [0xF0; 32]`, `PUMPSWAP_PROGRAM_ID = [0x5A; 32]` and `PUMPSWAP_BUY_DISCRIMINATOR = [10,20,30,40,50,60,70,80]` are **deliberate placeholders**. The PumpSwap discriminator placeholder is load-bearing: the real PumpSwap `buy` discriminator is byte-identical to pump.fun's, because both are `sha256("global:buy")[..8]`, and the gate needs the two venues to be distinguishable in a fixture. **Do not "fix" it to the real value.** |

So the question "are we encoding buy and sell correctly, and will we error out on-chain?"
has a precise answer: **the data encoding is correct and there is no account encoding yet.**
There is nothing currently capable of producing a chain error because nothing currently
produces a chain transaction. Phase B is where the exposure begins, and §2–§4 below are what
it must build against.

### 1.1 The `track_volume` warning in `docs/BUILD_SPEC_LIVE_EXECUTION.md:525` is stale, and was never about prod

That spec says a missing `track_volume` byte "is likely the cause of MissingAccount errors"
and prescribes a fix in `build_swap_data()` at `pumpswap.rs ~line 390`. Three facts settle it:

1. `build_swap_data` existed **only** in `legacy/rust-legacy/pump-quant-core/src/tx/pumpswap.rs`.
   The spec is a bug report against the legacy Rust bot, not against this workspace.
2. That bug was **already fixed in legacy** before deletion. The shipped legacy
   `build_swap_data` appends `0x00` for buys, with the comment *"prevents
   AccountNotInitialized errors when `user_volume_accumulator` hasn't been created via
   `init_user_volume_accumulator` yet."* The spec's prescription and the legacy code agree.
3. `track_volume` is a **PumpSwap AMM** argument. The pump.fun **bonding-curve** `buy` has
   exactly two args (`amount`, `max_sol_cost`) and its data blob is 24 bytes. Prod's
   `IX_DATA_LEN = 8 + 8 + 8` is the bonding-curve length and is right for the bonding curve.

Prod is not affected by any of it. The spec line should be read as history.

---

## 2. Constants, re-derived from first principles

Every address below was recomputed locally by `find_program_address` — SHA-256 over
`seeds ++ bump ++ program_id ++ b"ProgramDerivedAddress"`, rejecting on-curve results — with
no network call and no reliance on any document. The derivation script is reproduced in §6 so
the next auditor can re-run it rather than trust this table.

### pump.fun bonding curve — `6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P`

| Seed | Derived address | Bump |
|---|---|---|
| `["global"]` | `4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf` | 255 |
| `["__event_authority"]` | `Ce6TQqeHC9p8KetsN6JsjHK7UTZk7nasjjnr7XxXp9F1` | 255 |
| `["global_volume_accumulator"]` | `Hq2wp8uJ9jCPsYgNHex8RtqdvMPfVGoYwjvF1ATiwn2Y` | 255 |
| `["mint-authority"]` | `TSLvdd1pWpHVjahSpsvCXUbgwsL3JAcvokwaKt1eokM` | 255 |
| `["bonding-curve", mint]` | per-mint | — |
| `["creator-vault", creator]` | per-creator | — |
| `["user_volume_accumulator", user]` | per-wallet | — |

### PumpSwap AMM — `pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA`

| Seed | Derived address | Bump | Legacy constant | Verdict |
|---|---|---|---|---|
| `["global_config"]` | `ADyA8hdefvWN2dbGGWFotbzWxrAvLW83WG6QCVXvJKqw` | 255 | `PUMPSWAP_GLOBAL_CONFIG` | **matches** |
| `["__event_authority"]` | `GS4CU59F31iL7aR2Q8zVS8DRrcRnXX1yjQ66TqNVQnaR` | 255 | `PUMPSWAP_EVENT_AUTHORITY` | **matches** |
| `["global_volume_accumulator"]` | `C2aFPdENg4A2HQsmrd5rTw5TaYBX5Ku887cWjbFKtZpw` | 255 | `PUMPSWAP_GLOBAL_VOLUME_ACCUMULATOR` | **matches** |
| `["user_volume_accumulator", user]` | per-wallet | — | derived at build time | — |
| `["pool-v2", base_mint]` | per-mint | — | derived at build time | — |

Three independent PDA matches is what earns the legacy **PumpSwap** builder its credibility;
it was maintained against a live chain. The bonding-curve TypeScript was not.

### Fixed, non-derivable addresses (cannot be proven by algebra — verify on-chain in Phase B)

| Name | Address | Provenance |
|---|---|---|
| `fee_program` (both venues) | `pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ` | legacy `PUMPSWAP_FEE_PROGRAM`, independently corroborated |
| `fee_config` (PumpSwap) | `5PHirr8joyTMp9JMm6nW7hNDVyEYdkzDqazxPD7RaTjx` | legacy `PUMPSWAP_FEE_CONFIG` — **unverified** |
| pump.fun `fee_recipient` | `CebN5WGQ4jvEPvsVU4EoHEpgzq1VV7AbCJ2AWKyicZKR` | `legacy/src/execution/solana.ts:249`. Read from the `Global` account at runtime; **never hardcode this in prod** — §18.2 requires the fee model come from decoded state. |
| PumpSwap `protocol_fee_recipient` | 8-address rotation set | legacy `PUMPSWAP_FEE_RECIPIENTS[0..8]`, index selected `idx % 8` — **unverified** |

### The two rejected constants, recorded so nobody reintroduces them

| Source | Claimed | Truth |
|---|---|---|
| `legacy/src/mev/pump-tx-builder.ts:37` | Global `4wTV81ej3eDXFRv9dFGc3bJBFNHqEMWCeUhFpEsLWEMZ` | **FABRICATED.** Not the `["global"]` PDA. |
| `legacy/src/mev/pump-tx-builder.ts:36` | fee recipient `CebN5WGQ4jvEPvsVU4EoHEpgznyQHeP5R5wMA7iiMVJP` | **REJECTED.** Diverges from `solana.ts:249` in the same prefix-preserving way. |

---

## 3. Discriminators — all eight recomputed, all shipped values correct

`sha256("global:<name>")[..8]`:

| Instruction | Bytes | Hex | Where it ships |
|---|---|---|---|
| `buy` | `[102, 6, 61, 18, 1, 218, 235, 234]` | `66063d1201daebea` | `ix::BUY_DISCRIMINATOR` ✓ |
| `sell` | `[51, 230, 133, 164, 1, 127, 131, 173]` | `33e685a4017f83ad` | `ix::SELL_DISCRIMINATOR` ✓ |
| `create_pool` | `[233, 146, 209, 142, 207, 104, 64, 188]` | `e992d18ecf6840bc` | `pumpswap_ix::CREATE_POOL_DISCRIMINATOR` ✓ |
| `deposit` | `[242, 35, 198, 137, 82, 225, 242, 182]` | `f223c68952e1f2b6` | `pumpswap_ix::DEPOSIT_DISCRIMINATOR` ✓ |
| `withdraw` | `[183, 18, 70, 156, 148, 109, 161, 34]` | `b712469c946da122` | `pumpswap_ix::WITHDRAW_DISCRIMINATOR` ✓ |
| `migrate` | `[155, 234, 231, 146, 236, 158, 162, 30]` | `9beae792ec9ea21e` | `pumpswap_ix::PUMP_MIGRATE_DISCRIMINATOR` ✓ |
| `create` | `[24, 30, 200, 40, 5, 28, 7, 119]` | `181ec828051c0777` | not shipped |
| `extend_account` | `[234, 102, 194, 203, 150, 72, 62, 229]` | `ea66c2cb96483ee5` | not shipped |

`buy` and `sell` are **byte-identical across the two programs** — Anchor namespaces the hash
by instruction name, not by program. Venue disambiguation is by `program_id`, never by
discriminator. This is why `ex_construction_gate` uses a synthetic PumpSwap discriminator and
why `pumpswap_ix` correctly imports `BUY_DISCRIMINATOR` from `ix` rather than defining a
second one.

---

## 4. Account orders

### 4.1 pump.fun bonding curve — **the legacy layout is obsolete; do not ship it**

Both legacy TypeScript builders emit the **12-account, `rent`-at-index-9** layout:

```
buy : [0] global  [1] fee_recipient(w)  [2] mint  [3] bonding_curve(w)
      [4] associated_bonding_curve(w)  [5] associated_user(w)  [6] user(s,w)
      [7] system_program  [8] token_program  [9] SysvarRent  [10] event_authority
      [11] program
sell: identical except [8] = associated_token_program, [9] = token_program, no rent sysvar
```

That is the pre-creator-fee layout. The program has since (a) replaced the rent sysvar with
`creator_vault`, (b) appended the two volume accumulators, (c) appended `fee_config` +
`fee_program`, and (d) appended a trailing `bonding_curve_v2` that must be **last**.
Published current shape:

**`buy` — 17 accounts**

| # | Account | W | Note |
|---|---|---|---|
| 0 | `global` | | `["global"]` |
| 1 | `fee_recipient` | ✓ | read from `Global`, not hardcoded |
| 2 | `mint` | | |
| 3 | `bonding_curve` | ✓ | `["bonding-curve", mint]` |
| 4 | `associated_bonding_curve` | ✓ | |
| 5 | `associated_user` | ✓ | |
| 6 | `user` | ✓ | signer |
| 7 | `system_program` | | |
| 8 | `token_program` | | spl-token **or** Token-2022 — decode, never assume |
| 9 | `creator_vault` | ✓ | `["creator-vault", creator]` — **replaced the rent sysvar** |
| 10 | `event_authority` | | `["__event_authority"]` |
| 11 | `program` | | `6EF8rrec…` |
| 12 | `global_volume_accumulator` | | `["global_volume_accumulator"]` |
| 13 | `user_volume_accumulator` | ✓ | `["user_volume_accumulator", user]` |
| 14 | `fee_config` | | same key for buy and sell |
| 15 | `fee_program` | | `pfeeUxB6…` |
| 16 | `bonding_curve_v2` | | `["bonding-curve-v2", mint]` — **must be last**; need not exist on-chain |

**`sell` — 15 accounts (non-cashback) / 16 (cashback)**

`[0..7]` as above, then `[8] creator_vault(w)`, `[9] token_program`, `[10] event_authority`,
`[11] program`, `[12] fee_config`, `[13] fee_program`, then `[14] bonding_curve_v2` — except
on a **cashback** mint, where `user_volume_accumulator(w)` is inserted at `[14]` and
`bonding_curve_v2` moves to `[15]`. Cashback status is byte offset **82** of the bonding-curve
account data; it is *not* implied by the token program.

> **STATUS: UNVERIFIED ON-CHAIN.** §18.2 forbids accepting a layout because a document
> claims it. This table is a starting hypothesis for Phase B, sourced from published
> post-cashback documentation and consistent with the creator-vault and volume-accumulator
> mechanics that the legacy PumpSwap builder independently confirms. **Before the first live
> buy, decode one real successful `buy` and one real successful `sell` off the chain and
> diff the account list against this table.** The ingestion plane already sees these
> instructions; the fixture is a slot away, and it costs nothing compared with a failed
> entry. Record the result in the §18.2 protocol registry with the verifying slot.

### 4.2 PumpSwap AMM — preserved from the verified legacy builder

Source: `legacy/rust-legacy/pump-quant-core/src/tx/pumpswap.rs:420-616`. All three of its
derivable constants matched independent derivation (§2), which is the evidence this layout
is worth carrying forward.

**Fixed positions `[0]`–`[2]` and `[9]`–`[18]`; ordering-sensitive `[3]`–`[8]`.**

| # | Account | W | S |
|---|---|---|---|
| 0 | `pool` | ✓ | |
| 1 | `user` | ✓ | ✓ |
| 2 | `global_config` | | |
| 3 | `base_mint` (**on-chain order**) | | |
| 4 | `quote_mint` (**on-chain order**) | | |
| 5 | `user_base_token_account` | ✓ | |
| 6 | `user_quote_token_account` | ✓ | |
| 7 | `pool_base_token_account` | ✓ | |
| 8 | `pool_quote_token_account` | ✓ | |
| 9 | `protocol_fee_recipient` | ✓ | |
| 10 | `protocol_fee_recipient_token_account` | ✓ | |
| 11 | `base_token_program` | | |
| 12 | `quote_token_program` | | |
| 13 | `system_program` | | |
| 14 | `associated_token_program` | | |
| 15 | `event_authority` | | |
| 16 | `pump_program` (self-CPI) | | |
| 17 | `coin_creator_vault_ata` | ✓ | |
| 18 | `coin_creator_vault_authority` | | |
| 19 | *buy only* `global_volume_accumulator` | | |
| 20 | *buy only* `user_volume_accumulator` | ✓ | |
| 19/21 | `fee_config` | | sell `[19]`, buy `[21]` |
| 20/22 | `fee_program` | | sell `[20]`, buy `[22]` |
| — | *remaining*: `cashback_ata` (cashback mints only), then `pool_v2` | ✓ / | `pool_v2` is always last |

So: **21 accounts for sell, 23 for buy**, before the remaining-accounts tail.

The load-bearing subtleties, all of which are ways to lose real SOL rather than style points:

* **Pool ordering is a decoded fact, not a convention.** `[3]`–`[8]` follow the pool's own
  base/quote assignment. Legacy's empirical note (`momentum/pool.rs:1072`) is that the traded
  token lands in `quote_mint` (account-data offset 75) in roughly **81%** of pools — so the
  "obvious" normal-pool assumption is the *minority* case. A reversed pool also flips which
  discriminator expresses the trade you want: selling the token is a PumpSwap `buy` when the
  token is the quote side. Legacy handled this at `pumpswap.rs:750-757` and `:875-882`.
* **Protocol fees are collected in the pool's quote mint**, so `[10]` is the fee recipient's
  WSOL ATA on a normal pool and their *token* ATA on a reversed one.
* **`coin_creator_vault_ata` zeroed is a build-time failure, not a default.** Passing
  `Pubkey::default()` yields the System Program address, which PumpSwap validates as a token
  account and rejects. Legacy derives the ATA from `coin_creator_vault_authority` when only
  the ATA is missing, and **returns an error when both are zeroed** — fail closed (§18.2).
  Prod must keep this shape: refuse to build, never substitute a placeholder.
* **`track_volume` on buy.** Legacy appends `0x00` (`OptionBool::None`) making buy data 25
  bytes and sell data 24. This deliberately skips volume tracking so an uninitialised
  `user_volume_accumulator` cannot produce `AccountNotInitialized` (6/2014). If Phase B ever
  wants the volume credit, the correct move is `init_user_volume_accumulator` **first**, not
  flipping the byte.
* **OPEN QUESTION for Phase B.** At least one published IDL summary lists `track_volume:
  OptionBool` on PumpSwap **`sell`** as well; legacy appends it only on buy. Either legacy's
  sell was tolerated because the arg is trailing-optional, or the summary is wrong. Resolve
  by decoding one real PumpSwap sell before shipping. Prod's decoder is unaffected either way
  — `decode_sell_ix` ignores trailing bytes.

### 4.3 Also preserved, lower priority

* **Raydium AMM v4**, 18-account swap order: `legacy/rust-legacy/pump-quant-core/src/tx/raydium.rs:235-252`.
  Not on any current execution path; retrieve from git history if a Raydium venue is ever added.
* **PumpSwap pool account byte map**: `momentum/pool.rs` — pool creator `[11..43]`,
  `base_mint` @43, `quote_mint` @75, `coin_creator` `[211..243]`, cashback flag `[244]`, with
  the two-pass `getProgramAccounts` memcmp strategy over offsets 43 and 75. Prod's
  `pump-quant-protocol::pumpswap` decoder should be diffed against this before Phase B trusts
  either. `legacy/scripts-legacy/validate-pumpswap-layout.sh` verified these offsets against
  a live RPC and is the natural regression harness.

---

## 5. What deletion of `legacy/` cost, and the two loose ends

Nothing in `rust/`, `scripts/`, `tools/`, `supervisor/`, `bench/`, `analysis/`, the single
GitHub workflow, or the root Python scripts imports, links, or executes anything under
`legacy/`. It was outside the Cargo workspace (`rust/Cargo.toml` lists 26 members, none of
them legacy, and has no `exclude` key at all). Every textual hit was the English word in a
comment, a data label (`"strategy_version": "momentum-legacy"`), or a doc citation.

Two loose ends, both cosmetic, both deliberately left alone rather than folded into this
commit — the working tree carries a large pre-existing CRLF churn and touching those files
would drag hundreds of line-ending-only diffs into an otherwise surgical change:

1. `supervisor/gates/checks.py:223` — `_SECRET_SCAN_EXCLUDE_DIRS` still lists `"legacy/"`.
   With the directory gone the prefix matches nothing; the gate still passes. Drop it in a
   later cleanup.
2. `docs/HERMES_PHASE_B_ACTIVATION_ONESHOT.md:296-306` cites five `legacy/...` paths as the
   documentation of the credential contract (`tx/wallet.rs` env vars, `wallet-store.ts`,
   `transfer-sol.js`, `config-legacy/keys/README.md`). That doc already declares it is
   describing a **separate legacy checkout on the server**, so it is not broken — but if no
   such checkout exists, those citations now point at nothing retrievable outside git history.

**Credential material: none was present.** `legacy/config-legacy/keys/` contained exactly one
file, a README, self-redacted (`Base58 private key: (REDACTED …)`). Its one piece of
non-recoverable knowledge is preserved here: the **Jito ShredStream whitelist auth pubkey is
`2HegzSo8YujghD4jxwLjAri5XsQmUTCVwmVqoZjs21Wq`**, submitted 2026-03-28, status pending at the
time of writing. That is a public key, not a secret, and it is the only record of *which*
keypair is registered.

Also removed and worth noting: two 16–18MB unstripped Linux ELF binaries (`pump-quant-live`)
committed to git, and `legacy/shredstream-proxy`, a gitlink (mode `160000`, commit
`d8b44814…`) with **no `.gitmodules` entry anywhere in the repo** — an orphan submodule
pointer that was already broken. Deleting the working-tree copies does not reclaim the
binaries from git history; that needs a history rewrite and is a separate decision.

---

## 6. Reproducing §2 and §3

```python
import hashlib
B58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
def b58enc(b):
    n = int.from_bytes(b, "big"); s = ""
    while n: n, r = divmod(n, 58); s = B58[r] + s
    for c in b:
        if c == 0: s = "1" + s
        else: break
    return s
def b58dec(s):
    n = 0
    for c in s: n = n * 58 + B58.index(c)
    b = n.to_bytes((n.bit_length() + 7) // 8, "big")
    return b"\0" * (len(s) - len(s.lstrip("1"))) + b

p = 2**255 - 19
d = (-121665 * pow(121666, p - 2, p)) % p
def is_on_curve(b32):                       # ed25519 point decompression
    y = int.from_bytes(b32, "little") & ((1 << 255) - 1)
    if y >= p: return False
    y2 = y * y % p
    u, v = (y2 - 1) % p, (d * y2 + 1) % p
    x = u * pow(v, 3, p) % p * pow(u * pow(v, 7, p) % p, (p - 5) // 8, p) % p
    vx2 = v * x * x % p
    if vx2 == u % p: pass
    elif vx2 == (-u) % p: x = x * pow(2, (p - 1) // 4, p) % p
    else: return False
    return not (x == 0 and b32[31] >> 7)

def find_pda(seeds, program_b58):           # SHA-256(seeds ++ bump ++ program ++ marker)
    prog = b58dec(program_b58)
    for bump in range(255, -1, -1):
        h = hashlib.sha256()
        for s in seeds: h.update(s)
        h.update(bytes([bump])); h.update(prog); h.update(b"ProgramDerivedAddress")
        if not is_on_curve(h.digest()): return b58enc(h.digest()), bump
    raise AssertionError("no off-curve bump")

PUMP  = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P"
PSWAP = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA"
print(find_pda([b"global"], PUMP))                    # -> 4wTV1Ymi…, 255
print(find_pda([b"global_config"], PSWAP))            # -> ADyA8hde…, 255
print(hashlib.sha256(b"global:buy").digest()[:8].hex())   # -> 66063d1201daebea
```

---

## 7. Phase-B checklist this file exists to serve

1. Decode one real successful bonding-curve `buy` and one `sell` from chain; diff the account
   list against §4.1; record the verifying slot in the §18.2 protocol registry. **Gate the
   first live entry on this.**
2. Same for PumpSwap `buy`/`sell` against §4.2, and settle the `sell` `track_volume` question.
3. Read `fee_recipient` from the decoded `Global` account. Do not hardcode it.
4. Decode `token_program` per mint — Token-2022 is a live case on this venue.
5. Decode cashback (bonding curve: byte 82; PumpSwap: pool byte 244) and branch the account
   list on it. Do not infer it from the token program.
6. Build the real `AccountMeta` list behind `ex_construction_gate`'s fixture-parity rung
   (criterion 77a) so a re-ordering fails on the laptop rather than on chain, and keep the
   synthetic placeholder path for the existing fixtures.
7. Refuse to build rather than substitute a default for any unresolved account. `Pubkey::default()`
   is the System Program and every venue validates it as something else.
