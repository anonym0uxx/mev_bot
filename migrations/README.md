# migrations/ - historical SQLite schema (read-compat only)

These five SQL files created the schema of the legacy TypeScript-era database `pump-quant.db`
(documented in docs/RUNBOOK.md "Database Management"). The Rust runtime does not use SQLite.

The retained database on the operator host (`/mnt/data/Users/Alon/mev_bot/pump-quant/data/pump-quant.db`,
1.31 GB, last written 2026-03-28) has `schema_migrations` rows 1-5 applied, so these files are the only in-repo
description of its tables (raw_events 1.43M rows, state_transitions 268k, orders 8.2k, positions 2.6k, ...).
Keep them so the DB stays readable and restorable. Do not apply them to a new database.

Checked read-only (`mode=ro&immutable=1`); nothing in that file was modified.
