"""
`brain_analysis_v1` artifact loader — the supervisor's read side of the engine's
episodic-recall evidence.

The Rust engine writes ONE deterministic JSON object (`brain_analysis.json`, alongside
`live_status.json`) summarising what its episodic memory actually knows: which setup classes
are conditioned and what they paid, which style lens is paying per venue phase, which metas are
decaying, which callers earned trust, and which lanes/archetypes/classes/sources it nominates
for retirement. This module parses that artifact into frozen, typed rows the research loop
(§56) and the strategy-analysis report can consume.

THE ONE RULE THIS MODULE EXISTS TO ENFORCE
------------------------------------------
A row with ``confidence == "unknown"`` is a REFUSAL. The engine declined to answer because the
evidence was too thin (`unknown_reason` says why: `empty_index`, `no_episode_in_scope`,
`no_candidate_in_radius`, `insufficient_sample`). Every estimate field on such a row is `null`.

A refusal is NOT a zero. It is neither evidence of decay nor evidence against it. Therefore:

  * every optional estimate parses to ``None`` — never ``0``, never imputed, never defaulted;
  * the parser REJECTS an artifact whose unknown row carries a non-null estimate, and one whose
    known row is missing an estimate (a half-populated row means the emitter changed and this
    binary's semantics no longer apply);
  * :meth:`BrainAnalysis.known_setup_classes` / :meth:`BrainAnalysis.known_lenses` yield only
    conditioned rows, so a caller cannot accidentally read a refusal as if it were a datum.

Fail-closed everywhere else too: an artifact whose ``schema_version`` is NEWER than this binary
supports is REFUSED (returning ``None`` and logging loudly) rather than reinterpreted under old
field semantics. A MISSING artifact is not an error — the engine may simply not have run yet —
and yields ``None`` quietly, because the brain is an enhancement to the research loop, never a
dependency of it.

Stdlib only; deterministic; Windows-portable (no path assumptions beyond ``pathlib``).
"""
from __future__ import annotations

import json
import logging
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Optional


LOG = logging.getLogger(__name__)

# The artifact's record tag and the HIGHEST schema version this binary understands.
# A newer version is refused, not reinterpreted (see `load_brain_analysis`).
RECORD_TAG: str = "brain_analysis_v1"
SUPPORTED_SCHEMA_VERSION: int = 1

# Lamports per SOL — the artifact speaks in integer lamports; every SOL-denominated number the
# supervisor reports is derived from those integers by this single documented divisor.
LAMPORTS_PER_SOL: int = 1_000_000_000

# The two confidence states an estimate row may occupy. "unknown" is a refusal (§46).
CONFIDENCE_KNOWN: str = "known"
CONFIDENCE_UNKNOWN: str = "unknown"
_CONFIDENCE_VALUES: frozenset[str] = frozenset({CONFIDENCE_KNOWN, CONFIDENCE_UNKNOWN})

# Estimate fields that are null-on-refusal, per row kind. These names drive the refusal
# invariant check; nothing in this module may read one of them without a None check.
_SETUP_ESTIMATES: tuple[str, ...] = (
    "n", "median_net_lamports", "mean_net_lamports", "win_rate_bp", "p25_net_lamports",
    "p75_net_lamports", "median_hold_ns", "nearest_distance",
)
_LENS_ESTIMATES: tuple[str, ...] = ("n", "median_net_lamports", "win_rate_bp")


class BrainAnalysisError(ValueError):
    """The artifact was found but cannot be trusted.

    Raised by :func:`parse_brain_analysis`. `load_brain_analysis` converts it into a loud log
    line plus ``None`` — a malformed brain artifact degrades the loop to its pre-brain
    behaviour, it never crashes the loop and never yields a half-parsed record.
    """


# --------------------------------------------------------------------------- scalar parsing
def _obj(v: Any, where: str) -> dict:
    if not isinstance(v, dict):
        raise BrainAnalysisError(f"{where}: expected a JSON object, got {type(v).__name__}")
    return v


def _arr(o: dict, key: str, where: str) -> list:
    if key not in o:
        raise BrainAnalysisError(f"{where}: missing required array '{key}'")
    v = o[key]
    if not isinstance(v, list):
        raise BrainAnalysisError(f"{where}.{key}: expected a JSON array, got {type(v).__name__}")
    return v


def _req_str(o: dict, key: str, where: str, allowed: Optional[frozenset[str]] = None) -> str:
    v = o.get(key)
    if not isinstance(v, str):
        raise BrainAnalysisError(
            f"{where}.{key}: expected a string, got {type(v).__name__} ({v!r})")
    if allowed is not None and v not in allowed:
        raise BrainAnalysisError(
            f"{where}.{key}: {v!r} is not one of {sorted(allowed)}")
    return v


def _opt_str(o: dict, key: str, where: str) -> Optional[str]:
    """A JSON null (or an absent key) parses to None — never to an empty string."""
    v = o.get(key)
    if v is None:
        return None
    if not isinstance(v, str):
        raise BrainAnalysisError(
            f"{where}.{key}: expected a string or null, got {type(v).__name__}")
    return v


def _req_int(o: dict, key: str, where: str) -> int:
    v = o.get(key)
    # bool is a subclass of int in Python; a JSON true must never become 1 here.
    if isinstance(v, bool) or not isinstance(v, int):
        raise BrainAnalysisError(
            f"{where}.{key}: expected an integer, got {type(v).__name__} ({v!r}). "
            "The artifact is integers-only by construction; a float here means the emitter "
            "changed and this parser's semantics no longer apply.")
    return v


def _opt_int(o: dict, key: str, where: str) -> Optional[int]:
    """The refusal-safe integer read.

    A JSON null parses to ``None``. An ABSENT key also parses to ``None``. There is no code
    path in this function that can produce ``0`` from a null: a zero can only ever come from a
    literal ``0`` in the artifact, which the engine writes only when it measured zero.
    """
    v = o.get(key)
    if v is None:
        return None
    if isinstance(v, bool) or not isinstance(v, int):
        raise BrainAnalysisError(
            f"{where}.{key}: expected an integer or null, got {type(v).__name__} ({v!r})")
    return v


def _check_refusal_invariant(row: Any, fields: tuple[str, ...], where: str) -> None:
    """Enforce the artifact's core safety property on one parsed row.

    unknown  => every estimate is None and an `unknown_reason` is present.
    known    => every estimate is present (a half-populated known row is a schema break).
    """
    if row.confidence == CONFIDENCE_UNKNOWN:
        populated = [f for f in fields if getattr(row, f) is not None]
        if populated:
            raise BrainAnalysisError(
                f"{where}: confidence='unknown' but {populated} carry values. An unknown is a "
                "REFUSAL — it must have no estimate at all. Refusing this artifact rather than "
                "reading a number the engine did not stand behind.")
        if not row.unknown_reason:
            raise BrainAnalysisError(
                f"{where}: confidence='unknown' without an 'unknown_reason'. A refusal must say "
                "why it refused, or the research loop cannot price the missing evidence.")
    else:
        missing = [f for f in fields if getattr(row, f) is None]
        if missing:
            raise BrainAnalysisError(
                f"{where}: confidence='known' but {missing} are null. A conditioned row carries "
                "every estimate; a partial row means the emitter's field set changed.")


# --------------------------------------------------------------------------- row dataclasses
@dataclass(frozen=True)
class SetupClassRow:
    """One conditioned (or refused) setup class from the engine's episodic index."""
    signature: int                      # u128, carried in JSON as a decimal STRING (exact)
    venue_phase: str                    # 'curve' | 'pool'
    meta_category: int
    discovery_lane: str
    confidence: str                     # 'known' | 'unknown'
    unknown_reason: Optional[str]
    n: Optional[int]
    median_net_lamports: Optional[int]
    mean_net_lamports: Optional[int]
    win_rate_bp: Optional[int]
    p25_net_lamports: Optional[int]
    p75_net_lamports: Optional[int]
    median_hold_ns: Optional[int]
    nearest_distance: Optional[int]

    @property
    def is_known(self) -> bool:
        return self.confidence == CONFIDENCE_KNOWN

    @property
    def store_key(self) -> str:
        """Stable row key: what an evidence_ref points at (`brain_setup:<tick>/<key>`)."""
        return f"{self.signature}/{self.venue_phase}"


@dataclass(frozen=True)
class LensRow:
    """One style-lens / venue-phase cell of the scoreboard."""
    lens: str
    venue_phase: str
    confidence: str
    unknown_reason: Optional[str]
    n: Optional[int]
    median_net_lamports: Optional[int]
    win_rate_bp: Optional[int]

    @property
    def is_known(self) -> bool:
        return self.confidence == CONFIDENCE_KNOWN

    @property
    def store_key(self) -> str:
        return f"{self.lens}/{self.venue_phase}"


@dataclass(frozen=True)
class BestPayingLens:
    lens: str
    venue_phase: str
    median_net_lamports: int
    n: int


@dataclass(frozen=True)
class MetaStateRow:
    """Saturation state of one meta category. `phase` may itself be 'unknown' (a refusal)."""
    meta_category: int
    phase: str                          # emerging|hot|saturated|decaying|unknown
    n: int
    participation_decline_bp: int
    outcome_decline_bp: int

    @property
    def is_decaying(self) -> bool:
        return self.phase == "decaying"

    @property
    def store_key(self) -> str:
        return str(self.meta_category)


@dataclass(frozen=True)
class PastMetaMatchRow:
    current_meta: int
    past_meta: int
    distance: int
    past_realized_net_lamports: int
    n: int


@dataclass(frozen=True)
class CallerTrustRow:
    """Earned trust for one caller. `score_bp`/`n_markouts` are null for an unproven refusal."""
    author_id: int
    platform: Optional[str]
    tier: str                           # unproven|watch|trusted|demoted
    score_bp: Optional[int]
    n_markouts: Optional[int]
    exposure: str

    @property
    def store_key(self) -> str:
        return str(self.author_id)


@dataclass(frozen=True)
class FollowRecoRow:
    author_id: int
    platform: str
    n_calls: int
    realized_net_attributed: int
    median_lead_ns: int
    trust_tier: str

    @property
    def store_key(self) -> str:
        return str(self.author_id)


@dataclass(frozen=True)
class UnfollowRow:
    author_id: int
    platform: str
    realized_net_attributed: int
    n_calls: int

    @property
    def store_key(self) -> str:
        return str(self.author_id)


@dataclass(frozen=True)
class SupportInputRow:
    """A datum the engine says it NEEDS but does not have — the ingestion-side ask."""
    kind: str
    platform: Optional[str]
    author_id: Optional[int]
    mint_id: Optional[int]


@dataclass(frozen=True)
class RetirementFlagRow:
    """An engine NOMINATION for retirement. Never a retirement (see analysis/brain_review.py)."""
    subject: str                        # lane|archetype|setup_class|source
    key: str
    reason: str
    n: int
    realized_net_lamports: int

    @property
    def store_key(self) -> str:
        return f"{self.subject}/{self.key}"


@dataclass(frozen=True)
class Refusal:
    """A question the engine declined to answer, and why. The research agenda, in one row."""
    subject: str                        # 'setup_class' | 'lens' | 'meta' | 'caller'
    key: str
    reason: str


@dataclass(frozen=True)
class BrainAnalysis:
    """The whole parsed artifact. Every collection is a tuple: the record is immutable."""
    schema_version: int
    info_time_ns: int
    tick: int
    episodes_total: int
    episodes_admitted: int
    setup_classes: tuple[SetupClassRow, ...]
    lens_scoreboard: tuple[LensRow, ...]
    best_paying_lens: Optional[BestPayingLens]
    meta_state: tuple[MetaStateRow, ...]
    past_meta_matches: tuple[PastMetaMatchRow, ...]
    caller_trust: tuple[CallerTrustRow, ...]
    follow_recommendations: tuple[FollowRecoRow, ...]
    unfollow_candidates: tuple[UnfollowRow, ...]
    support_inputs_needed: tuple[SupportInputRow, ...]
    retirement_flags: tuple[RetirementFlagRow, ...]
    source_path: str = ""

    # ------------------------------------------------------------- refusal-safe accessors
    def known_setup_classes(self) -> tuple[SetupClassRow, ...]:
        """ONLY conditioned rows. Every estimate on every returned row is non-None."""
        return tuple(c for c in self.setup_classes if c.is_known)

    def unknown_setup_classes(self) -> tuple[SetupClassRow, ...]:
        """ONLY refusals. Every estimate on every returned row is None, by construction."""
        return tuple(c for c in self.setup_classes if not c.is_known)

    def known_lenses(self) -> tuple[LensRow, ...]:
        return tuple(l for l in self.lens_scoreboard if l.is_known)

    def unknown_lenses(self) -> tuple[LensRow, ...]:
        return tuple(l for l in self.lens_scoreboard if not l.is_known)

    def decaying_metas(self) -> tuple[MetaStateRow, ...]:
        return tuple(m for m in self.meta_state if m.is_decaying)

    def refusals(self) -> tuple[Refusal, ...]:
        """Everything the engine declined to answer, in deterministic artifact order.

        This is not an error list. It is the thin-evidence frontier: where a cheap experiment
        has the highest value of information, precisely because no estimate exists yet.
        """
        out: list[Refusal] = []
        # `unknown_reason` is guaranteed non-empty on a refusal by _check_refusal_invariant.
        # The fallback below is a type narrowing for Optional[str], NOT a substituted
        # estimate — no numeric field anywhere in this module has a fallback.
        for c in self.unknown_setup_classes():
            out.append(Refusal("setup_class", c.store_key, c.unknown_reason or "unspecified"))
        for l in self.unknown_lenses():
            out.append(Refusal("lens", l.store_key, l.unknown_reason or "unspecified"))
        for m in self.meta_state:
            if m.phase == CONFIDENCE_UNKNOWN:
                out.append(Refusal("meta", m.store_key, "phase_unknown"))
        for t in self.caller_trust:
            if t.score_bp is None or t.n_markouts is None:
                out.append(Refusal("caller", t.store_key, f"tier_{t.tier}_no_markout_estimate"))
        return tuple(out)


# --------------------------------------------------------------------------- row parsers
def _parse_setup_class(o: dict, i: int) -> SetupClassRow:
    where = f"setup_classes[{i}]"
    o = _obj(o, where)
    sig_raw = _req_str(o, "signature", where)
    try:
        signature = int(sig_raw, 10)
    except ValueError as e:
        raise BrainAnalysisError(
            f"{where}.signature: {sig_raw!r} is not a decimal integer string. The u128 setup "
            "signature travels as a string so it is never rounded by a float parser.") from e
    if signature < 0:
        raise BrainAnalysisError(f"{where}.signature: negative ({signature}); u128 expected")
    row = SetupClassRow(
        signature=signature,
        venue_phase=_req_str(o, "venue_phase", where),
        meta_category=_req_int(o, "meta_category", where),
        discovery_lane=_req_str(o, "discovery_lane", where),
        confidence=_req_str(o, "confidence", where, _CONFIDENCE_VALUES),
        unknown_reason=_opt_str(o, "unknown_reason", where),
        n=_opt_int(o, "n", where),
        median_net_lamports=_opt_int(o, "median_net_lamports", where),
        mean_net_lamports=_opt_int(o, "mean_net_lamports", where),
        win_rate_bp=_opt_int(o, "win_rate_bp", where),
        p25_net_lamports=_opt_int(o, "p25_net_lamports", where),
        p75_net_lamports=_opt_int(o, "p75_net_lamports", where),
        median_hold_ns=_opt_int(o, "median_hold_ns", where),
        nearest_distance=_opt_int(o, "nearest_distance", where),
    )
    _check_refusal_invariant(row, _SETUP_ESTIMATES, where)
    return row


def _parse_lens(o: dict, i: int) -> LensRow:
    where = f"lens_scoreboard[{i}]"
    o = _obj(o, where)
    row = LensRow(
        lens=_req_str(o, "lens", where),
        venue_phase=_req_str(o, "venue_phase", where),
        confidence=_req_str(o, "confidence", where, _CONFIDENCE_VALUES),
        unknown_reason=_opt_str(o, "unknown_reason", where),
        n=_opt_int(o, "n", where),
        median_net_lamports=_opt_int(o, "median_net_lamports", where),
        win_rate_bp=_opt_int(o, "win_rate_bp", where),
    )
    _check_refusal_invariant(row, _LENS_ESTIMATES, where)
    return row


def _parse_best_lens(v: Any) -> Optional[BestPayingLens]:
    if v is None:
        return None
    where = "best_paying_lens"
    o = _obj(v, where)
    return BestPayingLens(
        lens=_req_str(o, "lens", where),
        venue_phase=_req_str(o, "venue_phase", where),
        median_net_lamports=_req_int(o, "median_net_lamports", where),
        n=_req_int(o, "n", where),
    )


def _parse_meta_state(o: dict, i: int) -> MetaStateRow:
    where = f"meta_state[{i}]"
    o = _obj(o, where)
    return MetaStateRow(
        meta_category=_req_int(o, "meta_category", where),
        phase=_req_str(o, "phase", where),
        n=_req_int(o, "n", where),
        participation_decline_bp=_req_int(o, "participation_decline_bp", where),
        outcome_decline_bp=_req_int(o, "outcome_decline_bp", where),
    )


def _parse_past_meta(o: dict, i: int) -> PastMetaMatchRow:
    where = f"past_meta_matches[{i}]"
    o = _obj(o, where)
    return PastMetaMatchRow(
        current_meta=_req_int(o, "current_meta", where),
        past_meta=_req_int(o, "past_meta", where),
        distance=_req_int(o, "distance", where),
        past_realized_net_lamports=_req_int(o, "past_realized_net_lamports", where),
        n=_req_int(o, "n", where),
    )


def _parse_caller_trust(o: dict, i: int) -> CallerTrustRow:
    where = f"caller_trust[{i}]"
    o = _obj(o, where)
    return CallerTrustRow(
        author_id=_req_int(o, "author_id", where),
        platform=_opt_str(o, "platform", where),
        tier=_req_str(o, "tier", where),
        score_bp=_opt_int(o, "score_bp", where),
        n_markouts=_opt_int(o, "n_markouts", where),
        exposure=_req_str(o, "exposure", where),
    )


def _parse_follow(o: dict, i: int) -> FollowRecoRow:
    where = f"follow_recommendations[{i}]"
    o = _obj(o, where)
    return FollowRecoRow(
        author_id=_req_int(o, "author_id", where),
        platform=_req_str(o, "platform", where),
        n_calls=_req_int(o, "n_calls", where),
        realized_net_attributed=_req_int(o, "realized_net_attributed", where),
        median_lead_ns=_req_int(o, "median_lead_ns", where),
        trust_tier=_req_str(o, "trust_tier", where),
    )


def _parse_unfollow(o: dict, i: int) -> UnfollowRow:
    where = f"unfollow_candidates[{i}]"
    o = _obj(o, where)
    return UnfollowRow(
        author_id=_req_int(o, "author_id", where),
        platform=_req_str(o, "platform", where),
        realized_net_attributed=_req_int(o, "realized_net_attributed", where),
        n_calls=_req_int(o, "n_calls", where),
    )


def _parse_support_input(o: dict, i: int) -> SupportInputRow:
    where = f"support_inputs_needed[{i}]"
    o = _obj(o, where)
    return SupportInputRow(
        kind=_req_str(o, "kind", where),
        platform=_opt_str(o, "platform", where),
        author_id=_opt_int(o, "author_id", where),
        mint_id=_opt_int(o, "mint_id", where),
    )


def _parse_retirement_flag(o: dict, i: int) -> RetirementFlagRow:
    where = f"retirement_flags[{i}]"
    o = _obj(o, where)
    return RetirementFlagRow(
        subject=_req_str(o, "subject", where),
        key=_req_str(o, "key", where),
        reason=_req_str(o, "reason", where),
        n=_req_int(o, "n", where),
        realized_net_lamports=_req_int(o, "realized_net_lamports", where),
    )


# --------------------------------------------------------------------------- entry points
def parse_brain_analysis(doc: Any, source_path: str = "") -> BrainAnalysis:
    """Parse a decoded `brain_analysis_v1` document. Raises BrainAnalysisError on ANY problem.

    Strict by design: the caller either gets a fully validated record or an exception. There is
    no partially-parsed BrainAnalysis, because a half-read brain is worse than no brain.
    """
    o = _obj(doc, "brain_analysis")
    record = o.get("record")
    if record != RECORD_TAG:
        raise BrainAnalysisError(
            f"record tag is {record!r}, expected {RECORD_TAG!r}. Refusing to parse a document "
            "that does not identify itself as the brain analysis artifact.")
    version = _req_int(o, "schema_version", "brain_analysis")
    if version > SUPPORTED_SCHEMA_VERSION:
        raise BrainAnalysisError(
            f"schema_version {version} is NEWER than the highest this supervisor supports "
            f"({SUPPORTED_SCHEMA_VERSION}). Refusing to reinterpret unknown fields under old "
            "semantics — upgrade the supervisor, or the loop runs without brain grounding.")
    if version < 1:
        raise BrainAnalysisError(f"schema_version {version} is not a valid version")

    return BrainAnalysis(
        schema_version=version,
        info_time_ns=_req_int(o, "info_time_ns", "brain_analysis"),
        tick=_req_int(o, "tick", "brain_analysis"),
        episodes_total=_req_int(o, "episodes_total", "brain_analysis"),
        episodes_admitted=_req_int(o, "episodes_admitted", "brain_analysis"),
        setup_classes=tuple(
            _parse_setup_class(r, i)
            for i, r in enumerate(_arr(o, "setup_classes", "brain_analysis"))),
        lens_scoreboard=tuple(
            _parse_lens(r, i)
            for i, r in enumerate(_arr(o, "lens_scoreboard", "brain_analysis"))),
        best_paying_lens=_parse_best_lens(o.get("best_paying_lens")),
        meta_state=tuple(
            _parse_meta_state(r, i)
            for i, r in enumerate(_arr(o, "meta_state", "brain_analysis"))),
        past_meta_matches=tuple(
            _parse_past_meta(r, i)
            for i, r in enumerate(_arr(o, "past_meta_matches", "brain_analysis"))),
        caller_trust=tuple(
            _parse_caller_trust(r, i)
            for i, r in enumerate(_arr(o, "caller_trust", "brain_analysis"))),
        follow_recommendations=tuple(
            _parse_follow(r, i)
            for i, r in enumerate(_arr(o, "follow_recommendations", "brain_analysis"))),
        unfollow_candidates=tuple(
            _parse_unfollow(r, i)
            for i, r in enumerate(_arr(o, "unfollow_candidates", "brain_analysis"))),
        support_inputs_needed=tuple(
            _parse_support_input(r, i)
            for i, r in enumerate(_arr(o, "support_inputs_needed", "brain_analysis"))),
        retirement_flags=tuple(
            _parse_retirement_flag(r, i)
            for i, r in enumerate(_arr(o, "retirement_flags", "brain_analysis"))),
        source_path=source_path,
    )


def load_brain_analysis(path: str | Path) -> Optional[BrainAnalysis]:
    """Load `brain_analysis.json` from disk. Returns None rather than raising — always.

    Three distinct None cases, all logged at the honest level:

      * file absent      -> DEBUG. Not an error: the engine may not have run yet.
      * unreadable/bad   -> ERROR, loudly. The artifact exists but cannot be trusted.
      * newer schema     -> ERROR, loudly. Fail-closed: refuse, never reinterpret.

    In every None case the research loop must behave exactly as it did before the brain
    existed. That is the contract; `supervisor/tests/test_brain_grounding.py` pins it.
    """
    p = Path(path)
    if not p.is_file():
        LOG.debug("brain analysis artifact absent at %s — research loop runs unaided", p)
        return None
    try:
        raw = p.read_text(encoding="utf-8")
    except OSError as e:
        LOG.error("brain analysis artifact at %s is unreadable: %s — "
                  "running WITHOUT brain grounding", p, e)
        return None
    try:
        doc = json.loads(raw)
    except json.JSONDecodeError as e:
        LOG.error("brain analysis artifact at %s is not valid JSON (%s) — likely a torn or "
                  "truncated write. Running WITHOUT brain grounding rather than guessing.", p, e)
        return None
    try:
        return parse_brain_analysis(doc, source_path=str(p))
    except BrainAnalysisError as e:
        LOG.error("brain analysis artifact at %s REFUSED: %s — running WITHOUT brain "
                  "grounding.", p, e)
        return None
