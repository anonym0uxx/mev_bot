#!/usr/bin/env python
"""stage2_social_capture.py — orchestrator: run all free social/narrative lanes.

Runs (1) the Firecrawl narrative acquisition (Telegram/YouTube/Twitch/web/pump.fun/
DexScreener), (2) the Wayback CDX X-archive index, (3) Reddit anonymous .rss pulls.
All lanes are free, public, no login, no ToS evasion.

Deterministic output -> suitable as a no_agent cron script.

Reddit anonymous feed is rate-limited to ~1 req/min per IP and bursts return 429.
So we pull ONE subreddit per run (rotating by hour) and self-cooldown 30 min after
a 429, so we never hard-block the shared IP.
"""
import subprocess, sys, time, os, json

REPO = 'D:/repos/mev_bot'
DP = f'{REPO}/tools/data-pipeline'
SRC = f'{DP}/src'
REDDIT = ('C:/Users/Alon/AppData/Local/hermes/skills/social-media/'
          'reddit-reading/scripts/reddit.py')
REDDIT_SUBS = ['solana', 'memecoins', 'pumpfun']
REDDIT_COOLDOWN = f'{DP}/output/narrative_gold_v1/reddit_cooldown.json'
REDDIT_COOLDOWN_SEC = 1800  # 30 min after a 429


def run(cmd, cwd=None, timeout=1200):
    p = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, timeout=timeout)
    return p.returncode, p.stdout, p.stderr


def _last_429():
    try:
        with open(REDDIT_COOLDOWN) as f:
            return float(json.load(f).get('last_429_ts', 0.0))
    except (OSError, ValueError, json.JSONDecodeError):
        return 0.0


def _mark_429():
    try:
        os.makedirs(os.path.dirname(REDDIT_COOLDOWN), exist_ok=True)
        with open(REDDIT_COOLDOWN, 'w') as f:
            json.dump({'last_429_ts': time.time()}, f)
    except OSError:
        pass


def main():
    print('=== STAGE2 SOCIAL CAPTURE ===')
    # 1) Firecrawl narrative acquisition
    rc, out, err = run([sys.executable, f'{SRC}/acquire_narrative_web_v1.py', '--sources', 'all'], cwd=DP)
    tail = (out or err).strip().splitlines()
    print('[narrative] rc=%d' % rc)
    for line in tail[-12:]:
        print('  ' + line)

    # 2) Wayback CDX index (fast, ~30s)
    rc, out, err = run([sys.executable, f'{SRC}/wayback_x_archive.py'], cwd=REPO)
    print('[wayback] rc=%d' % rc)
    for line in (out or err).strip().splitlines():
        print('  ' + line)

    # 3) Reddit anonymous pull — ONE sub per run (rotating by hour), cooldown on 429
    if time.time() - _last_429() < REDDIT_COOLDOWN_SEC:
        print('[reddit] skipped (cooldown active after 429)')
    else:
        sub = REDDIT_SUBS[int(time.time() // 3600) % len(REDDIT_SUBS)]
        rc, out, err = run([sys.executable, REDDIT, 'sub', sub, '--sort', 'new', '--limit', '10'])
        n = len((out or '').strip().splitlines())
        print(f'[reddit r/{sub}] rc=%d lines=%d' % (rc, n))
        if rc == 2:
            _mark_429()

    print('=== DONE ===')


if __name__ == '__main__':
    main()