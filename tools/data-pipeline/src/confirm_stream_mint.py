#!/usr/bin/env python
"""confirm_stream_mint.py — mint-centric on-chain confirmation of stream/video trades.

The corrected architecture (see STAGE2_TWITCH_SPIKE_EVAL.md): don't page a KOL's
bot-wallet history (Millions of bundle-noise txs). Instead:

  1. transcript  -> candidate coin names (context-hugging heuristic)
  2. name        -> mint            (DexScreener search, filtered to Solana + recency)
  3. mint        -> buyers in window (Helius getSignaturesForAddress on the MINT — bounded)
  4. buyers      -> KOL wallet cluster match  => confirmed: "KOL traded <name> at <time>"

Reads HELIUS_API_KEY from ~/.hermes/creds/pump-quant.env (never echoed).

Usage:
  python confirm_stream_mint.py <transcript.txt> <broadcast_unix> <duration_s> \
      --wallet <addr> [--wallet <addr2> ...] [--max-tx 6] [--name <override>...]
"""
import os, sys, json, re, time, urllib.request, urllib.parse, datetime

CREDS = 'C:/Users/Alon/.hermes/creds/pump-quant.env'
RPC = 'https://mainnet.helius-rpc.com'
DEX = 'https://api.dexscreener.com/latest/dex/search'
PUMP = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P"
PUMP_SWAP = "pAMMBay6oceH9fJKBRHGP5D4apD2cMChhx64UHd2wa9"


def load_key():
    d = {}
    with open(CREDS, encoding='utf-8') as f:
        for line in f:
            line = line.strip()
            if line and not line.startswith('#') and '=' in line:
                k, v = line.split('=', 1)
                d[k.strip()] = v.strip()
    return d.get('HELIUS_API_KEY', '')


def helius(method, params, key):
    body = json.dumps({'jsonrpc': '2.0', 'id': 1, 'method': method,
                       'params': params}).encode()
    req = urllib.request.Request(f'{RPC}/?api-key={key}', data=body,
                                 headers={'Content-Type': 'application/json'})
    with urllib.request.urlopen(req, timeout=25) as r:
        return json.loads(r.read().decode())


# ---------------------------------------------------------------- transcript
def load_transcript(path):
    segs = []
    for line in open(path, encoding='utf-8'):
        m = re.match(r"\[([\d.]+)-([\d.]+)\]\s*(.*)", line.strip())
        if m:
            segs.append((float(m.group(1)), float(m.group(2)), m.group(3).strip()))
    return segs


# context-hugging name candidates (candidate generation, not ground truth)
_NAME_PATTERNS = [
    re.compile(r"call(?: it| this| that)? (?:the )?([A-Za-z0-9$][A-Za-z0-9$ .\-]{1,20})", re.I),
    re.compile(r"\b(?:buy|buying|bought|sell|selling|sold|hold|holding|exit|entered?|ape|snipe)[\w ]{0,12}\b([A-Za-z0-9$]{2,18})\b", re.I),
    re.compile(r"([A-Za-z0-9$]{2,18})['\u2019]s (?:wallet|dev|floor|coin|token)", re.I),
    re.compile(r"(?:the|a|that) ([A-Z][a-z]{2,18}) (?:coin|token|narrative|dev|wallet)", re.I),
    re.compile(r"\$([A-Za-z0-9]{2,12})"),
]
_STOP = {"the", "that", "this", "him", "his", "her", "you", "they", "what", "when",
         "where", "which", "there", "here", "about", "going", "gonna", "doing",
         "have", "with", "really", "actually", "another", "other", "someone",
         "their", "them", "bro", "dude", "guys", "fuck", "shit", "like", "just",
         "know", "think", "want", "one", "two", "people", "money", "profit",
         # noise caught in the Sep5 run (filler/function words + non-coin nouns)
         "it", "not", "used", "though", "same", "whole", "and", "tax", "wallet",
         "buyback", "cooler", "god", "fucking", "then", "than", "also", "even",
         "more", "less", "some", "all", "now", "back", "out", "into", "get",
         "got", "but", "so", "if", "or", "was", "were", "been", "being", "did",
         "does", "do", "will", "would", "could", "should", "can", "okay", "yeah",
         "right", "left", "thing", "stuff", "everything", "something", "nothing"}
_NUM = re.compile(r'^\d+$')


def extract_names(segs, broadcast_ts):
    """Return list of (abs_time, name) candidate mentions."""
    out = []
    for start, end, text in segs:
        seen = set()
        for pat in _NAME_PATTERNS:
            for m in pat.finditer(text):
                name = m.group(1).strip().strip('.-').lower()
                name = re.sub(r'\s+', ' ', name)
                if 2 <= len(name) <= 18 and name not in _STOP and not _NUM.match(name) and name not in seen:
                    seen.add(name)
                    out.append((broadcast_ts + start, name))
    # de-dupe by name, keep first occurrence
    dedup = {}
    for t, n in out:
        dedup.setdefault(n, t)
    return [(t, n) for n, t in sorted(dedup.items(), key=lambda x: x[1])]


# ----------------------------------------------------------------- name->mint
def resolve_name(name, broadcast_ts, recency_h=72):
    """DexScreener search -> Solana mints created within recency_h of broadcast.

    Memecoin names collide constantly, so the recency window is the disambiguator:
    keep only tokens whose DEX pair was created near the stream (not the old famous
    coin that shares the name)."""
    url = f'{DEX}?q={urllib.parse.quote(name)}'
    req = urllib.request.Request(url, headers={'User-Agent': 'Mozilla/5.0'})
    with urllib.request.urlopen(req, timeout=20) as r:
        data = json.loads(r.read().decode())
    lo = (broadcast_ts - recency_h * 3600) * 1000
    hi = (broadcast_ts + recency_h * 3600) * 1000
    mints = {}
    for p in data.get('pairs') or []:
        if p.get('chainId') != 'solana':
            continue
        mint = (p.get('baseToken') or {}).get('address', '')
        created = p.get('pairCreatedAt') or 0
        sym = (p.get('baseToken') or {}).get('symbol', '')
        nm = (p.get('baseToken') or {}).get('name', '')
        q = name.lower()
        # name/symbol fuzzy-contains the query AND pair created near the stream
        if (q in nm.lower() or q in sym.lower()) and lo <= created <= hi:
            mints.setdefault(mint, {'symbol': sym, 'name': nm, 'created': created})
    return mints


# ----------------------------------------------------------- mint buyer check
def mint_buyers(mint, start, end, kols, key, max_tx=6):
    """Return list of trades on <mint> in [start,end] whose signer is a KOL wallet."""
    res = helius('getSignaturesForAddress', [mint, {'limit': 1000}], key)
    sigs = [s for s in res.get('result', [])
            if s.get('blockTime') and start <= s['blockTime'] <= end]
    hits = []
    for s in sigs[:max_tx]:
        r = helius('getTransaction', [s['signature'],
                                      {'encoding': 'jsonParsed',
                                       'maxSupportedTransactionVersion': 0}], key)
        tx = r.get('result')
        if not tx:
            continue
        # signers
        signers = set()
        for a in tx['transaction']['message'].get('accountKeys', []):
            if isinstance(a, dict) and a.get('signer'):
                signers.add(a['pubkey'])
            elif isinstance(a, str):
                signers.add(a)
        matched = signers & kols
        if matched:
            hits.append({'sig': s['signature'][:16], 'block_time': s['blockTime'],
                         'signer': list(matched)[0]})
        time.sleep(0.1)
    return hits


def main():
    args = sys.argv[1:]
    path, bts, dur_s = args[0], int(args[1]), int(args[2])
    kols = set()
    names_override = []
    i = 3
    while i < len(args):
        if args[i] == '--wallet':
            kols.add(args[i + 1]); i += 2
        elif args[i] == '--name':
            names_override.append(args[i + 1]); i += 2
        elif args[i] == '--max-tx':
            i += 2
        else:
            i += 1

    key = load_key()
    if not key:
        print('ERROR: HELIUS_API_KEY missing'); return

    end = bts + dur_s
    print(f'stream: {datetime.datetime.utcfromtimestamp(bts).isoformat()}Z +{dur_s/3600:.1f}h '
          f'| KOL wallets: {len(kols)}')

    segs = load_transcript(path)
    names = names_override or [n for _, n in extract_names(segs, bts)]
    print(f'candidate coin names: {len(names)}  (first 20: {", ".join(names[:20])})')

    print('\n=== resolve + confirm ===')
    for name in names[:40]:
        try:
            mints = resolve_name(name, bts)
        except Exception as e:
            print(f'  {name:16s} resolve ERROR {e}')
            continue
        if not mints:
            continue
        for mint, meta in mints.items():
            hits = mint_buyers(mint, bts, end, kols, key)
            flag = 'CONFIRMED' if hits else 'no-kol-match'
            times = ' '.join('@%02dh' % (h['block_time'] % 86400 // 3600) for h in hits)
            print(f'  {name:16s} -> {meta["symbol"][:10]:10s} {mint[:12]}.. '
                  f'{flag} {times}')
        time.sleep(0.4)


if __name__ == '__main__':
    main()