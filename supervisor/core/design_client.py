"""
Design-model client — routes authoring briefs to an INDEPENDENT frontier model.

Why this exists (the independence boundary):
    The builder model (GLM, local) writes the code. The dossier's property tests are the
    only independent judge of that code. If the builder authors its own tests, it grades
    itself — the circularity the constitution's anti-agreeability law prohibits.

    So dossier authoring is routed to a DIFFERENT model at a DIFFERENT endpoint with a
    DIFFERENT key. The design model never sees the builder's code and never runs the gates;
    it only reads the constitution and emits a dossier. The gates remain the arbiter, and
    the human still reviews the authored dossier via the recorded escalation.

Configuration (supervisor.yaml):
    design_model:
      enabled: true
      provider: anthropic
      base_url: https://api.anthropic.com
      model: claude-opus-4-8
      api_key_env: ANTHROPIC_API_KEY      # NEVER inline the key
      max_tokens: 16000
      timeout_s: 600

If disabled or the key is absent, dossier authoring escalates to the human exactly as
before — the automation is an accelerant, never a requirement.
"""
from __future__ import annotations

import json
import os
import time
from dataclasses import dataclass
from typing import Any, Optional

try:
    import requests
except ImportError:
    requests = None  # type: ignore


class DesignModelUnavailable(RuntimeError):
    """Design endpoint unreachable, unauthenticated, or disabled."""


@dataclass
class DesignModelConfig:
    enabled: bool = False
    provider: str = "anthropic"
    base_url: str = "https://api.anthropic.com"
    model: str = "claude-opus-4-8"
    api_key_env: str = "ANTHROPIC_API_KEY"
    max_tokens: int = 16000
    timeout_s: float = 600.0
    max_retries: int = 3
    anthropic_version: str = "2023-06-01"

    def api_key(self) -> str:
        return os.environ.get(self.api_key_env, "").strip()

    def usable(self) -> tuple[bool, str]:
        if not self.enabled:
            return False, "design_model.enabled is false"
        if requests is None:
            return False, "`requests` not installed"
        if not self.api_key():
            return False, f"no API key in env {self.api_key_env}"
        return True, "ok"


class DesignModelClient:
    """Minimal, provider-shaped client. Only what dossier authoring needs."""

    def __init__(self, cfg: DesignModelConfig):
        self.cfg = cfg
        ok, why = cfg.usable()
        if not ok:
            raise DesignModelUnavailable(why)
        self._session = requests.Session()

    def complete(self, system: str, user: str) -> str:
        """One turn; returns raw text. Retries transient failures with backoff."""
        url = f"{self.cfg.base_url.rstrip('/')}/v1/messages"
        headers = {
            "x-api-key": self.cfg.api_key(),
            "anthropic-version": self.cfg.anthropic_version,
            "content-type": "application/json",
        }
        body = {
            "model": self.cfg.model,
            "max_tokens": self.cfg.max_tokens,
            "system": system,
            "messages": [{"role": "user", "content": user}],
        }
        last: Optional[Exception] = None
        for attempt in range(self.cfg.max_retries):
            try:
                r = self._session.post(url, headers=headers, json=body,
                                       timeout=self.cfg.timeout_s)
                if r.status_code in (429, 500, 502, 503, 529):
                    raise DesignModelUnavailable(f"transient {r.status_code}")
                if r.status_code == 401:
                    raise DesignModelUnavailable(
                        f"401 unauthorized — check env {self.cfg.api_key_env}")
                r.raise_for_status()
                data: dict[str, Any] = r.json()
                parts = [b.get("text", "") for b in data.get("content", [])
                         if b.get("type") == "text"]
                text = "\n".join(p for p in parts if p).strip()
                if not text:
                    raise DesignModelUnavailable("empty response from design model")
                return text
            except Exception as e:  # noqa: BLE001
                last = e
                if attempt < self.cfg.max_retries - 1:
                    time.sleep(2.0 * (attempt + 1))
        raise DesignModelUnavailable(f"design model failed after retries: {last}")

    def health(self) -> dict[str, Any]:
        """Cheap liveness probe (1-token completion)."""
        try:
            saved = self.cfg.max_tokens
            self.cfg.max_tokens = 16
            txt = self.complete("Reply with the single word: ok", "ping")
            self.cfg.max_tokens = saved
            return {"healthy": True, "model": self.cfg.model, "sample": txt[:40]}
        except Exception as e:  # noqa: BLE001
            return {"healthy": False, "error": str(e)}
