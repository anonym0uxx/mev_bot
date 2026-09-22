# training/ — the RL/SFT code of record (mirror)

This directory is a **read-only mirror** of the training-side git repo of record. The
code here is not executed from the mev_bot tree: it runs on the Linux training box from
`/training/v2`, and this mirror exists so the training tree is versioned, pushed and
recoverable from the same place as the bot it trains.

## 1:1 path correspondence

```
training/code/src/v2/rl/X.py   ==   /training/v2/code/src/v2/rl/X.py   (the file that runs)
training/reports/Y.json        ==   /training/v2/reports/Y.json        (the frozen artifact)
```

Paths inside the scripts are absolute (`/training/v2/...`), so the mirror location does
not affect execution.

## Sync (from the training box)

```bash
git -C /home/alon/build/mev_bot_gh checkout -b task/<slug> origin/main   # never commit on main
cd /home/alon/build/mev_bot_gh && rm -rf training && mkdir training
git -C /training/v2 archive HEAD | tar -x -C training
git add -A training && git commit -m "training: sync from /training/v2 @ <sha>"
git push -u origin task/<slug>
```

The mirror is a **snapshot**, not a submodule: `/training/v2` is a separate repo with its
own history; this directory carries the current tracked state of it.

## What is here (and what is not)

- `code/src/v2/rl/` — the RL/SFT pipeline: reward engine (`rl_reward_v3.py`,
  `reward_engine.py`, `reward_terms.py`), corpus builders, the GRPO loop
  (`grpo_loop.py`), the exit/management policy, the calibration scripts that freeze the
  measured costs, and the checkpoint-selection gates.
- `code/src/v2/rl/parity_rust/` — the Python↔Rust parity fixture generators.
- `code/systemd/` — the chain/watchdog units the training box runs.
- `reports/` — the **frozen** artifacts the code is required to read rather than
  hardcode: `MEMORIZATION_BOUNDS.json` (the preregistered selection veto),
  `MEM_LIMIT_POWER_STUDY.json` (why the veto samples 400 rows/group),
  `FILL_DRIFT_C15.json`, `EXIT_IMPAIRMENT_C14.json`, `FEE_QUANTILES_C16.json`.
- **Not** here: corpora, tapes, reserves, checkpoints, model weights. Those are bulk
  data and stay on the training box (`/training/v2/...`); they are reproducible from the
  documented builders in `code/src/v2/rl/`.

## The selection contract (why much of this code exists)

A checkpoint is selectable **only** with (a) a wall lower bound above the floor
(`grpo_loop.run_gate` / `grpo_trainer.gate_lower_bound`) **and** (b) a CLEAN verdict from
the preregistered memorization instrument. With `selection.emit_memorization_records` on,
the records the detector scores are emitted from the candidate policy itself at
evaluation time (`emit_memorization_records.py`), and the file is read back and refused
unless it names that checkpoint — so a verdict can never belong to a different policy
than the one being judged. Training reward is never a selection input.
