#!/usr/bin/env python3
"""Build-time derivability guard.

The v1 SFT corpus failed because the target was recoverable from the prompt: a
flat wallet mapped to BUY 99.9% of the time, so the model learned a one-bit
lookup instead of a decision. Aggregate loss curves did not reveal this -- the
val loss looked superb precisely BECAUSE the task was trivial.

This guard runs before a corpus is allowed to train and fails loudly when a
target is derivable. Three independent checks, because they fail differently:

  1. LEXICAL  -- the target string (or its decision token) literally appears in
                 the prompt. Trivially copyable under teacher forcing.
  2. CONDITIONAL ENTROPY -- bucket records by a discretised view of the prompt
                 state; if any well-populated bucket has a majority label above
                 CLEAN_SHARE, the label is a function of that state.
  3. MARGINALS -- overall label balance. Necessary but NOT sufficient: v1 had
                 healthy-looking marginals (EXIT 46.6 / BUY 28.3 / ADD 18.2 /
                 REDUCE 6.8) while the conditional structure was degenerate.

Exit code is non-zero if any check fails, so a build pipeline can gate on it.

Usage:
  derivability_guard.py <records.jsonl> [--threshold 0.90] [--min-bucket 50]
"""
import argparse
import json
import re
import sys
from collections import Counter, defaultdict

# The decision token may carry a _RECORDED / _ACTION suffix (the corpora use
# "SOURCE ATTRIBUTION: REDUCE_RECORDED"). A naive \bREDUCE\b fails on that,
# because '_' is a word character -- which silently extracted zero labels and
# let this guard report PASS on a corpus with the answer printed in the prompt.
DECISION_RE = re.compile(
    r'\b(BUY|SELL|EXIT|ADD|REDUCE|HOLD|WATCH|SKIP|NO_?ACTION)(?:_RECORDED|_ACTION|_TAKEN)?\b',
    re.I)
# State features we are willing to condition on. Each is a coarse view of the
# prompt that a decision SHOULD depend on -- if the label is a function of any
# one of these alone, the task is a lookup rather than a judgment.
STATE_PATTERNS = {
    'position_status': re.compile(r'POSITION[^A-Za-z0-9]{0,4}(FLAT|NONE|OPEN|HELD|HOLDING)', re.I),
    'wallet_activity': re.compile(r'(?:MOVES|TRADES|ACTIVITY|N_PRIOR)[^0-9]{0,8}(\d+)', re.I),
    # The corpora state the source label in the prompt itself. Conditioning on
    # it makes the leak explicit and measurable instead of inferential.
    'stated_source_label': re.compile(r'SOURCE ACTION LABEL[:\s]+([A-Za-z_]+)', re.I),
    'prior_movements': re.compile(r'window has\s+(\d+)\s+wallet', re.I),
}


def load(path):
    recs = []
    with open(path) as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                recs.append(json.loads(line))
            except Exception:
                continue
    return recs


def split_prompt_target(rec):
    """Return (prompt_text, target_text) for the record shapes we produce."""
    msgs = rec.get('messages')
    if isinstance(msgs, list) and msgs:
        prompt = '\n'.join(m.get('content', '') for m in msgs
                           if m.get('role') in ('system', 'user'))
        target = '\n'.join(m.get('content', '') for m in msgs
                           if m.get('role') == 'assistant')
        if target:
            return prompt, target
    if 'prompt' in rec and 'target' in rec:
        return str(rec['prompt']), str(rec['target'])
    # CPT-shaped records carry a single `content` body.
    body = rec.get('content')
    if isinstance(body, str):
        return '', body
    return '', ''


def decision_of(text):
    m = DECISION_RE.search(text or '')
    return m.group(1).upper() if m else None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('records')
    ap.add_argument('--threshold', type=float, default=0.90,
                    help='majority-label share within a bucket that fails the build')
    ap.add_argument('--min-bucket', type=int, default=50,
                    help='ignore buckets smaller than this (not evidence either way)')
    ap.add_argument('--json-out')
    args = ap.parse_args()

    recs = load(args.records)
    if not recs:
        print('GUARD: FAIL -- no parseable records (refusing to certify an empty corpus)')
        return 2

    problems = []

    # ---- 3. marginals -----------------------------------------------------
    labels = Counter()
    unlabeled = 0
    for r in recs:
        p, t = split_prompt_target(r)
        d = decision_of(t)
        if d is None:
            unlabeled += 1
        else:
            labels[d] += 1
    total_lab = sum(labels.values())
    print(f'GUARD: records={len(recs):,} labeled={total_lab:,} unlabeled={unlabeled:,}')
    if total_lab:
        for lab, n in labels.most_common():
            print(f'  marginal {lab:10s} {n:8,d}  {n/total_lab*100:6.2f}%')

    # A guard that extracts nothing cannot certify anything. Without this, a
    # regex that fails to match the corpus's actual target format reports PASS
    # on a corpus whose answer is printed in the prompt -- which is exactly what
    # happened on the first run of this file.
    if total_lab == 0:
        print('\nGUARD: FAIL -- extracted 0 decision labels from '
              f'{len(recs):,} records; refusing to certify an unparsed corpus')
        return 2

    # ---- 1. lexical leakage ----------------------------------------------
    lex = 0
    lex_examples = []
    for r in recs:
        p, t = split_prompt_target(r)
        d = decision_of(t)
        if not d:
            continue
        # Case-insensitive: the prompt states the label in lowercase
        # ("SOURCE ACTION LABEL: reduce") while the target capitalises it.
        if re.search(rf'\b{re.escape(d)}\b', p or '', re.I):
            lex += 1
            if len(lex_examples) < 3:
                m = re.search(rf'\b{re.escape(d)}\b', p, re.I)
                lex_examples.append(p[max(0, m.start() - 60):m.start() + 40].replace('\n', ' '))
    lex_rate = lex / total_lab if total_lab else 0
    print(f'\n[1] LEXICAL leakage: target token present in prompt for '
          f'{lex:,}/{total_lab:,} = {lex_rate*100:.2f}%')
    for ex in lex_examples:
        print(f'      ...{ex}...')
    if lex_rate > 0.02:
        problems.append(f'lexical: {lex_rate*100:.1f}% of targets appear verbatim in the prompt')

    # ---- 2. conditional entropy ------------------------------------------
    print('\n[2] CONDITIONAL structure (label given a coarse prompt state):')
    for feat, pat in STATE_PATTERNS.items():
        buckets = defaultdict(Counter)
        for r in recs:
            p, t = split_prompt_target(r)
            d = decision_of(t)
            if not d:
                continue
            m = pat.search(p or '')
            if not m:
                continue
            key = m.group(1).lower()
            if feat == 'wallet_activity':
                # discretise counts so buckets accumulate
                try:
                    v = int(key)
                except ValueError:
                    continue
                key = '0' if v == 0 else ('1-9' if v < 10 else ('10-99' if v < 100 else '100+'))
            buckets[key][d] += 1
        for key, c in sorted(buckets.items(), key=lambda kv: -sum(kv[1].values())):
            n = sum(c.values())
            if n < args.min_bucket:
                continue
            top, topn = c.most_common(1)[0]
            share = topn / n
            flag = '  <-- DERIVABLE' if share >= args.threshold else ''
            print(f'      {feat}={key:12s} n={n:7,d}  top={top:7s} {share*100:6.2f}%{flag}')
            if share >= args.threshold:
                problems.append(
                    f'{feat}={key}: label "{top}" holds {share*100:.1f}% of {n:,} records')

    verdict = 'FAIL' if problems else 'PASS'
    print(f'\nGUARD: {verdict}')
    for p in problems:
        print(f'  - {p}')

    if args.json_out:
        with open(args.json_out, 'w') as f:
            json.dump({'verdict': verdict, 'records': len(recs),
                       'labeled': total_lab, 'marginals': dict(labels),
                       'lexical_rate': lex_rate, 'problems': problems}, f, indent=2)

    return 1 if problems else 0


if __name__ == '__main__':
    sys.exit(main())
