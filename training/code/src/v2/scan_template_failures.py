import json, copy, collections, sys
sys.path.insert(0, "/training/code/qwen27b")
from transformers import AutoTokenizer

tok = AutoTokenizer.from_pretrained("/training/runs/cpt-001/cpt_final_bf16_hf")
KW = {"enable_thinking": False}
CTRL = ("<|im_start|>", "<|im_end|>", "<think>", "</think>")
marker = "QWEN_ASSISTANT_BODY_BOUNDARY_7cc94e"

stats = collections.Counter()
examples = {}
n = 0
for line in open("/training/v2/candidate_sft_c3/train.jsonl"):
    n += 1
    if n > 40000:
        break
    r = json.loads(line)
    m = r["meta"]
    fam = m.get("family", "decision")
    msgs = r["messages"]
    body = msgs[-1]["content"]
    reason = None
    if not body or not body.strip():
        reason = "empty_body"
    elif any(s in body for s in CTRL):
        reason = "control_token_in_body"
    else:
        full = tok.apply_chat_template(msgs, tokenize=False, add_generation_prompt=False, **KW)
        changed = copy.deepcopy(msgs)
        changed[-1]["content"] = marker
        marked = tok.apply_chat_template(changed, tokenize=False, add_generation_prompt=False, **KW)
        if marked.count(marker) != 1:
            reason = "ambiguous_boundary"
        else:
            pre, suf = marked.split(marker)
            if full != pre + body + suf:
                reason = "transformed_body"
    stats[(fam, reason or "OK")] += 1
    if reason and reason not in examples:
        examples[reason] = (fam, m.get("candidate_id"), repr(body[:100]), repr(body[-60:]))

print("scanned:", n)
for k, v in sorted(stats.items()):
    print(" ", k, v)
print("--- examples ---")
for k, v in examples.items():
    print(" ", k, "->", v)
