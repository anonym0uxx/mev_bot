#!/usr/bin/env python3
"""Telegram MTProto → normalized SocialEvent JSON (the `[S]` Telegram adapter).

This is the constitution's **designated primary** machine-friendly capture path
(§29.7): Telegram call-channels sit UPSTREAM of X-KOL amplification, so their
signals carry shorter measured latency (Signal-Horizon Law). It streams public
call channels in real time, captures **edits and deletions as first-class D6
integrity signals** (a deleted losing call is alpha the moment it disappears), and
leaves contract addresses / cashtags in the text for the deterministic core to
extract. Free — MTProto public-channel streaming has no X-style access ban regime.

    pip install telethon pyyaml
    export TELEGRAM_API_ID=...   TELEGRAM_API_HASH=...        # my.telegram.org (free)
    export TELEGRAM_SESSION=...  # a Telethon StringSession (see --login)
    python3 telegram_stream.py | cargo run --quiet --manifest-path probe/Cargo.toml

Discipline: read-only, a DEDICATED research identity — never the operator's
personal account (§29.7e). Each channel is a *source* in the D1-D10 ledger,
PUBLIC_BURNED-presumed; consistently-negative channels are kept as FADE signals.
Captured text is adversarial by definition and only ever feeds capture/research,
never the decision hot path.
"""
import argparse
import os
import sys

import normalize

DEFAULT_CHANNELS = [
    "crypticannouncements",
    "chasescharts",
    "PikalosiCalls",
]


def load_channels(path):
    try:
        import yaml  # optional; falls back to defaults if absent
    except ImportError:
        return DEFAULT_CHANNELS
    try:
        with open(path, encoding="utf-8") as f:
            data = yaml.safe_load(f) or {}
        chans = ((data.get("telegram") or {}).get("channels")) or []
        return [str(c) for c in chans] or DEFAULT_CHANNELS
    except FileNotFoundError:
        return DEFAULT_CHANNELS


def normalize_message(msg, channel: str) -> dict:
    """Telethon Message → normalized event. Views=reach, forwards, replies; a
    forwarded message is an echo (not origination)."""
    text = getattr(msg, "message", None) or getattr(msg, "text", None) or ""
    views = getattr(msg, "views", 0) or 0
    forwards = getattr(msg, "forwards", 0) or 0
    replies_obj = getattr(msg, "replies", None)
    replies = getattr(replies_obj, "replies", 0) if replies_obj else 0
    is_echo = getattr(msg, "fwd_from", None) is not None
    return normalize.build(
        "telegram",
        author=channel,
        text=text,
        community=channel,
        likes=views,      # channel views = reach weight (TG has no "likes")
        reposts=forwards,
        replies=replies,
        echo=is_echo,
    )


def selftest() -> int:
    # Two baked Telethon-shaped messages: a normal call and a forwarded echo.
    class M:  # minimal stand-in for a Telethon Message
        def __init__(self, message, views=0, forwards=0, replies=0, fwd=False):
            self.message = message
            self.views = views
            self.forwards = forwards
            self.replies = type("R", (), {"replies": replies})()
            self.fwd_from = object() if fwd else None

    samples = [
        normalize_message(
            M("NEW CALL $WIF EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v ape", 5000, 40, 12),
            "crypticannouncements",
        ),
        normalize_message(M("forwarded: $WIF sending", 2000, 5, 1, fwd=True), "chasescharts"),
    ]
    return normalize.run_selftest(samples)


def run_live(channels) -> int:
    try:
        from telethon import TelegramClient, events
        from telethon.sessions import StringSession
    except ImportError:
        sys.stderr.write("error: pip install telethon\n")
        return 2

    api_id = os.environ.get("TELEGRAM_API_ID", "").strip()
    api_hash = os.environ.get("TELEGRAM_API_HASH", "").strip()
    session = os.environ.get("TELEGRAM_SESSION", "").strip()
    if not (api_id and api_hash and session):
        sys.stderr.write(
            "error: set TELEGRAM_API_ID / TELEGRAM_API_HASH (my.telegram.org) and "
            "TELEGRAM_SESSION (a StringSession from a dedicated research account)\n"
        )
        return 2

    client = TelegramClient(StringSession(session), int(api_id), api_hash)

    @client.on(events.NewMessage(chats=channels))
    async def on_new(event):  # noqa: ANN001
        chan = getattr(event.chat, "username", None) or str(event.chat_id)
        normalize.write(normalize_message(event.message, chan))

    @client.on(events.MessageEdited(chats=channels))
    async def on_edit(event):  # noqa: ANN001 — D6 integrity signal
        sys.stderr.write(f"[D6 edit] chan={event.chat_id} id={event.message.id}\n")

    @client.on(events.MessageDeleted(chats=channels))
    async def on_delete(event):  # noqa: ANN001 — D6 integrity signal (deleted call)
        sys.stderr.write(f"[D6 delete] chan={event.chat_id} ids={event.deleted_ids}\n")

    sys.stderr.write(f"[telegram] streaming {len(channels)} channels (read-only)\n")
    with client:
        client.run_until_disconnected()
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description="Telegram MTProto → normalized SocialEvent NDJSON")
    ap.add_argument("--sources", default="sources.yaml")
    ap.add_argument("--selftest", action="store_true", help="emit sample events; no network/keys")
    args = ap.parse_args()
    if args.selftest:
        return selftest()
    return run_live(load_channels(args.sources))


if __name__ == "__main__":
    raise SystemExit(main())
