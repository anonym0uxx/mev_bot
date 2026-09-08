#!/usr/bin/env python
"""stage2_social_capture.py — orchestrator: run all free social/narrative lanes.

Runs (1) the Firecrawl narrative acquisition (Telegram/YouTube/Twitch/web/pump.fun/
DexScreener), (2) the Wayback CDX X-archive index, (3) Reddit anonymous .rss pulls.
All lanes are free, public, no login, no ToS evasion.

Deterministic output → suitable as a no_agent cron script.
"""
import subprocess, sys, time, os

REPO = 'D:/repos/mev_bot'
DP = f'{REPO}/tools/data-pipeline'
SRC = f'{DP}/src'
REDDIT = ('C:/Users/Alon/AppData/Local/hermes/skills/social-media/'
          'reddit-reading/scripts/reddit.py')
REDDIT_SUBS = ['solana', 'memecoins', 'pumpfun']


def run(cmd, cwd=None, timeout=1200):
    p = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, timeout=timeout)
    return p.returncode, p.stdout, p.stderr


def main():
    print('=== STAGE2 SOCIAL CAPTURE ===')
    # 1) Firecrawl narrative acquisition
    rc, out, err = run([sys.executable, f'{SRC}/acquire_narrative_web_v1.py', '--sources', 'all'], cwd=DP)
    tail = (out or err).strip().splitlines()
    # print the acquisition report (last ~12 lines)
    print('[narrative] rc=%d' % rc)
    for line in tail[-12:]:
        print('  ' + line)

    # 2) Wayback CDX index (fast, ~30s)
    rc, out, err = run([sys.executable, f'{SRC}/wayback_x_archive.py'], cwd=REPO)
    print('[wayback] rc=%d' % rc)
    for line in (out or err).strip().splitlines():
        print('  ' + line)

    # 3) Reddit anonymous pulls (throttled ~1 req/min)
    for i, sub in enumerate(REDDIT_SUBS):
        rc, out, err = run([sys.executable, REDDIT, 'sub', sub, '--sort', 'new', '--limit', '10'])
        n = len((out or '').strip().splitlines())
        print(f'[reddit r/{sub}] rc=%d lines=%d' % (rc, n))
        if i < len(REDDIT_SUBS) - 1:
            time.sleep(65)  # respect anonymous rate limit

    print('=== DONE ===')


if __name__ == '__main__':
    main()