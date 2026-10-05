import json, sys
sys.path.insert(0, "/training/code/qwen27b")
from transformers import AutoTokenizer

tok = AutoTokenizer.from_pretrained("/training/runs/cpt-001/cpt_final_bf16_hf")
r = json.loads(open("/training/v2/candidate_sft_c3/train.jsonl").readline())
msgs = r["messages"]
print("roles:", [m["role"] for m in msgs])
print("meta.family:", r["meta"].get("family"), "task:", r["meta"].get("task"))

body = msgs[-1]["content"]
print("BODY head repr:", repr(body[:140]))
print("BODY tail repr:", repr(body[-60:]))

prefix = tok.apply_chat_template(msgs[:-1], tokenize=False, add_generation_prompt=True)
full = tok.apply_chat_template(msgs, tokenize=False)
print("PREFIX tail repr:", repr(prefix[-70:]))
suffix = full[len(prefix):] if full.startswith(prefix) else "<prefix not a prefix of full>"
print("SUFFIX repr:", repr(suffix[:140]))
print("full.startswith(prefix):", full.startswith(prefix))
print("body.endswith(suffix):", body.endswith(suffix))
print("full == prefix+body+suffix:", full == prefix + body + suffix)
print("len full/prefix/body/suffix:", len(full), len(prefix), len(body), len(suffix))

# first divergence between full and prefix+body
cand = prefix + body
n = min(len(full), len(cand))
i = 0
while i < n and full[i] == cand[i]:
    i += 1
print("first divergence idx:", i, "of", n)
print(" full:", repr(full[max(0, i-30):i+50]))
print(" cand:", repr(cand[max(0, i-30):i+50]))
