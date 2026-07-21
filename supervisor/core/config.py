"""Typed configuration load/validate for the supervisor."""
from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

try:
    import yaml
except ImportError:
    yaml = None  # type: ignore

from .model_client import ModelConfig
from .design_client import DesignModelConfig
from ..gates.runner import GateConfig


@dataclass
class SupervisorConfig:
    repo_path: str
    constitution_path: str
    evidence_db: str = "supervisor_evidence.db"
    model: ModelConfig = field(default_factory=ModelConfig)
    design_model: DesignModelConfig = field(default_factory=DesignModelConfig)
    gate: GateConfig = field(default_factory=GateConfig)
    task_retry_budget: int = 4
    escalate_channel: str = "cli"          # 'cli' | 'telegram'
    telegram_token_env: str = "SUPERVISOR_TG_TOKEN"
    telegram_chat_id_env: str = "SUPERVISOR_TG_CHAT"
    # ---- production-phase bindings (populated once the build produces these artifacts) ----
    evaluator_bin: str = ""                # path to the built pq-evaluator binary
    evaluator_pinned_sha256: str = ""      # frozen-evaluator hash (§44); verify before trusting grades
    research_runner_bin: str = ""          # path to the bot's sealed-experiment runner
    live_status_file: str = ""             # path to the bot's exported status/metrics JSON

    @classmethod
    def load(cls, path: str | Path, repo_root: str | Path | None = None) -> "SupervisorConfig":
        if yaml is None:
            raise ImportError("pyyaml required: pip install pyyaml")
        cfg_path = Path(path).resolve()
        data: dict[str, Any] = yaml.safe_load(cfg_path.read_text(encoding="utf-8"))
        model = ModelConfig(**data.get("model", {}))
        design = DesignModelConfig(**data.get("design_model", {}))
        gate = GateConfig(**data.get("gate", {}))

        # Path resolution (portability fix): the shipped config uses REPO-RELATIVE paths
        # (e.g. "." and "docs/HERMES_ONE_SHOT_PROMPT.md"). Absolute paths are honored as-is;
        # relative paths resolve against the repo root, which is either passed in, taken from
        # an absolute repo_path in the file, or inferred as the config file's repo (the dir
        # containing docs/ + supervisor/). This makes the same config work at ANY clone location.
        def _infer_repo_root() -> Path:
            if repo_root is not None:
                return Path(repo_root).resolve()
            rp = data.get("repo_path", "")
            if rp and Path(rp).is_absolute():
                return Path(rp)
            # walk up from the config file looking for the repo markers
            for base in [cfg_path.parent, *cfg_path.parents]:
                if (base / "docs").is_dir() or (base / "supervisor").is_dir():
                    return base
            return cfg_path.parent

        root = _infer_repo_root()

        def _resolve(val: str, default: str) -> str:
            v = val or default
            p = Path(v)
            return str(p if p.is_absolute() else (root / v).resolve())

        return cls(
            repo_path=str(root),
            constitution_path=_resolve(data.get("constitution_path", ""),
                                       "docs/HERMES_ONE_SHOT_PROMPT.md"),
            evidence_db=_resolve(data.get("evidence_db", ""), "supervisor_evidence.db"),
            model=model,
            design_model=design,
            gate=gate,
            task_retry_budget=data.get("task_retry_budget", 4),
            escalate_channel=data.get("escalate_channel", "cli"),
            telegram_token_env=data.get("telegram_token_env", "SUPERVISOR_TG_TOKEN"),
            telegram_chat_id_env=data.get("telegram_chat_id_env", "SUPERVISOR_TG_CHAT"),
            evaluator_bin=data.get("evaluator_bin", ""),
            evaluator_pinned_sha256=data.get("evaluator_pinned_sha256", ""),
            research_runner_bin=data.get("research_runner_bin", ""),
            live_status_file=data.get("live_status_file", ""),
        )
