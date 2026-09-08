#!/usr/bin/env python
"""measure_reasoning_yield.py — score a VOD transcript for trading-reasoning density.

Reads a <file>.txt transcript (the format transcribe_vod.py emits) and reports:
  - total segments / words
  - trading-signal segments (those matching a buy/sell/coin/PNL lexicon)
  - reasoning-yield % = signal words / total words
  - a sample of the signal segments (for eyeballing quality)

This is the single number that decides whether the Twitch-transcript lane is worth
putting into the Stage 3+ pipeline: if reasoning-yield is tiny, the lane is banter.

Usage:
  python measure_reasoning_yield.py <file>.txt
"""
import sys, re

# lightweight lexicon — deliberately generous; a real classifier would be tighter
SIGNAL = re.compile(
    r"\b(buy|bought|buying|sell|sold|selling|hold|holding|entry|exit|exit(?:ed|ing)|"
    r"dip|pump|dump|rug|snipe|sniped|coin|token|mcap|market.?cap|price|pnl|chart|"
    r"holder|holders|liquidity|floor|flip|flipped|send|sent|moon|bag|bags|profit|"
    r"loss|lost|made|x[0-9]+|milly|million|\$[0-9]|[0-9]+k\b|sol\b|solana|dev|"
    r"deploy|ca\b|contract|address|wallet|risk|size|signal|alpha|call|raid)\b",
    re.IGNORECASE,
)
TICKER = re.compile(r"\$[A-Za-z0-9]{2,}|[A-Z]{3,}[0-9]*\b")


def main():
    path = sys.argv[1]
    segs = []
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            m = re.match(r"\[([\d.]+)-([\d.]+)\]\s*(.*)", line)
            if not m:
                continue
            segs.append({"start": float(m.group(1)), "end": float(m.group(2)),
                         "text": m.group(3)})

    total_words = sum(len(s["text"].split()) for s in segs)
    signal_segs = [s for s in segs if SIGNAL.search(s["text"])]
    signal_words = sum(len(s["text"].split()) for s in signal_segs)
    ticker_segs = [s for s in segs if TICKER.search(s["text"])]

    dur = max((s["end"] for s in segs), default=0)
    yield_frac = signal_words / total_words if total_words else 0.0

    print(f"segments={len(segs)} words={total_words} audio_span={dur/60:.1f}min")
    print(f"signal_segments={len(signal_segs)} ({len(signal_segs)/len(segs)*100:.0f}% of segments)")
    print(f"signal_words={signal_words} ({yield_frac*100:.1f}% of words)")
    print(f"ticker_mentions={len(ticker_segs)} segments name a ticker")
    print(f"REASONING_YIELD={yield_frac:.3f}")
    print("\n--- sample signal segments ---")
    for s in signal_segs[:25]:
        print(f"[{s['start']/60:.1f}m] {s['text'][:140]}")


if __name__ == "__main__":
    main()