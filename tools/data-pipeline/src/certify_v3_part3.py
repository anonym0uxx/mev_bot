"""
SLINKY_GOLD_V3 — Final Certification Part 3
Opportunity diagnostics: missed-opportunity and false-positive rates
by age bucket, curve %, market-cap bucket, regime, and activity level.
"""
import duckdb
import json
from pathlib import Path

BASE = Path("D://repos/mev_bot/tools/data-pipeline/output/slinky_gold_v3")
con = duckdb.connect()
con.execute("SET threads TO 8;")
con.execute("SET memory_limit TO '48GB';")

def glob_pattern(layer):
    return str((BASE / layer).absolute()).replace("\\", "/") + "/*.parquet"

def build_read(layer):
    return f"read_parquet('{glob_pattern(layer)}')"

print("=" * 80)
print("SLINKY_GOLD_V3 — CERTIFICATION PART 3 (opportunity diagnostics)")
print("=" * 80)

s = build_read("pump_state_v3")
c = build_read("counterfactual_trade_v3")
p = build_read("policy_eval_v3")

# ═════════════════════════════════════════════════════════════════
# CHECK 10: OPPORTUNITY DIAGNOSTICS BY BUCKETS
# ═════════════════════════════════════════════════════════════════
print("\n[10] OPPORTUNITY DIAGNOSTICS BY BUCKET")
print("-" * 80)

# Build a joined query template: state + policy_eval + counterfactual
# We join on state_id
join_sql = f"""
    SELECT pe.state_id, pe.evaluation, pe.would_enter,
           cf.economic_class, cf.eligible, cf.exit_reason, cf.net_pnl_sol,
           st.seconds_since_launch, st.curve_pct_depleted, st.market_cap_sol,
           st.trade_count_so_far, st.price_sol
    FROM read_parquet('{glob_pattern('policy_eval_v3')}') pe
    JOIN read_parquet('{glob_pattern('counterfactual_trade_v3')}') cf ON pe.state_id = cf.state_id
    JOIN read_parquet('{glob_pattern('pump_state_v3')}') st ON pe.state_id = st.state_id
"""

def bucket_report(label, bucket_expr, bucket_order=None):
    """Report missed-opportunity and false-positive rates per bucket."""
    print(f"\n  ─── {label} ───")
    order_clause = bucket_order or bucket_expr
    sql = f"""
        WITH joined AS ({join_sql}),
        bucketed AS (
            SELECT
                {bucket_expr} as bucket,
                evaluation,
                COUNT(*) as cnt
            FROM joined
            GROUP BY {bucket_expr}, evaluation
        ),
        pivoted AS (
            SELECT
                bucket,
                SUM(CASE WHEN evaluation = 'CORRECT_ENTER' THEN cnt ELSE 0 END) as correct_enter,
                SUM(CASE WHEN evaluation = 'CORRECT_SKIP' THEN cnt ELSE 0 END) as correct_skip,
                SUM(CASE WHEN evaluation = 'FALSE_POSITIVE' THEN cnt ELSE 0 END) as false_positive,
                SUM(CASE WHEN evaluation = 'MISSED_OPPORTUNITY' THEN cnt ELSE 0 END) as missed,
                SUM(CASE WHEN evaluation = 'AMBIGUOUS' THEN cnt ELSE 0 END) as ambiguous,
                SUM(cnt) as total
            FROM bucketed
            GROUP BY bucket
        )
        SELECT
            bucket,
            total,
            correct_enter, correct_skip, false_positive, missed, ambiguous,
            -- missed rate among champion SKIPs
            CASE WHEN (missed + correct_skip) > 0
                THEN ROUND(missed * 100.0 / (missed + correct_skip), 2) ELSE NULL END as missed_rate_among_skips,
            -- missed share of profitable opportunities
            CASE WHEN (missed + correct_enter) > 0
                THEN ROUND(missed * 100.0 / (missed + correct_enter), 2) ELSE NULL END as missed_share_profitable,
            -- losing share of champion entries
            CASE WHEN (false_positive + correct_enter) > 0
                THEN ROUND(false_positive * 100.0 / (false_positive + correct_enter), 2) ELSE NULL END as losing_share_entries,
            -- classic FPR
            CASE WHEN (false_positive + correct_skip) > 0
                THEN ROUND(false_positive * 100.0 / (false_positive + correct_skip), 2) ELSE NULL END as classic_fpr
        FROM pivoted
        WHERE total > 1000
        ORDER BY total DESC
    """
    r = con.execute(sql).fetchall()
    print(f"    {'bucket':<20} {'total':>10} {'CE':>8} {'CS':>8} {'FP':>8} {'MO':>8} {'amb':>6} {'miss/skip':>10} {'miss/prof':>10} {'loss/ent':>10} {'fpr':>8}")
    for row in r:
        b = str(row[0])[:20]
        print(f"    {b:<20} {row[1]:>10,} {row[2]:>8,} {row[3]:>8,} {row[4]:>8,} {row[5]:>8,} {row[6]:>6,}  {str(row[7]):>10}  {str(row[8]):>10}  {str(row[9]):>10}  {str(row[10]):>8}")

# 10a: By age bucket (seconds_since_launch)
print("\n  [10a] By AGE BUCKET (seconds since launch)")
bucket_report(
    "Age bucket",
    """
    CASE
        WHEN seconds_since_launch < 30 THEN '0-30s'
        WHEN seconds_since_launch < 60 THEN '30-60s'
        WHEN seconds_since_launch < 120 THEN '60-120s'
        WHEN seconds_since_launch < 300 THEN '120-300s'
        WHEN seconds_since_launch < 600 THEN '300-600s'
        WHEN seconds_since_launch < 1800 THEN '600-1800s'
        ELSE '1800s+'
    END
    """
)

# 10b: By curve % bucket
print("\n  [10b] By CURVE % DEPLETED BUCKET")
bucket_report(
    "Curve % bucket",
    """
    CASE
        WHEN curve_pct_depleted < 10 THEN '0-10%'
        WHEN curve_pct_depleted < 25 THEN '10-25%'
        WHEN curve_pct_depleted < 50 THEN '25-50%'
        WHEN curve_pct_depleted < 75 THEN '50-75%'
        WHEN curve_pct_depleted < 90 THEN '75-90%'
        ELSE '90%+'
    END
    """
)

# 10c: By market cap bucket (SOL)
print("\n  [10c] By MARKET CAP BUCKET (SOL)")
bucket_report(
    "Market cap bucket",
    """
    CASE
        WHEN market_cap_sol < 5 THEN '<5 SOL'
        WHEN market_cap_sol < 20 THEN '5-20 SOL'
        WHEN market_cap_sol < 50 THEN '20-50 SOL'
        WHEN market_cap_sol < 100 THEN '50-100 SOL'
        WHEN market_cap_sol < 500 THEN '100-500 SOL'
        ELSE '500+ SOL'
    END
    """
)

# 10d: By activity level (trade_count_so_far)
print("\n  [10d] By ACTIVITY LEVEL (trade count so far)")
bucket_report(
    "Activity bucket",
    """
    CASE
        WHEN trade_count_so_far < 5 THEN '1-4 trades'
        WHEN trade_count_so_far < 10 THEN '5-9 trades'
        WHEN trade_count_so_far < 20 THEN '10-19 trades'
        WHEN trade_count_so_far < 50 THEN '20-49 trades'
        WHEN trade_count_so_far < 100 THEN '50-99 trades'
        ELSE '100+ trades'
    END
    """
)

# 10e: By regime — using price_sol momentum as proxy
print("\n  [10e] By PRICE REGIME (entry price SOL)")
bucket_report(
    "Price regime",
    """
    CASE
        WHEN price_sol < 0.000001 THEN '<1e-6 SOL'
        WHEN price_sol < 0.00001 THEN '1e-6 to 1e-5'
        WHEN price_sol < 0.0001 THEN '1e-5 to 1e-4'
        WHEN price_sol < 0.001 THEN '1e-4 to 1e-3'
        ELSE '1e-3+ SOL'
    END
    """
)

# 10f: Summary — overall rates for reference
print("\n  [10f] OVERALL RATES (reference)")
r = con.execute(f"""
    WITH joined AS ({join_sql})
    SELECT
        SUM(CASE WHEN evaluation = 'CORRECT_ENTER' THEN 1 ELSE 0 END) as ce,
        SUM(CASE WHEN evaluation = 'CORRECT_SKIP' THEN 1 ELSE 0 END) as cs,
        SUM(CASE WHEN evaluation = 'FALSE_POSITIVE' THEN 1 ELSE 0 END) as fp,
        SUM(CASE WHEN evaluation = 'MISSED_OPPORTUNITY' THEN 1 ELSE 0 END) as mo,
        SUM(CASE WHEN evaluation = 'AMBIGUOUS' THEN 1 ELSE 0 END) as amb
    FROM joined
""").fetchone()
ce, cs, fp, mo, amb = r
print(f"    CORRECT_ENTER={ce:,}  CORRECT_SKIP={cs:,}  FALSE_POSITIVE={fp:,}  MISSED={mo:,}  AMBIGUOUS={amb:,}")
print(f"    missed/skip={mo/(mo+cs)*100:.2f}%  missed/prof={mo/(mo+ce)*100:.2f}%  loss/ent={fp/(fp+ce)*100:.2f}%  fpr={fp/(fp+cs)*100:.2f}%")

con.close()
print("\n[10] COMPLETE")
