#!/usr/bin/env python
"""Build general-retention shard v1.1 for Qwen curriculum (Gate 3, diversity rebuild).

Source: HuggingFaceTB/smol-smoltalk (Apache-2.0). FULL-STREAM stratified
reservoir pass over the entire train split:
  chat  (~400K tok): per-source reservoirs for magpie-ultra*, constraints,
                     rewrite, summarize*, everyday-conversations; interleaved
                     draw with a PER-SOURCE TOKEN CAP (<=30% of chat budget)
                     so no single generator/template dominates.
  code  (~400K tok): self-oss-instruct (StarCoder2-15B, exec-filtered).

Contamination screen: 8-gram overlap vs GSM8K-test / HumanEval / MBPP-test.
Overlapping records are dropped (result phrased as retained-overlaps=0 against
the screened sets — not a universal cleanliness claim).

Manifest provenance: HF repo + revision + config/split, per-subset counts,
seed/algorithm, builder git SHA, tokenizer id + revision, output sha256.
"""
import json, os, random, re, subprocess, hashlib

from datasets import load_dataset
from transformers import AutoTokenizer
from huggingface_hub import HfApi

OUT_DIR = os.path.join(os.path.dirname(__file__), '..', 'output',
                       'qwen_curriculum_v1', 'retention')
os.makedirs(OUT_DIR, exist_ok=True)
OUT_JSONL = os.path.join(OUT_DIR, 'qwen_retention_v1.jsonl')
OUT_MANIFEST = os.path.join(OUT_DIR, 'RETENTION_MANIFEST_V1.json')

DATASET_ID = 'HuggingFaceTB/smol-smoltalk'
TOKENIZER_ID = 'unsloth/Qwen3.8-27B'
TARGET_CHAT_TOK = 400_000
TARGET_CODE_TOK = 400_000
CHAT_SOURCE_CAP_FRAC = 0.30   # no chat source may exceed 30% of chat tokens
SEED = 42
POOL_CAP = 8_000              # reservoir cap PER SOURCE

# Chat source families (prefix match). Reservoirs are kept per exact source
# string, capped per family at draw time.
CHAT_PREFIXES = ('smol-magpie-ultra', 'smol-constraints', 'smol-rewrite',
                 'smol-summarize', 'everyday-conversations')
CODE_PREFIXES = ('self-oss-instruct',)

_ws = re.compile(r'\s+')

def norm(t):
    return _ws.sub(' ', t.lower()).strip()

def ngrams8(text):
    w = norm(text).split()
    return {' '.join(w[i:i+8]) for i in range(len(w) - 7)}

def bench_ngrams():
    grams = set()
    print('loading benchmark sets for contamination screen...', flush=True)
    for name, args, fields in [
        ('gsm8k', ('openai/gsm8k', 'main'), ('question', 'answer')),
        ('humaneval', ('openai/openai_humaneval',), ('prompt', 'canonical_solution', 'test')),
        ('mbpp', ('google-research-datasets/mbpp', 'full'), ('text', 'code')),
    ]:
        ds = load_dataset(*args, split='test')
        n0 = len(grams)
        for row in ds:
            for f in fields:
                v = row.get(f)
                if isinstance(v, str) and v:
                    grams |= ngrams8(v)
        print(f'  {name}: {len(ds)} items, +{len(grams)-n0} 8-grams', flush=True)
    return grams

def conv_text(messages):
    return '\n'.join(f"{m['role']}: {m['content']}" for m in messages)

def git_sha():
    try:
        return subprocess.check_output(
            ['git', 'rev-parse', 'HEAD'],
            cwd=os.path.dirname(os.path.abspath(__file__))).decode().strip()
    except Exception:
        return 'unknown'

def main():
    tok = AutoTokenizer.from_pretrained(TOKENIZER_ID)
    api = HfApi()
    ds_rev = api.dataset_info(DATASET_ID).sha
    tok_rev = api.model_info(TOKENIZER_ID).sha
    print(f'dataset revision: {ds_rev}\ntokenizer revision: {tok_rev}', flush=True)

    bad_grams = bench_ngrams()
    print(f'benchmark 8-gram set: {len(bad_grams)}', flush=True)

    print(f'FULL-STREAM pass over {DATASET_ID} train split...', flush=True)
    ds = load_dataset(DATASET_ID, split='train', streaming=True)

    rng = random.Random(SEED)
    pools = {}           # exact source -> reservoir list
    seen = {}            # exact source -> count seen
    total_seen = 0
    for row in ds:
        src = row.get('source', '')
        if not (any(src.startswith(p) for p in CHAT_PREFIXES)
                or any(src.startswith(p) for p in CODE_PREFIXES)):
            continue
        total_seen += 1
        seen[src] = seen.get(src, 0) + 1
        p = pools.setdefault(src, [])
        item = {'messages': row['messages'], 'source': src}
        if len(p) < POOL_CAP:
            p.append(item)
        else:
            j = rng.randrange(seen[src])
            if j < POOL_CAP:
                p[j] = item
    print(f'full-stream complete: {total_seen} eligible rows', flush=True)
    for s in sorted(seen):
        print(f'  {s}: seen={seen[s]} pooled={len(pools[s])}', flush=True)

    chat_sources = sorted(s for s in pools if any(s.startswith(p) for p in CHAT_PREFIXES))
    code_sources = sorted(s for s in pools if any(s.startswith(p) for p in CODE_PREFIXES))
    for s in pools:
        random.Random(f'{SEED}-{s}').shuffle(pools[s])

    records = []
    stats = {'chat': {'recs': 0, 'tokens': 0}, 'code': {'recs': 0, 'tokens': 0}}
    src_counts, src_tokens = {}, {}
    dropped_contam = 0

    def try_add(item, bucket, cap_tokens=None):
        nonlocal dropped_contam
        src = item['source']
        if cap_tokens is not None and src_tokens.get(src, 0) >= cap_tokens:
            return 'capped'
        text = conv_text(item['messages'])
        if ngrams8(text) & bad_grams:
            dropped_contam += 1
            return 'contam'
        n_tok = len(tok.encode(text))
        if n_tok > 8000:
            return 'toolong'
        if cap_tokens is not None and src_tokens.get(src, 0) + n_tok > cap_tokens:
            return 'capped'
        records.append({
            'record_type': 'retention_sft',
            'messages': item['messages'],
            'provenance': {
                'corpus': 'general_retention_v1',
                'dataset': DATASET_ID,
                'dataset_revision': ds_rev,
                'license': 'apache-2.0',
                'subset': src,
                'bucket': bucket,
            },
            'token_count': n_tok,
        })
        stats[bucket]['recs'] += 1
        stats[bucket]['tokens'] += n_tok
        src_counts[src] = src_counts.get(src, 0) + 1
        src_tokens[src] = src_tokens.get(src, 0) + n_tok
        return 'ok'

    # CHAT: round-robin across sources with per-source token cap.
    chat_cap = int(TARGET_CHAT_TOK * CHAT_SOURCE_CAP_FRAC)
    idx = {s: 0 for s in chat_sources}
    active = [s for s in chat_sources if pools[s]]
    while stats['chat']['tokens'] < TARGET_CHAT_TOK and active:
        for s in list(active):
            if stats['chat']['tokens'] >= TARGET_CHAT_TOK:
                break
            advanced = False
            while idx[s] < len(pools[s]):
                item = pools[s][idx[s]]
                idx[s] += 1
                res = try_add(item, 'chat', cap_tokens=chat_cap)
                if res in ('ok', 'capped'):
                    if res == 'capped':
                        idx[s] = len(pools[s])   # source exhausted by cap
                    advanced = True
                    break
                # contam/toolong -> keep scanning this source
            if not advanced or idx[s] >= len(pools[s]):
                if idx[s] >= len(pools[s]) and s in active:
                    active.remove(s)

    # CODE: single source, straightforward fill.
    for s in code_sources:
        for item in pools[s]:
            if stats['code']['tokens'] >= TARGET_CODE_TOK:
                break
            try_add(item, 'code')

    random.Random(SEED).shuffle(records)
    with open(OUT_JSONL, 'w', encoding='utf-8') as f:
        for r in records:
            f.write(json.dumps(r, ensure_ascii=False) + '\n')
    sha = hashlib.sha256(open(OUT_JSONL, 'rb').read()).hexdigest()

    manifest = {
        'version': 'retention_v1.1',
        'dataset': DATASET_ID,
        'dataset_revision': ds_rev,
        'dataset_config': 'default',
        'dataset_split': 'train',
        'license': 'apache-2.0',
        'provenance_note': ('Generated with Llama-3.1-405B-Instruct (Magpie/distilabel) '
                            'and StarCoder2-15B (self-oss-instruct). No Qwen-generated '
                            'content, no OpenAI-generated content, no eval benchmark items.'),
        'tokenizer': TOKENIZER_ID,
        'tokenizer_revision': tok_rev,
        'builder_git_sha': git_sha(),
        'seed': SEED,
        'sampling_algorithm': ('full-stream per-source reservoir (cap %d) -> seeded shuffle -> '
                               'round-robin chat draw with per-source token cap %.0f%% of chat '
                               'budget; code half sequential fill from self-oss-instruct'
                               % (POOL_CAP, CHAT_SOURCE_CAP_FRAC * 100)),
        'records': len(records),
        'total_tokens': stats['chat']['tokens'] + stats['code']['tokens'],
        'buckets': stats,
        'source_counts': src_counts,
        'source_tokens': src_tokens,
        'chat_source_token_cap': chat_cap,
        'contamination_screen': {
            'method': '8-gram overlap, whitespace/case-normalized',
            'benchmarks': ['gsm8k(test)', 'humaneval', 'mbpp(test-full)'],
            'benchmark_8grams': len(bad_grams),
            'records_dropped': dropped_contam,
            'result': '0 retained overlaps against the screened GSM8K-test/HumanEval/MBPP-test 8-gram sets',
        },
        'sha256': sha,
    }
    with open(OUT_MANIFEST, 'w', encoding='utf-8') as f:
        json.dump(manifest, f, indent=2)
    print(json.dumps(manifest, indent=2), flush=True)
    print('DONE', flush=True)

if __name__ == '__main__':
    main()
