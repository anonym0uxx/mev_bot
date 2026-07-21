#!/usr/bin/env python3
"""
recall_test.py — Does the model actually READ the constitution, or just the edges?

This is the single most important pre-flight check for the Hermes build. A 372KB
constitution is ~93k tokens. A low-bit quant can hold that in context and still fail to
CONDITION on the middle of it — producing confident, plausible answers that quietly
ignore the law. That failure is invisible in normal use and catastrophic over a
multi-week build.

The test: feed the full constitution as the system prompt, then ask the model to state
specific acceptance criteria verbatim — chosen from the START, MIDDLE, and END of the
document. Compare its answer to ground truth extracted from the file itself.

Usage (with llama-server running on 127.0.0.1:8080):
    python recall_test.py --constitution C:\\hermes\\artifacts\\HERMES_ONE_SHOT_FINAL.md
    python recall_test.py --constitution ... --criteria 89 100 103 109

Defaults are chosen deliberately: long, highly specific criteria (89 = scalp lane
minimal-change rule, 100 = hazard-estimated time-stops, 103 = microsecond latency law,
109 = Rust performance law) spread across the middle and end of the document. Short
one-line criteria make a WEAK test because a capable model can guess them from context.
    python recall_test.py --constitution ... --url http://127.0.0.1:8080

Grading is keyword-overlap based and deliberately crude — it flags obvious failure, but
YOU should read the side-by-side output. A model that paraphrases correctly passes; a
model that invents a plausible-sounding criterion fails, and that is exactly what this
is looking for.
"""
from __future__ import annotations

import argparse
import json
import re
import sys
import time
import urllib.request

STOPWORDS = {
    "the", "a", "an", "and", "or", "of", "to", "in", "is", "are", "be", "by", "for",
    "with", "that", "this", "it", "as", "on", "at", "from", "no", "not", "any", "all",
    "every", "must", "can", "may", "never", "always", "its", "their", "which", "than",
}


def extract_criteria(text: str) -> dict[int, str]:
    """Pull individual acceptance criteria out of the §63 region.

    Criteria are written inline ('47. text... 48. text...'), so each criterion runs from
    its own number to the next number that starts a criterion.
    """
    idx = text.find("ACCEPTANCE CRITERIA")
    region = text[idx:] if idx != -1 else text
    # bound at the next top-level section header
    m = re.search(r"^\s*\d{1,2}\.\s+[A-Z][A-Z0-9 \-/&,'\.]{4,}\s*$",
                  region[len("ACCEPTANCE CRITERIA"):], re.MULTILINE)
    if m:
        region = region[:len("ACCEPTANCE CRITERIA") + m.start()]

    hits = [(int(mm.group(1)), mm.start(), mm.end())
            for mm in re.finditer(r"(?:^|[ \n])(\d{1,3})\.\s", region)
            if 1 <= int(mm.group(1)) <= 999]
    out: dict[int, str] = {}
    for i, (num, _s, e) in enumerate(hits):
        end = hits[i + 1][1] if i + 1 < len(hits) else len(region)
        body = region[e:end].strip()
        if len(body) > 40:                 # ignore stray numeric matches
            out.setdefault(num, body)
    return out


def keywords(s: str, n: int = 40) -> set[str]:
    words = re.findall(r"[a-zA-Z_][a-zA-Z0-9_\-]{3,}", s.lower())
    return {w for w in words if w not in STOPWORDS}


def overlap_score(truth: str, answer: str) -> float:
    t, a = keywords(truth), keywords(answer)
    if not t:
        return 0.0
    return round(100.0 * len(t & a) / len(t), 1)


def ask(url: str, system: str, user: str, timeout: int = 1800,
        max_tokens: int = 1400) -> tuple[str, float]:
    payload = {
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
        "temperature": 1.0,
        "top_p": 0.95,
        "min_p": 0.01,
        "max_tokens": max_tokens,
        "stream": False,
    }
    req = urllib.request.Request(
        f"{url.rstrip('/')}/v1/chat/completions",
        data=json.dumps(payload).encode("utf-8"),
        headers={"Content-Type": "application/json"},
    )
    t0 = time.time()
    with urllib.request.urlopen(req, timeout=timeout) as r:
        data = json.loads(r.read().decode("utf-8"))
    elapsed = time.time() - t0
    msg = data["choices"][0]["message"]
    content = msg.get("content") or ""
    if not content and msg.get("reasoning_content"):
        content = "[only reasoning returned]\n" + msg["reasoning_content"]
    return content.strip(), elapsed


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--constitution", required=True)
    ap.add_argument("--url", default="http://127.0.0.1:8080")
    ap.add_argument("--criteria", nargs="*", type=int, default=[89, 100, 103, 109])
    ap.add_argument("--pass-threshold", type=float, default=45.0,
                    help="keyword-overlap %% below which a criterion is flagged")
    args = ap.parse_args()

    text = open(args.constitution, encoding="utf-8", errors="replace").read()
    crits = extract_criteria(text)
    approx_tokens = len(text) // 4
    print(f"constitution: {len(text):,} chars (~{approx_tokens:,} tokens), "
          f"{len(crits)} criteria parsed\n")

    missing = [c for c in args.criteria if c not in crits]
    if missing:
        print(f"WARNING: criteria not found in file: {missing}")
        args.criteria = [c for c in args.criteria if c in crits]
        if not args.criteria:
            return 1

    system = text
    results = []
    for num in args.criteria:
        truth = crits[num]
        q = (f"State acceptance criterion {num} from the constitution in the system prompt. "
             f"Quote it as exactly as you can. If you cannot locate it, say "
             f"'NOT FOUND' — do not guess or reconstruct it.")
        print(f"=== asking for criterion {num} ===")
        try:
            answer, secs = ask(args.url, system, q)
        except Exception as e:  # noqa: BLE001
            print(f"  REQUEST FAILED: {e}")
            return 2
        score = overlap_score(truth, answer)
        claimed_missing = "NOT FOUND" in answer.upper()
        verdict = ("HONEST-MISS" if claimed_missing
                   else "PASS" if score >= args.pass_threshold else "FAIL")
        results.append((num, score, verdict, secs))
        print(f"  {verdict}  overlap={score}%  ({secs:.1f}s)")
        print(f"  --- ground truth (first 320 chars) ---\n  {truth[:320]}")
        print(f"  --- model answer (first 320 chars) ---\n  {answer[:320]}\n")

    print("=" * 70)
    print(f"{'criterion':>10} {'overlap':>9} {'verdict':>12} {'seconds':>9}")
    for num, score, verdict, secs in results:
        print(f"{num:>10} {score:>8}% {verdict:>12} {secs:>8.1f}")

    fails = [r for r in results if r[2] == "FAIL"]
    misses = [r for r in results if r[2] == "HONEST-MISS"]
    print("\nINTERPRETATION:")
    if not fails and not misses:
        print("  Model conditions on the full document. Quant is adequate — proceed.")
    elif misses and not fails:
        print("  Model admits it cannot find some criteria. Honest, but recall is")
        print("  incomplete: consider a higher-fidelity quant or a smaller constitution.")
    else:
        print("  FAILURE MODE DETECTED: the model produced confident text that does not")
        print("  match the actual criterion. This is the dangerous case — it will build")
        print("  against a constitution it only appears to have read.")
        print("  Recommended: switch to DeepSeek-V4-Flash UD-Q8_K_XL (162GB, lossless),")
        print("  or GLM-5.2 at a higher quant, before starting M0.")
    return 0 if not fails else 3


if __name__ == "__main__":
    sys.exit(main())
