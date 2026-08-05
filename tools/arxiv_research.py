#!/usr/bin/env python3
"""ArXiv research: find papers relevant to memecoin quant trading triggers."""
import sys
import xml.etree.ElementTree as ET
import urllib.request
import urllib.parse
import time

QUERIES = [
    "MEV maximal extractable value Solana",
    "memecoin trading strategy DEX pump",
    "DEX arbitrage blockchain liquidity discovery",
    "social signals cryptocurrency trading prediction",
    "attention sentiment cryptocurrency returns",
    "copy trading blockchain wallet profiling",
    "liquidity pools AMM impermanent loss optimal",
    "token launch bubble speculation attention",
    "order flow toxicity informed trading crypto",
    "wallet clustering sybil detection blockchain",
]

def search(query, max_results=5):
    url = f"https://export.arxiv.org/api/query?search_query={urllib.parse.quote('all:' + query)}&max_results={max_results}&sortBy=relevance"
    try:
        with urllib.request.urlopen(url, timeout=15) as resp:
            data = resp.read()
        root = ET.fromstring(data)
        ns = {'a': 'http://www.w3.org/2005/Atom'}
        results = []
        for entry in root.findall('a:entry', ns):
            title = entry.find('a:title', ns).text.strip().replace('\n', ' ')
            aid = entry.find('a:id', ns).text.split('/abs/')[-1]
            summary = entry.find('a:summary', ns).text.strip().replace('\n', ' ')
            results.append((aid, title, summary))
        return results
    except Exception as e:
        return [("ERROR", str(e), "")]

print("=" * 80)
print("ArXiv Research: Quant Memecoin Trading Triggers")
print("=" * 80)

for q in QUERIES:
    results = search(q)
    print(f"\n### QUERY: {q}")
    for i, (aid, title, summary) in enumerate(results):
        print(f"  {i+1}. [{aid}] {title}")
        print(f"     {summary[:250]}")
    time.sleep(1)  # rate limit

print("\n\n=== DONE ===")
