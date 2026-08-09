#!/usr/bin/env python
"""Search arXiv for papers relevant to dev wallet tracking / memecoin monitoring."""
import sys, urllib.request, urllib.parse, xml.etree.ElementTree as ET, json, time

NS = {'a': 'http://www.w3.org/2005/Atom'}

def search(query, max_results=10):
    url = f"https://export.arxiv.org/api/query?search_query=all:{urllib.parse.quote(query)}&max_results={max_results}&sortBy=relevance"
    try:
        req = urllib.request.Request(url, headers={'User-Agent': 'Mozilla/5.0'})
        data = urllib.request.urlopen(req, timeout=30).read()
        root = ET.fromstring(data)
        results = []
        for entry in root.findall('a:entry', NS):
            title = entry.find('a:title', NS).text.strip().replace('\n', ' ')
            aid = entry.find('a:id', NS).text.strip().split('/abs/')[-1]
            published = entry.find('a:published', NS).text[:10]
            authors = ', '.join(a.find('a:name', NS).text for a in entry.findall('a:author', NS))
            summary = entry.find('a:summary', NS).text.strip().replace('\n', ' ')
            cats = ', '.join(c.get('term') for c in entry.findall('a:category', NS))
            results.append({
                'id': aid, 'title': title, 'published': published,
                'authors': authors[:200], 'abstract': summary[:500],
                'categories': cats, 'pdf': f"https://arxiv.org/pdf/{aid}"
            })
        return results
    except Exception as e:
        return [{'error': str(e)}]

queries = [
    "memecoin rug pull detection blockchain",
    "Solana wallet clustering on-chain forensics",
    "smart money identification cryptocurrency trading",
    "wallet reputation trust score blockchain",
    "pump fun token launch pump.fun",
    "deceptive token deployment cryptocurrency",
    "wallet graph analysis cryptocurrency fraud",
    "copy trading whale wallet following",
    "creator deployer credibility blockchain",
    "Sybil attack wallet identity linking",
]

all_results = {}
for q in queries:
    print(f"### {q}")
    results = search(q, 8)
    all_results[q] = results
    for r in results:
        if 'error' in r:
            print(f"  ERROR: {r['error']}")
        else:
            print(f"  [{r['id']}] {r['title'][:120]}")
            print(f"    {r['published']} | {r['categories']}")
            print(f"    {r['abstract'][:250]}")
            print()
    time.sleep(3)  # arXiv rate limit ~1 req/3s

# Save full JSON
with open('D:/repos/mev_bot/tools/arxiv_results.json', 'w') as f:
    json.dump(all_results, f, indent=2)
print(f"\nSaved {sum(len(v) for v in all_results.values())} results to arxiv_results.json")
