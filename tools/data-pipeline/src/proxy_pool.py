#!/usr/bin/env python
"""
proxy_pool.py - Proxy rotation pool for Solana RPC requests.

Loads HTTP and SOCKS5 proxies from files, tests them against Solana RPC,
and provides a rotating proxy interface for rate-limited requests.

Usage:
    from proxy_pool import ProxyRPC
    rpc = ProxyRPC()
    result = rpc.post("getAccountInfo", [mint, {...}])
"""

import json
import os
import time
import random
import urllib.request
import socket
from concurrent.futures import ThreadPoolExecutor, as_completed

# Additional free Solana RPC endpoints
FREE_RPCS = [
    "https://api.mainnet.solana.com",
    "https://rpc.ankr.com/solana",
    "https://solana-rpc.publicnode.com",
]

PROXY_FILES = {
    'http': "D:/repos/mev_bot/tools/data-pipeline/output/proxies_http.txt",
    'socks5': "D:/repos/mev_bot/tools/data-pipeline/output/proxies_socks5.txt",
    'working': "D:/repos/mev_bot/tools/data-pipeline/output/proxies_working.txt",
}

class ProxyRPC:
    """Rotating proxy + multi-RPC Solana client.
    
    Strategy:
    1. Try direct RPC (fastest, rate-limited)
    2. Try via working proxies
    3. Try via random proxy from pool
    4. Rotate through all options
    """
    
    def __init__(self, max_proxies=50):
        self.rpcs = list(FREE_RPCS)
        self.working_proxies = self._load_proxies('working')
        self.http_proxies = self._load_proxies('http')[:max_proxies]
        self.socks5_proxies = self._load_proxies('socks5')[:max_proxies]
        
        # Track rate-limit state per RPC
        self.rpc_cooldowns = {}  # rpc_url -> cooldown_until_ts
        self.proxy_cooldowns = {}  # proxy_addr -> cooldown_until_ts
        
        # Stats
        self.stats = {
            'direct_success': 0, 'direct_fail': 0,
            'proxy_success': 0, 'proxy_fail': 0,
            'total_requests': 0,
        }
        
        print(f"ProxyRPC initialized: {len(self.rpcs)} RPCs, "
              f"{len(self.working_proxies)} working proxies, "
              f"{len(self.http_proxies)} HTTP, {len(self.socks5_proxies)} SOCKS5")
    
    def _load_proxies(self, kind):
        path = PROXY_FILES.get(kind)
        if not path or not os.path.exists(path):
            return []
        with open(path, 'r') as f:
            return [l.strip() for l in f if l.strip()]
    
    def _is_cooled_down(self, key, cooldowns):
        if key in cooldowns:
            if time.time() < cooldowns[key]:
                return False  # still cooling down
            del cooldowns[key]
        return True
    
    def _mark_rate_limited(self, key, cooldowns, cooldown_seconds=30):
        cooldowns[key] = time.time() + cooldown_seconds
    
    def post(self, method, params, timeout=20):
        """Make a JSON-RPC POST with proxy rotation.
        Returns (result, source_info)."""
        self.stats['total_requests'] += 1
        
        # Try direct RPC first (fastest)
        for rpc in self.rpcs:
            if not self._is_cooled_down(rpc, self.rpc_cooldowns):
                continue
            result = self._try_direct(rpc, method, params, timeout)
            if result is not None:
                self.stats['direct_success'] += 1
                return result, {'rpc': rpc, 'via': 'direct'}
            else:
                self._mark_rate_limited(rpc, self.rpc_cooldowns, 30)
                self.stats['direct_fail'] += 1
        
        # Try via working proxies
        for proxy in self.working_proxies:
            if not self._is_cooled_down(proxy, self.proxy_cooldowns):
                continue
            # Pick a random RPC to use through the proxy
            rpc = random.choice(self.rpcs)
            result = self._try_proxy(proxy, rpc, method, params, timeout)
            if result is not None:
                self.stats['proxy_success'] += 1
                return result, {'rpc': rpc, 'via': 'proxy', 'proxy': proxy}
            else:
                self._mark_rate_limited(proxy, self.proxy_cooldowns, 60)
                self.stats['proxy_fail'] += 1
        
        # Try random HTTP proxies from pool
        for _ in range(5):  # try 5 random proxies
            if not self.http_proxies:
                break
            proxy = random.choice(self.http_proxies)
            if not self._is_cooled_down(proxy, self.proxy_cooldowns):
                continue
            rpc = random.choice(self.rpcs)
            result = self._try_proxy(proxy, rpc, method, params, timeout=10)
            if result is not None:
                self.stats['proxy_success'] += 1
                # Add to working proxies
                if proxy not in self.working_proxies:
                    self.working_proxies.append(proxy)
                return result, {'rpc': rpc, 'via': 'proxy_pool', 'proxy': proxy}
            else:
                self._mark_rate_limited(proxy, self.proxy_cooldowns, 120)
                self.stats['proxy_fail'] += 1
        
        return None, {'error': 'all methods failed'}
    
    def _try_direct(self, rpc, method, params, timeout):
        payload = json.dumps({
            "jsonrpc": "2.0", "id": 1,
            "method": method, "params": params
        }).encode()
        try:
            req = urllib.request.Request(rpc, data=payload,
                headers={'Content-Type': 'application/json'}, method='POST')
            with urllib.request.urlopen(req, timeout=timeout) as resp:
                result = json.loads(resp.read().decode())
            if result.get('result') is not None:
                return result
            if '429' in str(result.get('error', '')):
                return None
            return result  # return even errors that aren't rate limits
        except Exception as e:
            if '429' in str(e):
                return None
            return None
    
    def _try_proxy(self, proxy_addr, rpc, method, params, timeout):
        payload = json.dumps({
            "jsonrpc": "2.0", "id": 1,
            "method": method, "params": params
        }).encode()
        try:
            proxy_handler = urllib.request.ProxyHandler({
                'http': f'http://{proxy_addr}',
                'https': f'http://{proxy_addr}'
            })
            opener = urllib.request.build_opener(proxy_handler)
            req = urllib.request.Request(rpc, data=payload,
                headers={'Content-Type': 'application/json'}, method='POST')
            with opener.open(req, timeout=timeout) as resp:
                result = json.loads(resp.read().decode())
            if result.get('result') is not None:
                return result
            return None
        except:
            return None
    
    def get_stats(self):
        return self.stats


def test_proxies_parallel(proxies_file, rpc_url, max_test=100, workers=10):
    """Test proxies in parallel to find working ones quickly."""
    if not os.path.exists(proxies_file):
        return []
    with open(proxies_file, 'r') as f:
        all_proxies = [l.strip() for l in f if l.strip()]
    
    print(f"Testing {min(len(all_proxies), max_test)} proxies (parallel, {workers} workers)...")
    
    test_payload = json.dumps({
        "jsonrpc": "2.0", "id": 1,
        "method": "getHealth", "params": []
    }).encode()
    
    def test_one(proxy_addr):
        try:
            proxy_handler = urllib.request.ProxyHandler({
                'http': f'http://{proxy_addr}',
                'https': f'http://{proxy_addr}'
            })
            opener = urllib.request.build_opener(proxy_handler)
            req = urllib.request.Request(rpc_url, data=test_payload,
                headers={'Content-Type': 'application/json'}, method='POST')
            with opener.open(req, timeout=8) as resp:
                result = json.loads(resp.read().decode())
            if result.get('result') == 'ok':
                return proxy_addr
        except:
            pass
        return None
    
    working = []
    with ThreadPoolExecutor(max_workers=workers) as executor:
        futures = {executor.submit(test_one, p): p for p in all_proxies[:max_test]}
        for future in as_completed(futures):
            result = future.result()
            if result:
                working.append(result)
    
    print(f"  Working: {len(working)}/{min(len(all_proxies), max_test)}")
    return working


if __name__ == '__main__':
    # Test the proxy pool
    print("Testing proxy pool against Solana RPC...\n")
    
    # Test more proxies in parallel
    working = test_proxies_parallel(
        PROXY_FILES['http'],
        "https://api.mainnet.solana.com",
        max_test=200,
        workers=20
    )
    
    if working:
        with open(PROXY_FILES['working'], 'a') as f:
            for p in working:
                f.write(p + '\n')
        print(f"  Saved {len(working)} working proxies")
    
    # Test the ProxyRPC class
    print("\n=== Testing ProxyRPC ===")
    rpc = ProxyRPC()
    
    # Make a test request
    result, source = rpc.post("getHealth", [])
    if result:
        print(f"  getHealth: {result.get('result')} via {source}")
    
    # Make a real request
    test_mint = "72iZDQ4JtkiCDkxH1eLC3kbYdFkufqutgcUUeSpGpump"
    result, source = rpc.post("getAccountInfo", [test_mint, {"encoding": "base64"}])
    if result and result.get('result', {}).get('value'):
        print(f"  getAccountInfo: ✓ exists via {source}")
    
    print(f"\n  Stats: {rpc.get_stats()}")
