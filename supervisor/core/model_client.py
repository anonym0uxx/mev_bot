"""
Model client for a local llama.cpp server hosting quantized GLM-5.2.

Two response modes:
  * constrained()  -> control turns; enforces a JSON schema via llama.cpp `json_schema`,
                      so malformed control output is mechanically impossible. Returns parsed dict.
  * freeform()     -> code/prose turns; unconstrained, returns raw text.

Robustness: health check, bounded retry with backoff, schema-violation detection
(repeated violations => model/quant problem, surfaced distinctly from code problems),
deterministic sampling for control turns (fixed seed, low temp).
"""
from __future__ import annotations

import json
import time
from dataclasses import dataclass
from typing import Any, Optional

try:
    import requests
except ImportError:  # keep import-time failure actionable
    requests = None  # type: ignore

from .schemas import schema_to_gbnf_via_server_hint


class ModelUnavailable(RuntimeError):
    """Endpoint unreachable, OOM, or unhealthy."""


class SchemaViolation(RuntimeError):
    """Model produced output that does not satisfy the requested control schema."""


@dataclass
class ModelConfig:
    base_url: str = "http://127.0.0.1:8080"
    model: str = "glm-5.2"
    request_timeout_s: float = 300.0
    max_retries: int = 4
    backoff_base_s: float = 2.0
    # control-turn sampling: deterministic and tight
    control_temperature: float = 0.1
    control_seed: int = 1
    # freeform sampling: room to think
    freeform_temperature: float = 0.6
    freeform_max_tokens: int = 8192


class ModelClient:
    def __init__(self, cfg: ModelConfig):
        if requests is None:
            raise ImportError("`requests` is required: pip install requests")
        self.cfg = cfg
        self._session = requests.Session()

    # ------------------------------------------------------------------ health
    def health(self) -> dict[str, Any]:
        """Return endpoint health; raise ModelUnavailable if it cannot serve."""
        try:
            r = self._session.get(f"{self.cfg.base_url}/health", timeout=10)
            if r.status_code == 200:
                return {"ok": True, "detail": r.json() if r.content else {}}
            # llama.cpp returns 503 while the model loads
            raise ModelUnavailable(f"health status {r.status_code}: {r.text[:200]}")
        except Exception as e:  # noqa: BLE001 - surface any transport failure uniformly
            raise ModelUnavailable(str(e)) from e

    # ---------------------------------------------------------------- internals
    def _chat(self, payload: dict) -> dict:
        url = f"{self.cfg.base_url}/v1/chat/completions"
        last_err: Optional[Exception] = None
        for attempt in range(self.cfg.max_retries):
            try:
                r = self._session.post(url, json=payload, timeout=self.cfg.request_timeout_s)
                if r.status_code == 200:
                    return r.json()
                if r.status_code in (500, 503):  # transient / loading / OOM-recover
                    last_err = ModelUnavailable(f"{r.status_code}: {r.text[:200]}")
                else:
                    last_err = RuntimeError(f"{r.status_code}: {r.text[:200]}")
            except Exception as e:  # noqa: BLE001
                last_err = e
            time.sleep(self.cfg.backoff_base_s * (2 ** attempt))
        raise ModelUnavailable(f"chat failed after {self.cfg.max_retries} retries: {last_err}")

    @staticmethod
    def _extract_text(resp: dict) -> str:
        try:
            return resp["choices"][0]["message"]["content"]
        except (KeyError, IndexError) as e:
            raise RuntimeError(f"unexpected response shape: {json.dumps(resp)[:300]}") from e

    # ------------------------------------------------------------- constrained
    def constrained(
        self,
        system: str,
        user: str,
        schema: dict,
        *,
        max_tokens: int = 4096,
        schema_retries: int = 3,
        seed: Optional[int] = None,
        temperature: Optional[float] = None,
    ) -> dict:
        """
        Control turn. Enforces `schema` via llama.cpp json_schema (server converts to GBNF).
        Guarantees a schema-valid dict or raises SchemaViolation after bounded retries.

        `seed` overrides the deterministic control seed for THIS call. Control turns want
        determinism and should leave it None. Best-of-N SAMPLING must pass a distinct seed per
        candidate: with a fixed seed and a fixed prompt the sampler is deterministic, so N
        candidates come back byte-identical and best-of-N degenerates to best-of-1.
        """
        base_payload = {
            "model": self.cfg.model,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user},
            ],
            # MEASURED 2026-07-30: at control_temperature (0.1) the distribution is peaked
            # enough that DIFFERENT seeds return byte-identical text, while the SAME seed can
            # diverge (llama.cpp is not bit-reproducible across requests under continuous
            # batching + cache reuse). Seed is therefore not the diversity lever — temperature
            # is. Best-of-N sampling MUST pass a temperature above the control band or the N
            # candidates collapse onto one.
            "temperature": self.cfg.control_temperature if temperature is None else temperature,
            "seed": self.cfg.control_seed if seed is None else seed,
            "max_tokens": max_tokens,
            # llama.cpp server: constrain decoding to the JSON schema
            "response_format": {
                "type": "json_schema",
                "json_schema": {"name": "control", "schema": schema, "strict": True},
            },
            # belt-and-suspenders: also pass json_schema for servers that read it here
            "json_schema": schema,
        }
        last_text = ""
        for _attempt in range(schema_retries):
            # A retry of a DETERMINISTIC call reproduces the same failure by definition. The
            # first attempt keeps the caller's seed (control turns depend on that); every retry
            # after a failure perturbs it, otherwise schema_retries is decoration.
            payload = base_payload if _attempt == 0 else {
                **base_payload, "seed": int(base_payload["seed"]) + _attempt
            }
            resp = self._chat(payload)
            last_text = self._extract_text(resp).strip()
            try:
                data = json.loads(last_text)
            except json.JSONDecodeError:
                # constrained decoding should prevent this; if it happens the grammar path
                # is not active on this server build -> surface loudly, do not silently pass
                continue
            if self._validate(data, schema):
                return data
        raise SchemaViolation(
            f"model output failed schema after {schema_retries} tries; "
            f"last (truncated): {last_text[:300]}"
        )

    # --------------------------------------------------------------- freeform
    def freeform(self, system: str, user: str, *, max_tokens: Optional[int] = None) -> str:
        payload = {
            "model": self.cfg.model,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user},
            ],
            "temperature": self.cfg.freeform_temperature,
            "max_tokens": max_tokens or self.cfg.freeform_max_tokens,
        }
        return self._extract_text(self._chat(payload)).strip()

    # ------------------------------------------------------ minimal validation
    @staticmethod
    def _validate(data: Any, schema: dict) -> bool:
        """
        Lightweight structural validation (types + required + enum + additionalProperties).
        We keep this dependency-free; jsonschema may be swapped in if present.
        """
        try:
            import jsonschema  # type: ignore
            jsonschema.validate(data, schema)
            return True
        except ImportError:
            return _shallow_validate(data, schema)
        except Exception:
            return False


def _shallow_validate(data: Any, schema: dict) -> bool:
    t = schema.get("type")
    if t == "object":
        if not isinstance(data, dict):
            return False
        for req in schema.get("required", []):
            if req not in data:
                return False
        props = schema.get("properties", {})
        if schema.get("additionalProperties") is False:
            for k in data:
                if k not in props:
                    return False
        for k, v in data.items():
            if k in props and not _shallow_validate(v, props[k]):
                return False
        return True
    if t == "array":
        if not isinstance(data, list):
            return False
        item_s = schema.get("items")
        return all(_shallow_validate(x, item_s) for x in data) if item_s else True
    if t == "string":
        if not isinstance(data, str):
            return False
        if "enum" in schema:
            return data in schema["enum"]
        return True
    if t == "number":
        if not isinstance(data, (int, float)) or isinstance(data, bool):
            return False
        if "minimum" in schema and data < schema["minimum"]:
            return False
        if "maximum" in schema and data > schema["maximum"]:
            return False
        return True
    if t == "integer":
        return isinstance(data, int) and not isinstance(data, bool)
    if t == "boolean":
        return isinstance(data, bool)
    return True  # unconstrained
