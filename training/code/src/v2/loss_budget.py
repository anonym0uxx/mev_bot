#!/usr/bin/env python
"""Stage 5b — real token census + loss budget accounting.

Uses the actual CPT tokenizer so the numbers are delivered token counts, not
char heuristics. Reports supervised mass separately from input (context) mass,
per split and per span.
"""
import argparse, json, sys, os, glob

SPANS = ("system", "user", "assistant")

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--candidate", default="candidate")
    ap.add_argument("--tokenizer", default="/training/runs/cpt-001/cpt_final_bf16_hf")
    ap.add_argument("--context-length", type=int, default=12288)
    a = ap.parse_args()

    from transformers import AutoTokenizer
    tok = AutoTokenizer.from_pretrained(a.tokenizer)
    print(f"[CENSUS] tokenizer={a.tokenizer} vocab={tok.vocab_size}")

    report = {"schema": "north_star_token_census_v2", "tokenizer": a.tokenizer,
              "vocab_size": tok.vocab_size, "context_length": a.context_length, "splits": {}}
    total_sup = 0
    total_ctx = 0
    over_ctx = 0
    for split in ("train", "validation", "test"):
        fp = os.path.join(a.candidate, f"{split}.jsonl")
        if not os.path.exists(fp):
            continue
        n = 0
        sup = 0
        ctx = 0
        span = {k: 0 for k in SPANS}
        maxlen = 0
        act = {}
        with open(fp) as f:
            for line in f:
                if not line.strip():
                    continue
                r = json.loads(line)
                n += 1
                msgs = r["messages"]
                toks = {}
                for m in msgs:
                    t = len(tok(m["content"]).input_ids)
                    toks.setdefault(m["role"], 0)
                    toks[m["role"]] += t
                s = toks.get("system", 0); u = toks.get("user", 0); asst = toks.get("assistant", 0)
                span["system"] += s; span["user"] += u; span["assistant"] += asst
                sup += asst
                ctx += s + u + asst
                L = s + u + asst
                maxlen = max(maxlen, L)
                if L > a.context_length:
                    over_ctx += 1
                act[r["meta"]["action"]] = act.get(r["meta"]["action"], 0) + 1
        report["splits"][split] = {
            "records": n, "supervised_tokens": sup, "context_tokens": ctx,
            "spans": span, "max_seq_len": maxlen,
            "frac_over_context": round(over_ctx / max(n, 1), 6),
            "action_counts": act,
        }
        if split == "train":
            total_sup = sup; total_ctx = ctx
        print(f"[CENSUS] {split:12s} records={n:7,d} supervised={sup:10,d} context={ctx:10,d} maxlen={maxlen}")

    report["train_supervised_tokens"] = total_sup
    report["target_band"] = [15_000_000, 30_000_000]
    report["meets_target_band"] = total_sup >= 15_000_000
    json.dump(report, open("reports/TOKEN_CENSUS_ACTUAL.json", "w"), indent=1)
    print(f"[CENSUS] train supervised = {total_sup:,}  target band {report['target_band']}  "
          f"meets={report['meets_target_band']}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
