import json, re, collections

P = ("/mnt/data/mev_bot-artifacts/north_star/aggregation/north_star_training_dataset_v1/"
     "corpus_builder_v1/integrated_data_v1/full_history_closure_v1/economics_v1/"
     "astra_review_v1/revision_candidates/decision_sft/decision_sft_train.jsonl")

DEC = re.compile(r"\b(BUY|SELL|EXIT|ADD|REDUCE|HOLD|WATCH|SKIP|NO_POSITION_CHANGE)\b")
LEAK = ("episode_pnl", "realized_lamports", "outcome", "mfe_bp", "mae_bp",
        "hold_seconds", "exit_quality", "return_bp", "h30s_bp", "h5m_bp",
        "action_label", "source_action", "action_basis")

leak = collections.Counter(); lex = 0; n = 0
acts = collections.Counter(); plen = []
first = None
for line in open(P):
    r = json.loads(line); n += 1
    if first is None: first = r
    msgs = r.get("messages") or []
    prompt = "".join(m.get("content", "") for m in msgs if m.get("role") != "assistant")
    ans = "".join(m.get("content", "") for m in msgs if m.get("role") == "assistant")
    plen.append(len(prompt))
    if DEC.search(prompt): lex += 1
    for k in LEAK:
        if k in prompt: leak[k] += 1
    m = re.search(r"\b(?:DECISION|ACTION)\s*[:=]\s*([A-Z_]+)", ans)
    acts[m.group(1) if m else "?"] += 1

print(f"rows={n:,}  prompt_contains_action_token={lex:,} ({lex/n*100:.2f}%)")
print(f"leaked_field_hits={dict(leak)}")
print(f"assistant actions={dict(acts.most_common())}")
print(f"prompt chars mean={sum(plen)/len(plen):.0f} max={max(plen)}")
print("\n--- first record ---")
print("keys:", list(first.keys()))
msgs = first.get("messages") or []
for m in msgs:
    print(f"[{m.get('role')}] {m.get('content','')[:900]}")
    print("-" * 60)
