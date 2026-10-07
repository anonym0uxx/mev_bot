# Milestone 8: PumpSwap corpus-row producer, production-path comparison, durable flow history

Production path: wire (outer index + stack height per instruction) -> LaserStreamTx -> `corpus_rows::resolve_row`
(no preferred trader index for PumpSwap, as in the frozen builder) -> `AppEvent::CorpusFlowRow` -> flow reducer/ledger.
Seed = frozen-tape rows with recv < capture start, applied once. No tape rows for in-session events.

* 14 clocks, mint 51nH (S1): all 10 flow fields exact (prod14_51nH.fd.txt; remaining diffs are non-flow).
* 8-mint sample (rule + relaxation disclosed in milestone7; exploratory, NOT held-out): field_matrix_production.txt.
* Unclassified enriched clock resolved: S2 BHprQ1wT t=1788975741870. Corpus enriched block uses recv <= t and includes a
  trade received at exactly t (0.018822516 SOL, J8RfHzLg). Volume gap equals it to the lamport. Causal-contract
  discrepancy: availability of an equal-ms event at decision time is NOT proven; production keeps recv < t.
* Checkpoint (flow_checkpoint.rs): 6,389,425 seed events -> 257,214 wallets, 1,081,614 co-entry links, 103,354,879 B,
  persist 0.51 s, restore 0.53 s, re-encode byte-identical (ckpt_measure.json).
* Not wired into the daemon yet; no replay-source adapter yet.
