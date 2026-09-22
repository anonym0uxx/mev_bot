import json, copy, sys
sys.path.insert(0, "/training/code/qwen27b")
import release_data as rd
from transformers import AutoTokenizer

tok = AutoTokenizer.from_pretrained("/training/runs/cpt-001/cpt_final_bf16_hf")
print("TEMPLATE_KWARGS:", getattr(rd, "TEMPLATE_KWARGS", None))

r = json.loads(open("/training/v2/candidate_sft_c3/train.jsonl").readline())
msgs = r["messages"]
body = msgs[-1]["content"]
KW = getattr(rd, "TEMPLATE_KWARGS", {})

full = tok.apply_chat_template(msgs, tokenize=False, add_generation_prompt=False, **KW)
marker = "QWEN_ASSISTANT_BODY_BOUNDARY_7cc94e"
changed = copy.deepcopy(msgs)
changed[-1]["content"] = marker
marked = tok.apply_chat_template(changed, tokenize=False, add_generation_prompt=False, **KW)
print("marker count:", marked.count(marker))
prefix, suffix = marked.split(marker)
print("PREFIX repr:", repr(prefix))
print("SUFFIX repr:", repr(suffix))
print("full == prefix+body+suffix:", full == prefix + body + suffix)

cand = prefix + body + suffix
n = min(len(full), len(cand))
i = 0
while i < n and full[i] == cand[i]:
    i += 1
print("first divergence:", i, "(len full=%d cand=%d)" % (len(full), len(cand)))
print(" full:", repr(full[max(0, i - 40):i + 60]))
print(" cand:", repr(cand[max(0, i - 40):i + 60]))
