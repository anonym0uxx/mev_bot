#!/usr/bin/env python3
"""Generate decision-parser fixtures from the TRAINED corpus assistant messages.

Defaults to c12 - the corpus built after the KELLY_AUDIT_C12 ruling (MID retired, size
vocabulary {NONE, SMALL, FULL}). Pass a corpus path as argv[3] to override.

Each fixture is one real completion plus the (action, size, price_limit) the corpus itself
records, so the Rust parser is checked against the corpus's own text rather than against
text I wrote.
"""
import json
import re
import sys
import collections

SRC = '/training/v2/candidate_sft_c12/train.jsonl'
OUT = sys.argv[1] if len(sys.argv) > 1 else '/tmp/fixtures_decisions.jsonl'
LIMIT = int(sys.argv[2]) if len(sys.argv) > 2 else 300
if len(sys.argv) > 3:
    SRC = sys.argv[3]


def field(text, key):
    pre = key + ':'
    for line in text.split('\n'):
        line = line.strip()
        if line.startswith(pre):
            return line[len(pre):].strip()
    return None


def main():
    seen = collections.Counter()
    rows = []
    n = 0
    bad = 0
    with open(SRC) as f:
        for line in f:
            r = json.loads(line)
            fam = r.get('family')
            if fam not in ('decision', 'management_replay'):
                continue
            a = next((m['content'] for m in r['messages'] if m.get('role') == 'assistant'), None)
            if a is None:
                continue
            act = (r.get('meta') or {}).get('action')
            if act is None:
                continue
            n += 1
            size = field(a, 'SIZE')
            price = field(a, 'PRICE LIMIT')
            if act == 'BUY':
                # PRICE LIMIT is optional in the corpus: 68% of BUY rows carry none.
                # MID is retired (KELLY_AUDIT_C12): a row carrying it is not a fixture.
                if size not in ('SMALL', 'FULL'):
                    bad += 1
                    continue
                if price is not None:
                    try:
                        float(price)
                    except ValueError:
                        bad += 1
                        continue
                key = ('BUY', size, price is not None)
            else:
                if size is not None and size != 'NONE':
                    bad += 1
                    continue
                size = None
                price = None
                key = (act, None, False)
            if seen[key] < 12:
                seen[key] += 1
                rows.append({'completion': a, 'action': act, 'size': size,
                             'price_limit': float(price) if price else None})
            if len(rows) >= LIMIT:
                break
    with open(OUT, 'w') as fo:
        for r in rows:
            fo.write(json.dumps(r, sort_keys=True) + '\n')
    print(json.dumps({'rows': n, 'written': len(rows), 'skipped_bad': bad,
                      'keys': {str(k): v for k, v in seen.items()}}, indent=1))


if __name__ == '__main__':
    main()
