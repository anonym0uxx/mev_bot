"""
SLINKY_GOLD_V3 — Final Certification Part 2
Mint-level distributions, opportunity diagnostics, policy metrics with correct denominators.
"""
import duckdb
import json
import math
from pathlib import Path
from collections import Counter, defaultdict

BASE = Path("D://repos/mev_bot/tools/data-pipeline/output/slinky_gold_v3")
con = duckdb.connect()
con.execute("SET threads TO 8;")
con.execute("SET memory_limit TO '48GB';")

def glob_pattern(layer):
    return str((BASE / layer).absolute()).replace("\\", "/") + "/*.parquet"

def build_read(layer):
    return f"read_parquet('{glob_pattern(layer)}')"

manifest = json.loads((BASE / "manifest_v3.json").read_text())

print("=" * 80)
print("SLINKY_GOLD_V3 — CERTIFICATION PART 2 (mint-level + opportunity diagnostics)")
print("=" * 80)

# ═════════════════════════════════════════════════════════════════
# CHECK 8: MINT-LEVEL DISTRIBUTIONS
# ═════════════════════════════════════════════════════════════════
print("\n[8] MINT-LEVEL DISTRIBUTIONS — concentration analysis")
print("-" * 80)

s = build_read("pump_state_v3")
o = build_read("pump_outcome_v3")
c = build_read("counterfactual_trade_v3")
p = build_read("policy_eval_v3")

# 8a: States per mint — distribution stats
r = con.execute(f"""
    SELECT 
        COUNT(*) as n_mints,
        MIN(cnt) as min_states,
        MAX(cnt) as max_states,
        AVG(cnt)::DOUBLE as avg_states,
        percentile_cont(0.5) WITHIN GROUP (ORDER BY cnt) as median_states,
        percentile_cont(0.1) WITHIN GROUP (ORDER BY cnt) as p10,
        percentile_cont(0.9) WITHIN GROUP (ORDER BY cnt) as p90,
        percentile_cont(0.99) WITHIN GROUP (ORDER BY cnt) as p99,
        SUM(cnt) as total_states
    FROM (
        SELECT mint, COUNT(*) as cnt FROM {s} GROUP BY mint
    )
""").fetchone()
print(f"  States per mint:")
print(f"    mints: {r[0]:,}")
print(f"    total states: {r[8]:,}")
print(f"    min: {r[1]}  max: {r[2]:,}  mean: {r[3]:.1f}  median: {r[4]:.1f}")
print(f"    p10: {r[5]:.1f}  p90: {r[6]:.1f}  p99: {r[7]:.1f}")

# 8b: Top-N mints concentration
r2 = con.execute(f"""
    WITH mint_counts AS (
        SELECT mint, COUNT(*) as cnt FROM {s} GROUP BY mint
    ),
    ranked AS (
        SELECT mint, cnt,
               ROW_NUMBER() OVER (ORDER BY cnt DESC) as rk,
               SUM(cnt) OVER (ORDER BY cnt DESC) as cumulative
        FROM mint_counts
    )
    SELECT rk, mint, cnt, cumulative FROM ranked
    WHERE rk <= 20
""").fetchall()
print(f"\n  Top-20 mints by state count:")
total_states = r[8]
for row in r2:
    pct_of_total = row[2] / total_states * 100
    cum_pct = row[3] / total_states * 100
    print(f"    #{row[0]:>3} {row[1][:16]}...  {row[2]:>8,} states ({pct_of_total:.2f}%)  cum={cum_pct:.2f}%")

# 8c: Gini coefficient of states-per-mint
r3 = con.execute(f"""
    WITH mint_counts AS (
        SELECT COUNT(*) as cnt FROM {s} GROUP BY mint
    )
    SELECT cnt FROM mint_counts ORDER BY cnt
""").fetchall()
counts = [x[0] for x in r3]
n = len(counts)
total_sum = sum(counts)
gini_num = sum((2 * i - n - 1) * counts[i-1] for i in range(1, n+1))
gini = gini_num / (n * total_sum) if n * total_sum > 0 else 0
print(f"\n  Gini coefficient (states/mint inequality): {gini:.4f}")
print(f"  (0=perfectly equal, 1=maximally concentrated)")

# 8d: Share of states from top 1%/5%/10% of mints
p1_idx = int(n * 0.99)
p5_idx = int(n * 0.95)
p10_idx = int(n * 0.90)
sorted_desc = sorted(counts, reverse=True)
top1_sum = sum(sorted_desc[:max(1, int(n*0.01))])
top5_sum = sum(sorted_desc[:max(1, int(n*0.05))])
top10_sum = sum(sorted_desc[:max(1, int(n*0.10))])
print(f"  Top 1% of mints ({int(n*0.01)} mints): {top1_sum:,} states ({top1_sum/total_sum*100:.1f}% of all)")
print(f"  Top 5% of mints ({int(n*0.05)} mints): {top5_sum:,} states ({top5_sum/total_sum*100:.1f}% of all)")
print(f"  Top 10% of mints ({int(n*0.10)} mints): {top10_sum:,} states ({top10_sum/total_sum*100:.1f}% of all)")

# 8e: Unique mints per economic class
print(f"\n  Unique mints per economic class:")
r4 = con.execute(f"""
    SELECT economic_class, COUNT(DISTINCT mint) as n_mints, COUNT(*) as n_states
    FROM {c}
    GROUP BY economic_class
    ORDER BY n_states DESC
""").fetchall()
for row in r4:
    print(f"    {row[0]:<10}: {row[1]:>8,} mints, {row[2]:>12,} states")

# 8f: Median states per mint for STRONG/TOXIC mints
r5 = con.execute(f"""
    WITH class_mint AS (
        SELECT mint, economic_class, COUNT(*) as cnt
        FROM {c}
        WHERE economic_class IN ('STRONG', 'GOOD', 'TOXIC', 'BAD')
        GROUP BY mint, economic_class
    )
    SELECT economic_class,
        COUNT(DISTINCT mint) as n_mints,
        AVG(cnt)::DOUBLE as mean_states,
        percentile_cont(0.5) WITHIN GROUP (ORDER BY cnt) as median_states
    FROM class_mint
    GROUP BY economic_class
    ORDER BY mean_states DESC
""").fetchall()
print(f"\n  States/mint by economic class (among mints that have that class):")
for row in r5:
    print(f"    {row[0]:<10}: {row[1]:>8,} mints  mean={row[2]:.1f}  median={row[3]:.1f}")

# ═════════════════════════════════════════════════════════════════
# CHECK 9: POLICY METRICS WITH CORRECT DENOMINATORS
# ═════════════════════════════════════════════════════════════════
print("\n[9] POLICY EVALUATION — CORRECT DENOMINATORS (whole corpus)")
print("-" * 80)

r = con.execute(f"""
    SELECT evaluation, COUNT(*) as cnt
    FROM {p}
    GROUP BY evaluation
    ORDER BY cnt DESC
""").fetchall()
eval_counts = {row[0]: row[1] for row in r}
total = sum(eval_counts.values())
print(f"  Policy evaluation counts (total={total:,}):")
for ev, cnt in sorted(eval_counts.items(), key=lambda x: -x[1]):
    print(f"    {ev:<22}: {cnt:>12,} ({cnt/total*100:.2f}%)")

CORRECT_ENTER = eval_counts.get("CORRECT_ENTER", 0)
CORRECT_SKIP = eval_counts.get("CORRECT_SKIP", 0)
FALSE_POSITIVE = eval_counts.get("FALSE_POSITIVE", 0)
MISSED_OPPORTUNITY = eval_counts.get("MISSED_OPPORTUNITY", 0)
AMBIGUOUS = eval_counts.get("AMBIGUOUS", 0)

print(f"\n  POLICY METRICS WITH EXPLICIT DENOMINATORS:")
print(f"  ──────────────────────────────────────────────────────────")

# 1. Missed among champion SKIPS = MISSED / (MISSED + CORRECT_SKIP)
denom1 = MISSED_OPPORTUNITY + CORRECT_SKIP
rate1 = MISSED_OPPORTUNITY / denom1 * 100 if denom1 > 0 else 0
print(f"  1. Missed-opportunity rate (among champion SKIPs):")
print(f"     = MISSED / (MISSED + CORRECT_SKIP)")
print(f"     = {MISSED_OPPORTUNITY:,} / ({MISSED_OPPORTUNITY:,} + {CORRECT_SKIP:,})")
print(f"     = {MISSED_OPPORTUNITY:,} / {denom1:,}")
print(f"     = {rate1:.2f}%")
print(f"     → Champion skips {rate1:.1f}% of states where it could have profited")

# 2. Missed share of actual profitable opportunities = MISSED / (MISSED + CORRECT_ENTER)
denom2 = MISSED_OPPORTUNITY + CORRECT_ENTER
rate2 = MISSED_OPPORTUNITY / denom2 * 100 if denom2 > 0 else 0
print(f"\n  2. Missed share of profitable opportunities:")
print(f"     = MISSED / (MISSED + CORRECT_ENTER)")
print(f"     = {MISSED_OPPORTUNITY:,} / ({MISSED_OPPORTUNITY:,} + {CORRECT_ENTER:,})")
print(f"     = {MISSED_OPPORTUNITY:,} / {denom2:,}")
print(f"     = {rate2:.2f}%")
print(f"     → Of all profitable candidate states, champion misses {rate2:.1f}%")

# 3. Losing share of champion ENTRIES = FALSE_POSITIVE / (FALSE_POSITIVE + CORRECT_ENTER)
denom3 = FALSE_POSITIVE + CORRECT_ENTER
rate3 = FALSE_POSITIVE / denom3 * 100 if denom3 > 0 else 0
print(f"\n  3. Losing share of champion entries:")
print(f"     = FALSE_POSITIVE / (FALSE_POSITIVE + CORRECT_ENTER)")
print(f"     = {FALSE_POSITIVE:,} / ({FALSE_POSITIVE:,} + {CORRECT_ENTER:,})")
print(f"     = {FALSE_POSITIVE:,} / {denom3:,}")
print(f"     = {rate3:.2f}%")
print(f"     → Of all champion ENTERs, {rate3:.1f}% lose money")

# 4. Classic FPR = FALSE_POSITIVE / (FALSE_POSITIVE + CORRECT_SKIP)
denom4 = FALSE_POSITIVE + CORRECT_SKIP
rate4 = FALSE_POSITIVE / denom4 * 100 if denom4 > 0 else 0
print(f"\n  4. Classic false-positive rate:")
print(f"     = FALSE_POSITIVE / (FALSE_POSITIVE + CORRECT_SKIP)")
print(f"     = {FALSE_POSITIVE:,} / ({FALSE_POSITIVE:,} + {CORRECT_SKIP:,})")
print(f"     = {FALSE_POSITIVE:,} / {denom4:,}")
print(f"     = {rate4:.2f}%")
print(f"     → Of all SKIP decisions, {rate4:.1f}% were false positives")

# Eligible states context
eligible_states = FALSE_POSITIVE + CORRECT_ENTER + MISSED_OPPORTUNITY + CORRECT_SKIP
print(f"\n  Eligible states (sum of 4 non-ambiguous): {eligible_states:,}")
print(f"  (AMBIGUOUS={AMBIGUOUS:,} excluded from denominators — MARGINAL class)")

con.close()
print("\n[8-9] COMPLETE")
