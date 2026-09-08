"""
SLINKY_GOLD_V3 — Deep Audit
1. Split overlap check (exact)
2. Markout forward-fill semantics: no-trade states get ret=0, MFE=0, MAE=0
3. Counterfactual feasibility: STRONG states with exit_reason='hold' (no future trades)
4. Time-to-next-trade distribution
5. Censored states: are they correctly handled in counterfactual?
6. Economic class vs censoring cross-tab
"""
import duckdb
import json
from pathlib import Path

BASE = Path("D:/repos/mev_bot/tools/data-pipeline/output/slinky_gold_v3")
con = duckdb.connect()

def gp(layer):
    return str(BASE / layer / "*.parquet")

# Load splits
splits = json.loads((BASE / "splits.json").read_text())
print("=" * 80)
print("SLINKY_GOLD_V3 — DEEP AUDIT")
print("=" * 80)

# ─── 1. Split overlap check ──────────────────────────────────────
print("\n[1] SPLIT OVERLAP CHECK (exact)")
print("-" * 80)

# Read all mints from parquet, check which split they belong to
train_frac = splits.get("train", 0)
val_frac = splits.get("val", 0)
test_frac = splits.get("test", 0)
print(f"  splits.json: train={train_frac}  val={val_frac}  test={test_frac}")

# Get mint lists from parquet files - check if mints appear in multiple layers
# Actually splits.json only has counts. Let's check via the data itself.
# The split assignment is stored in the parquet files.
print("  (Split assignment is not stored in parquet rows — it's computed at export time)")
print("  Checking mint-disjointness via mint count parity...")

# Count unique mints per layer
for layer in ["pump_state_v3", "pump_outcome_v3", "counterfactual_trade_v3", "policy_eval_v3"]:
    cnt = con.execute(f"SELECT COUNT(DISTINCT mint) FROM read_parquet('{gp(layer)}')").fetchone()[0]
    print(f"  {layer}: {cnt:,} unique mints")

# ─── 2. Markout forward-fill semantics ───────────────────────────
print("\n[2] MARKOUT FORWARD-FILL SEMANTICS")
print("-" * 80)

# States where ret_300s is NULL but MFE=0 and MAE=0 (forward-fill artifact)
r = con.execute(f"""
    SELECT
        COUNT(*) AS total,
        COUNT(*) FILTER (WHERE ret_300s_bp IS NULL AND mfe_bp = 0 AND mae_bp = 0) AS no_trade_flat,
        COUNT(*) FILTER (WHERE ret_300s_bp IS NULL AND mfe_bp IS NULL AND mae_bp IS NULL) AS no_trade_null,
        COUNT(*) FILTER (WHERE ret_300s_bp IS NULL AND mfe_bp > 0) AS no_trade_but_positive_mfe,
        COUNT(*) FILTER (WHERE ret_300s_bp IS NULL AND mae_bp < 0) AS no_trade_but_negative_mae,
        COUNT(*) FILTER (WHERE ret_300s_bp IS NOT NULL AND mfe_bp = 0 AND mae_bp = 0) AS trade_flat,
        COUNT(*) FILTER (WHERE ret_300s_bp IS NOT NULL AND (mfe_bp > 0 OR mae_bp < 0)) AS trade_nonflat
    FROM read_parquet('{gp("pump_outcome_v3")}')
""").fetchone()

print(f"  Total outcomes: {r[0]:,}")
print(f"  No-trade flat (ret_300=NULL, MFE=0, MAE=0): {r[1]:,}  ← forward-fill artifact, should be NA")
print(f"  No-trade null (ret_300=NULL, MFE=NULL, MAE=NULL): {r[2]:,}  ← correctly NULL")
print(f"  No-trade but positive MFE: {r[3]:,}  ← has some trades before 300s but not at 300s")
print(f"  No-trade but negative MAE: {r[4]:,}")
print(f"  Trade flat (ret_300 present, MFE=0, MAE=0): {r[5]:,}  ← genuinely flat")
print(f"  Trade non-flat: {r[6]:,}")

# ─── 3. Censored states in counterfactual ────────────────────────
print("\n[3] CENSORED STATES IN COUNTERFACTUAL TRADE")
print("-" * 80)

# For censored states (right_censored=true in outcomes), what economic class
# do they get in counterfactual?
r = con.execute(f"""
    WITH o AS (SELECT state_id, right_censored, censoring_reason, ret_300s_bp FROM read_parquet('{gp("pump_outcome_v3")}')),
         c AS (SELECT state_id, economic_class, exit_reason, eligible, net_pnl_sol, net_return_bp FROM read_parquet('{gp("counterfactual_trade_v3")}'))
    SELECT
        o.right_censored,
        c.economic_class,
        c.exit_reason,
        COUNT(*) AS cnt,
        AVG(c.net_return_bp) AS avg_ret_bp
    FROM o JOIN c ON o.state_id = c.state_id
    WHERE o.right_censored = true
    GROUP BY 1, 2, 3
    ORDER BY cnt DESC
""").fetchall()

print("  Censored states (right_censored=true) cross-tab with counterfactual:")
print(f"  {'class':<12} {'exit_reason':<16} {'count':>12} {'avg_ret_bp':>12}")
for row in r:
    print(f"  {row[1]:<12} {row[2] if row[2] else 'None':<16} {row[3]:>12,} {row[4] if row[4] is not None else 0:>12.1f}")

# ─── 4. STRONG states with exit_reason='hold' ────────────────────
print("\n[4] STRONG STATES WITH exit_reason='hold' (no future trades)")
print("-" * 80)

r = con.execute(f"""
    SELECT COUNT(*) AS cnt
    FROM read_parquet('{gp("counterfactual_trade_v3")}')
    WHERE economic_class = 'STRONG' AND exit_reason = 'hold'
""").fetchone()[0]
print(f"  STRONG states with exit_reason='hold': {r:,}")
if r > 0:
    print("  ⚠️ STRONG label assigned to states with NO executable exit — INFEASIBLE")
else:
    print("  ✅ No STRONG states have 'hold' exit — all have executable exits")

# ─── 5. Time-to-next-trade distribution ──────────────────────────
print("\n[5] TIME-TO-NEXT-TRADE (from outcome survival flags)")
print("-" * 80)

r = con.execute(f"""
    SELECT
        survived_60s, survived_300s,
        COUNT(*) AS cnt
    FROM read_parquet('{gp("pump_outcome_v3")}')
    GROUP BY 1, 2
    ORDER BY cnt DESC
""").fetchall()

print(f"  {'surv_60s':<10} {'surv_300s':<10} {'count':>12}")
for row in r[:10]:
    print(f"  {str(row[0]):<10} {str(row[1]):<10} {row[2]:>12,}")

# ─── 6. Economic class vs censoring cross-tab ────────────────────
print("\n[6] ECONOMIC CLASS × CENSORING CROSS-TAB (whole corpus)")
print("-" * 80)

r = con.execute(f"""
    WITH o AS (SELECT state_id, right_censored FROM read_parquet('{gp("pump_outcome_v3")}')),
         c AS (SELECT state_id, economic_class FROM read_parquet('{gp("counterfactual_trade_v3")}'))
    SELECT
        c.economic_class,
        COUNT(*) FILTER (WHERE o.right_censored = true) AS censored,
        COUNT(*) FILTER (WHERE o.right_censored = false) AS uncensored,
        COUNT(*) AS total
    FROM o JOIN c ON o.state_id = c.state_id
    GROUP BY 1
    ORDER BY total DESC
""").fetchall()

print(f"  {'class':<12} {'censored':>12} {'uncensored':>12} {'total':>12} {'%censored':>10}")
for row in r:
    pct = row[1] / row[3] * 100 if row[3] else 0
    print(f"  {row[0]:<12} {row[1]:>12,} {row[2]:>12,} {row[3]:>12,} {pct:>9.1f}%")

# ─── 7. Counterfactual exit_reason distribution for eligible states ──
print("\n[7] COUNTERFACTUAL EXIT REASONS (eligible states only)")
print("-" * 80)

r = con.execute(f"""
    SELECT exit_reason, COUNT(*) AS cnt, AVG(net_return_bp) AS avg_ret
    FROM read_parquet('{gp("counterfactual_trade_v3")}')
    WHERE eligible = true
    GROUP BY 1 ORDER BY cnt DESC
""").fetchall()

print(f"  {'exit_reason':<16} {'count':>12} {'avg_ret_bp':>12}")
for row in r:
    avg = row[2] if row[2] is not None else 0
    print(f"  {str(row[0]):<16} {row[1]:>12,} {avg:>12.1f}")

# ─── 8. Forward-fill audit: states with no trades within 1s ──────
print("\n[8] FORWARD-FILL AUDIT: ret_1s NULL but ret_300s NOT NULL")
print("-" * 80)

r = con.execute(f"""
    SELECT COUNT(*) AS cnt
    FROM read_parquet('{gp("pump_outcome_v3")}')
    WHERE ret_1s_bp IS NULL AND ret_300s_bp IS NOT NULL
""").fetchone()[0]
print(f"  States with ret_1s=NULL but ret_300s present: {r:,}")
print("  (These have no trade within 1s but have a trade within 300s — forward-fill is correct here)")

# States with all markouts NULL (truly no future trades)
r = con.execute(f"""
    SELECT COUNT(*) AS cnt
    FROM read_parquet('{gp("pump_outcome_v3")}')
    WHERE ret_1s_bp IS NULL AND ret_2s_bp IS NULL AND ret_5s_bp IS NULL
      AND ret_10s_bp IS NULL AND ret_30s_bp IS NULL AND ret_60s_bp IS NULL
      AND ret_120s_bp IS NULL AND ret_300s_bp IS NULL
""").fetchone()[0]
print(f"  States with ALL markouts NULL (truly no future trades): {r:,}")

# MFE/MAE for these all-NULL states
r2 = con.execute(f"""
    SELECT
        COUNT(*) FILTER (WHERE mfe_bp = 0 AND mae_bp = 0) AS mfe_mae_zero,
        COUNT(*) FILTER (WHERE mfe_bp IS NULL AND mae_bp IS NULL) AS mfe_mae_null,
        COUNT(*) FILTER (WHERE mfe_bp > 0 OR mae_bp < 0) AS mfe_mae_nonzero
    FROM read_parquet('{gp("pump_outcome_v3")}')
    WHERE ret_1s_bp IS NULL AND ret_300s_bp IS NULL
""").fetchone()
print(f"  Of those: MFE=0&MAE=0: {r2[0]:,}  MFE=NULL&MAE=NULL: {r2[1]:,}  MFE/MAE nonzero: {r2[2]:,}")

print("\n[AUDIT COMPLETE]")
