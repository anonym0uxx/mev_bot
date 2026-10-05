import json, copy, sys, collections
sys.path.insert(0, "/training/code/qwen27b")
from transformers import AutoTokenizer

tok = AutoTokenizer.from_pretrained("/training/runs/cpt-001/cpt_final_bf16_hf")
KW = {"enable_thinking": False}
marker = "QWEN_ASSISTANT_BODY_BOUNDARY_7cc94e"


def renders_verbatim(msgs, body):
    full = tok.apply_chat_template(msgs, tokenize=False, add_generation_prompt=False, **KW)
    changed = copy.deepcopy(msgs)
    changed[-1]["content"] = marker
    marked = tok.apply_chat_template(changed, tokenize=False, add_generation_prompt=False, **KW)
    if marked.count(marker) != 1:
        return False
    pre, suf = marked.split(marker)
    return full == pre + body + suf


res = collections.Counter()
n = 0
for line in open("/training/v2/candidate_sft_c3/train.jsonl"):
    r = json.loads(line)
    m = r["meta"]
    if m.get("family", "decision") == "decision":
        continue
    n += 1
    if n > 400:
        break
    msgs = r["messages"]
    body = msgs[-1]["content"]
    for name, cand in (("raw", body),
                       ("rstrip_nl", body.rstrip("\n")),
                       ("rstrip", body.rstrip())):
        t = copy.deepcopy(msgs)
        t[-1]["content"] = cand
        res[(name, renders_verbatim(t, cand))] += 1

print("records tested:", n)
for k in sorted(res, key=lambda x: (x[0], str(x[1]))):
    print(" ", k, res[k])
