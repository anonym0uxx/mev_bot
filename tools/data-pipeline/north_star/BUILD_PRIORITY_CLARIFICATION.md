# Operator clarification: build now; live on-chain inputs remain essential

## Direction

The operator wants the highest-quality North Star training dataset built now, targeting end of day today, without waiting for the September 16–23 prospective evaluation window. The operator also explicitly reaffirmed that the trading bot will use LaserStream and active on-chain data streams at runtime.

## Execution interpretation

- Continue aggregation, deterministic canonicalization, cleanup, causal reconstruction and eligible training-export preparation now. The future evaluation calendar is not a reason to idle these tasks.
- Treat training-corpus construction/freeze and later prospective economic evaluation as distinct deliverables. Completion of future evaluation is not needed merely to freeze eligible training bytes. Full master certification and economic claims still require their actual evidence; do not claim all stages complete from a train-only snapshot.
- Do not disable existing fit/admission gates to meet the date. Current routing/custody, source lineage, verified economics and narrative timing gaps are substantive, independent of waiting for future dates. Resolve legitimate development split/fit preconditions without requiring future outcomes.
- The end-of-day request is a target, not an evidence-backed guarantee of complete D01–D14/G01–G12 coverage or certification. Report actual admitted, rejected, quarantined and missing coverage rather than padding or promoting unsupported records.
- Preserve the preregistered evaluation policy and exposed-source history. Do not silently cancel, move, reclassify or retrospectively choose windows.

## Runtime architecture and training implications

LaserStream/live ingestion supplies actual on-chain observations to Rust. Rust validates, normalizes and maintains point-in-time market/account/portfolio state for Qwen's trading decisions and deterministic execution checks. Qwen is the trading brain, not a replacement for the data feed or the Rust engine.

Training examples must teach decisions conditioned on those causal input schemas, including data age, commitment/finality, gaps, uncertainty, pending orders and current inventory. Match training feature definitions to runtime producers; do not teach static memorization of historical prices or feed future outcomes as current inputs. Required narrative evidence remains a separate co-temporal input dimension.

A live feed does not retroactively reconstruct missing historical state or authenticate old narrative timing. Source-only encoder fixes and offline helpers are not deployed runtime integrations. No new subscriptions, spending overages, live trades or collector replacement are authorized by this clarification alone.
