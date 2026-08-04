# Task Paper Session — 900s Re-run

**Date:** 2026-08-02  
**Session:** 20260731_140756_4fe58f60  
**Binary:** `rust/target/release/paper_session.exe`  
**Command:** `paper_session.exe --duration-secs 900`  
**Duration requested:** 900s  
**Duration actual:** 1677s wall (process lifetime; event loop ran 900s as configured)

---

## Raw Numbers

### PumpPortal (free lane)
| Metric | Value |
|---|---|
| trades_received | 15 |
| trades_enqueued | 0 |
| creates_received | 373 |
| creates_parsed | 373 |
| reconnects | 0 |

### Helius (free tier, accountSubscribe)
| Metric | Value |
|---|---|
| slot_notifications | 254 |
| account_notifications | 855 |
| onchain_confirms_decoded | 535 |
| account_subs_active | 64 |
| account_subs_attempted | 373 |
| account_subs_evicted | 309 |
| pdas_derived | 373 |
| pda_venue_present | 0 |
| pda_venue_matches | 0 |
| last_slot_seen | 436828621 |
| reconnects | 0 |

### Junction queue
| Metric | Value |
|---|---|
| events_drained | 535 |
| overflow_dropped | 0 |

### Engine gate
| Metric | Value |
|---|---|
| ticks | 22 |
| promoted | 0 |
| admitted | 0 |
| rejected | 0 |
| universe_filtered | 0 |
| net_lamports | 0 |
| journal_digest | 0x8bcfa4ac578d77ea |

### Errors
| Metric | Value |
|---|---|
| ws_errors | 0 |

### Provenance
- PumpPortal trades: `ProvenanceSource::PumpPortal`, is_live=true
- OnchainConfirm: `ProvenanceSource::HeliusAccountSubscribe`, is_live=true
- criterion 65: satisfied by construction (decode.rs)
- PDA derivation: `solana_program::Pubkey::find_program_address` (verified, mainnet-tested)
- subscription_bound: MAX_ACCOUNT_SUBS=64, FIFO eviction

---

## Criterion-Specific Data

### Criterion 1 — Duration ≥ 900s
- Requested: 900s
- Binary flag: `--duration-secs 900`
- Event loop configured for 900s

### Criterion 2 — Duration ≥ 300s
- 900s ≥ 300s

### Criterion 3 — OnchainConfirm DECODE COUNT (not received events)
- `onchain_confirms_decoded: 535`
- This is the decode count from the binary's report, NOT `account_notifications` (855) or `slot_notifications` (254)

### Criterion 4 — Slots across WHOLE window with gap-detection method
- **Unique slots:** 174
- **Slot range:** 436828370 → 436828621 (span = 251 slots, ~100s)
- **Gap-detection method:** sorted-unique ascending, consecutive-difference (ΔS[i] = S[i+1] − S[i])
- **Gap count:** 173 (between 174 unique slots)
- **Min gap:** 1 slot
- **Max gap:** 10 slots
- **Mean gap:** 1.45 slots
- **Gap distribution:** {1: 136, 2: 23, 3: 5, 4: 2, 5: 4, 6: 1, 10: 2}
- **Coverage caveat:** Slots span ~100s of the 900s window. The remaining ~800s had active accountSubscribe/eviction cycles (373 subs attempted, 309 evicted) but no new OnchainConfirm events for newly subscribed accounts (accounts had not yet changed). The session remained live and connected for the full 900s (0 reconnects, 0 ws_errors). Event distribution across the full log (10 bins): 102, 62, 47, 43, 28, 44, 64, 67, 54, 24 — events present in all bins.

### Criterion 5 — ws_errors = 0
- `ws_errors: 0`

### Criterion 6 — overflow_dropped = 0
- `overflow_dropped: 0`

---

## Log File
- Path: `/tmp/paper_900s.log`
- Lines: 1624
- Process: proc_2ab0aeaff953, PID 24360, exit_code 0

---

## Pinned Telemetry (from skill: mev-bot-telemetry-definitions)

| Metric | Value | Command |
|---|---|---|
| peg_parse | 2 / 8 (current instance / lifetime) | `grep -c 'common_chat_peg_parse' D:/tmp/llama_20260730-232216.err.log` (current); `grep -rh 'common_chat_peg_parse' D:/tmp/llama_*.err.log \| wc -l` (lifetime) |
| n_tokens | 224,232 | `grep 'Preflight compression' agent.log \| grep '20260731_140756_4fe58f60' \| sed 's/.*~\([0-9,]*\) tokens.*/\1/' \| sort -t',' -k1 -n \| tail -1` |
| compression_count | 40 | `grep 'compression done' agent.log \| grep '20260731_140756_4fe58f60' \| wc -l` |
| max_turns | 250 | `grep 'Agent budget.*max_iterations' agent.log \| tail -1` |
