"""
Escalation channel — how the loop calls the human when it must stop.

Two backends: 'cli' (prints a structured block and, in interactive mode, blocks for input)
and 'telegram' (sends a message; resolution is polled or handled out-of-band). The channel
never auto-resolves a Tier-0 escalation; only a human does.
"""
from __future__ import annotations

import os
import textwrap
from dataclasses import dataclass
from typing import Optional

try:
    import requests
except ImportError:
    requests = None  # type: ignore


@dataclass
class Escalation:
    milestone: str
    task_id: str
    domain: str
    context: str


class EscalationChannel:
    def __init__(self, backend: str = "cli",
                 tg_token_env: str = "SUPERVISOR_TG_TOKEN",
                 tg_chat_env: str = "SUPERVISOR_TG_CHAT"):
        self.backend = backend
        self.tg_token = os.environ.get(tg_token_env, "")
        self.tg_chat = os.environ.get(tg_chat_env, "")

    def raise_escalation(self, e: Escalation) -> None:
        msg = self._format(e)
        if self.backend == "telegram" and self.tg_token and self.tg_chat and requests:
            self._telegram(msg)
        else:
            print(msg, flush=True)

    def _telegram(self, msg: str) -> None:
        try:
            requests.post(
                f"https://api.telegram.org/bot{self.tg_token}/sendMessage",
                json={"chat_id": self.tg_chat, "text": msg[:4000]},
                timeout=15,
            )
        except Exception as ex:  # noqa: BLE001
            print(f"[escalate] telegram failed ({ex}); falling back to stdout:\n{msg}", flush=True)

    @staticmethod
    def _format(e: Escalation) -> str:
        return textwrap.dedent(f"""
        ================= SUPERVISOR ESCALATION =================
        milestone : {e.milestone}
        task/leaf : {e.task_id}
        domain    : {e.domain}
        --------------------------------------------------------
        {e.context}
        --------------------------------------------------------
        The loop has HALTED and needs a human decision.
        (Tier-0 domains never auto-resume.)
        ========================================================
        """).strip()
