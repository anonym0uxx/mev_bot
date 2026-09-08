"""
narrative_gold_v1 — Schema definitions for 5-layer narrative/social dataset.

DESIGN PRINCIPLE: Quality > Quantity. Broad RAW is fine; GOLD admission is
fail-closed. Uncertain data stays RAW/REJECTED/UNRESOLVED. Authority hierarchy:
  on-chain truth > policy-independent economics > policy critique > creator/tool claims.

OBJECTIVE: maximize executable NET SOL after fees, slippage, latency, failed
fills/losses and capacity. Creator assertion never validates strategy.

CAUSAL/BIAS CONTROLS (BINDING):
  - Availability = first_seen + realistic ingestion latency. No future edits/
    replies/win-rate/later identity leaks.
  - Reliability at t uses ONLY prior resolved claims (track_record_as_of_t).
  - Dedup reposts/copied alpha so amplification != independent breadth.
  - Required controls: contemporaneous unmentioned mints + matched same-regime/
    onchain states (untreated controls, not just treated cases).
  - Track event→alpha/tracker→wallet/onchain→CT propagation to separate early
    edge from crowding.
  - Missing source != zero mentions.
  - Separate ex-ante from ex-post recap/PnL brag/education.

FIVE GOLD LAYERS keyed by stable content_id / claim_id / narrative_state_id:
  1. creator_content_v1     — cleaned/deduped chronological content/events +
                              slang/provenance. RAW preservation.
  2. creator_claim_v1       — entry/exit/risk/narrative/regime claims;
                              fact/opinion/prediction/call; confidence, horizon,
                              sizing/stop/target, entities.
  3. narrative_state_v1     — causal at t: mention velocity, independent creator
                              breadth, platform diversity, amplification, consensus,
                              novelty/age/saturation, alpha/tracker activity,
                              reliability-as-of-t, social↔onchain divergence;
                              account/site reuse, remake/OG-beta competition,
                              bundle/first-buyer patterns, dev/connected/precursor-
                              wallet lineage, copytrade/crowding/shill risk.
  4. narrative_validation_v1 — link claims/states to Slinky/LaserStream outcomes +
                              executable economics; lead/lag, MFE/MAE, net-SOL
                              EV/PnL/capacity/toxic outcomes +
                              SUPPORTED/CONTRADICTED/MIXED/UNRESOLVED.
  5. strategy_card_v1       — setup, trigger, entry, invalidation, exit/stop/target,
                              sizing/capacity, horizon, regime/narrative phase,
                              confirmations, failure modes, sample size, net-SOL EV,
                              MFE/MAE/tail risk, confidence/status. Merge duplicates
                              preserving disagreement.

RAW LAYER (pre-GOLD, not a gold layer):
  raw_social_event_v1       — one-object-per-line JSON from social-ingest adapters.
                              Immutable. Never deleted. All gold layers derive FROM
                              raw via deterministic pipeline. First_seen recorded
                              at ingestion.

SPLITS (BINDING):
  - Chronological + mint/content/thread/call-group-disjoint.
  - Creator/theme/source holdouts if feasible.
  - No leakage across splits. Contamination guards enforced.

VERSIONING:
  - All tables carry: schema_version, pipeline_version, run_uuid, source_hash,
    code_config_hash, git_sha.
  - Immutable manifests with per-file SHA256.
  - Rejected items carry rejection_reason. Completion != GOLD.

PROVENANCE (every row):
  - source: platform (x|telegram|tiktok|web|discord|youtube|tracker)
  - account: handle/ID on that platform
  - account_type: creator|tracker|alpha|kol|amplifier|community|dev|anonymous
  - publish_time_ms: when the content was originally published (EX-ANTE boundary)
  - first_seen_ms: when we first ingested it (availability boundary)
  - ingestion_latency_ms: first_seen - publish_time (realistic, not zero)
  - retrieval_method: api|scrape|mtproto|firecrawl|manual|rss
  - url: canonical link to source
  - source_id: platform-native ID (tweet_id, tg_msg_id, etc.)
  - content_hash: SHA256 of normalized text for dedup
  - raw_payload_ref: path to immutable raw JSON
"""

from __future__ import annotations
from dataclasses import dataclass, field
from typing import Optional
import hashlib

# ─── Provenance ──────────────────────────────────────────────────────

SCHEMA_VERSION = "1.0.0"
GENERATOR_VERSION = "narrative_gold_v1"
PIPELINE_VERSION = "1.0.0"
PRODUCER_VERSION = "social-ingest"

# Claim taxonomy
CLAIM_TYPES = ["entry", "exit", "risk", "narrative", "regime"]
CLAIM_MODALITIES = ["fact", "opinion", "prediction", "call"]
CLAIM_STATUS = ["EX_ANTE", "EX_POST", "AMBIGUOUS", "UNRESOLVED"]
VALIDATION_STATUS = ["SUPPORTED", "CONTRADICTED", "MIXED", "UNRESOLVED"]
ADMISSION_STATUS = ["RAW", "GOLD", "REJECTED", "UNRESOLVED"]

# Usefulness classification (GOLD strongly favors first 6)
USEFULNESS_CLASSES = [
    "HIGH_SIGNAL_REASONING",    # substantive reasoning/thought-process
    "STRATEGY",                 # explicit strategy/approach
    "TRADE_THESIS",             # entry/exit thesis with reasoning
    "NARRATIVE_ANALYSIS",       # narrative/meta interpretation
    "RISK_POSTMORTEM",          # post-mortem analysis of a trade/outcome
    "TOOL_WALLET_SIGNAL",       # tracker/wallet signal (context, not authority)
    "LOW_SIGNAL_CHATTER",       # empty chatter, no substance
    "PROMOTIONAL_SHILL",        # referral farming, shilling
    "DUPLICATE",                # copied/reposted content
    "UNRESOLVED",               # not yet classified
]

# Identity confidence levels
IDENTITY_CONFIDENCE = ["HIGH", "MEDIUM", "LOW", "UNRESOLVED"]
WALLET_CONFIDENCE = ["HIGH", "MEDIUM", "LOW", "NONE"]

# Account types
ACCOUNT_TYPES = [
    "creator", "tracker", "alpha", "kol", "amplifier",
    "community", "dev", "anonymous", "fund", "exchange"
]

# Platforms
PLATFORMS = ["x", "telegram", "tiktok", "web", "discord", "youtube", "tracker"]


def content_id(source: str, source_id: str, content_hash: str) -> str:
    """Deterministic content_id from source+platform_id+content_hash."""
    raw = f"{source}|{source_id}|{content_hash}"
    return hashlib.sha256(raw.encode()).hexdigest()[:16]


def claim_id(content_id: str, claim_type: str, entity: str, seq: int) -> str:
    """Deterministic claim_id."""
    raw = f"{content_id}|{claim_type}|{entity}|{seq}"
    return hashlib.sha256(raw.encode()).hexdigest()[:16]


def narrative_state_id(mint: str, content_id: str, t_ms: int) -> str:
    """Deterministic narrative_state_id — causal state at time t for a mint."""
    raw = f"{mint}|{content_id}|{t_ms}"
    return hashlib.sha256(raw.encode()).hexdigest()[:16]


def strategy_card_id(setup_hash: str, trigger_hash: str) -> str:
    """Deterministic strategy_card_id from setup+trigger fingerprints."""
    raw = f"{setup_hash}|{trigger_hash}"
    return hashlib.sha256(raw.encode()).hexdigest()[:16]


# ═══════════════════════════════════════════════════════════════════════
# RAW LAYER (pre-GOLD, immutable)
# ═══════════════════════════════════════════════════════════════════════

@dataclass
class RawSocialEventV1:
    """Raw event from social-ingest adapters. One-object-per-line JSON.
    Immutable. Never deleted. All gold layers derive from raw."""
    # ─── Provenance ─────────────────────────────────────────────────
    event_id: str                    # SHA256 of raw payload
    schema_version: str
    pipeline_version: str
    run_uuid: str
    git_sha: str

    # ─── Source ─────────────────────────────────────────────────────
    platform: str                    # x|telegram|tiktok|web|discord|youtube|tracker
    account_handle: str              # handle on that platform
    account_id: str                  # platform-native stable ID
    account_type: str                # creator|tracker|alpha|kol|amplifier|...
    source_id: str                   # platform-native content ID (tweet_id, tg_msg_id)
    url: str                         # canonical link

    # ─── Timing (CORRECTED semantics) ───────────────────────────────
    # publish_time_ms:    when the content was originally published (EX-ANTE boundary)
    # retrieval_time_ms:  when our collector fetched it this crawl cycle
    # first_seen_ms:      first time OUR collector ever observed this item
    #                       (for historical backfill, first_seen = retrieval of first crawl;
    #                       for live crawls, first_seen = first observation = causal)
    # ingestion_latency_ms: first_seen - publish_time (realistic, not zero)
    publish_time_ms: int             # when originally published (EX-ANTE boundary)
    first_seen_ms: int               # first time OUR collector observed this item
    retrieval_time_ms: int           # when this specific fetch happened
    ingestion_latency_ms: int        # first_seen - publish_time

    # ─── Content ────────────────────────────────────────────────────
    text: str                        # raw text (preserved exactly)
    text_hash: str                   # SHA256 of normalized text
    engagement_likes: Optional[int]
    engagement_reposts: Optional[int]
    engagement_replies: Optional[int]
    engagement_views: Optional[int]
    is_echo: bool                    # repost/copy flag
    is_edit: bool                    # edited after first publication
    is_delete: bool                  # deleted but captured

    # ─── Extracted entities ────────────────────────────────────────
    cashtags: Optional[str]          # pipe-separated $TICKER list
    contract_addresses: Optional[str] # pipe-separated Solana mint addresses
    mentioned_mints: Optional[str]   # resolved mint addresses (after normalization)
    mentioned_tokens: Optional[str]  # pipe-separated token names/symbols
    urls_in_text: Optional[str]      # pipe-separated URLs found in text

    # ─── QA ────────────────────────────────────────────────────────
    retrieval_method: str            # api|scrape|mtproto|firecrawl|manual|rss
    raw_payload_path: str            # path to immutable raw JSON file
    admission_status: str            # RAW (always for this layer)
    contamination_flags: Optional[str]


# ═══════════════════════════════════════════════════════════════════════
# GOLD LAYER 1: creator_content_v1
# ═══════════════════════════════════════════════════════════════════════

@dataclass
class CreatorContentV1:
    """Cleaned, deduped chronological content/events + slang/provenance.
    Dedup: reposts/copies collapsed (echo flag preserved), original content kept.
    Slang: preserved in raw_text; normalized_text for NLP."""
    # ─── Provenance ─────────────────────────────────────────────────
    content_id: str
    schema_version: str
    pipeline_version: str
    run_uuid: str
    source_hash: str
    code_config_hash: str
    git_sha: str

    # ─── Source ─────────────────────────────────────────────────────
    platform: str
    account_handle: str
    account_id: str
    account_type: str
    source_id: str
    url: str

    # ─── Timing ─────────────────────────────────────────────────────
    publish_time_ms: int
    first_seen_ms: int
    ingestion_latency_ms: int

    # ─── Content ────────────────────────────────────────────────────
    raw_text: str                    # exact original text
    normalized_text: str             # lowercased, slang-preserved, URL-stripped
    text_hash: str                   # dedup key
    language: Optional[str]          # detected language code
    slang_terms: Optional[str]       # pipe-separated detected slang/idioms
    content_type: str                # post|thread|space|video|message|call|chart|tracker_alert

    # ─── Dedup / echo ──────────────────────────────────────────────
    is_original: bool                # True if NOT a repost/copy
    echo_of_content_id: Optional[str] # content_id of original if echo
    echo_chain_length: Optional[int] # 1=original, 2=first repost, etc.
    duplicate_group_id: Optional[str] # groups identical/near-identical content

    # ─── Extracted entities ────────────────────────────────────────
    cashtags: Optional[str]
    contract_addresses: Optional[str]
    mentioned_mints: Optional[str]
    mentioned_tokens: Optional[str]

    # ─── Engagement (as-of first_seen, NOT updated later) ──────────
    engagement_likes: Optional[int]
    engagement_reposts: Optional[int]
    engagement_replies: Optional[int]
    engagement_views: Optional[int]

    # ─── Ex-ante / ex-post classification ──────────────────────────
    temporal_classification: str     # EX_ANTE|EX_POST|AMBIGUOUS
    temporal_reason: Optional[str]   # why classified this way

    # ─── Provenance / slang dictionary ─────────────────────────────
    source_provenance: str           # how we got it (retrieval chain)
    raw_payload_path: str            # back-reference to immutable raw

    # ─── QA / admission ────────────────────────────────────────────
    admission_status: str            # GOLD|REJECTED|UNRESOLVED
    rejection_reason: Optional[str]
    contamination_flags: Optional[str]

    # ─── Quality classification ────────────────────────────────────
    usefulness_class: str            # HIGH_SIGNAL_REASONING|STRATEGY|TRADE_THESIS|...|DUPLICATE
    signal_density: Optional[float]  # substantive content ratio (0.0-1.0)
    originality_score: Optional[float] # 0=copy, 1=fully original
    memecoin_relevance: Optional[float] # 0=irrelevant, 1=directly relevant
    identity_confidence: str         # HIGH|MEDIUM|LOW|UNRESOLVED (canonical ID resolution)
    wallet_confidence: str           # HIGH|MEDIUM|LOW|NONE (wallet-to-identity link)

# ═══════════════════════════════════════════════════════════════════════
# GOLD LAYER 2: creator_claim_v1

@dataclass
class CreatorClaimV1:
    """Extracted claims from creator content. Each content may yield multiple
    claims (entry call + stop loss + target). Claims are the unit of validation."""
    # ─── Provenance ─────────────────────────────────────────────────
    claim_id: str
    schema_version: str
    pipeline_version: str
    run_uuid: str
    git_sha: str

    # ─── Link to content ────────────────────────────────────────────
    content_id: str                  # back-link to creator_content_v1
    platform: str
    account_handle: str
    account_id: str
    account_type: str
    publish_time_ms: int             # EX-ANTE boundary

    # ─── Claim taxonomy ─────────────────────────────────────────────
    claim_type: str                  # entry|exit|risk|narrative|regime
    claim_modality: str              # fact|opinion|prediction|call
    temporal_classification: str     # EX_ANTE|EX_POST|AMBIGUOUS

    # ─── Claim content ──────────────────────────────────────────────
    claim_text: str                  # extracted claim text (snippet)
    entities: str                    # pipe-separated: mint|token|account|venue
    entity_type: str                 # mint|token|account|venue|market|regime
    primary_mint: Optional[str]      # resolved mint address if applicable
    primary_token: Optional[str]     # token symbol/name if applicable

    # ─── Call parameters (if entry/exit call) ──────────────────────
    direction: Optional[str]         # long|short|neutral|watch|avoid
    entry_price_sol: Optional[float] # stated entry price (if any)
    target_price_sol: Optional[float]
    stop_loss_price_sol: Optional[float]
    sizing_sol: Optional[float]      # stated position size
    sizing_pct_bankroll: Optional[float] # stated % of bankroll
    horizon_seconds: Optional[int]   # stated time horizon
    confidence_stated: Optional[float] # stated confidence 0.0-1.0

    # ─── Narrative / regime context ────────────────────────────────
    narrative_theme: Optional[str]   # e.g. "dev-buy", "rug-pull", "graduation"
    regime_label: Optional[str]      # e.g. "risk-on", "risk-off", "rotation"

    # ─── Reliability (as-of publish_time, ONLY prior resolved claims) ──
    creator_track_record_as_of_t: Optional[float]  # win rate from PRIOR claims only
    creator_n_prior_claims: Optional[int]           # count of prior resolved claims
    creator_n_prior_correct: Optional[int]          # count of prior SUPPORTED
    creator_n_prior_contradicted: Optional[int]     # count of prior CONTRADICTED

    # ─── QA / admission ────────────────────────────────────────────
    admission_status: str            # GOLD|REJECTED|UNRESOLVED
    rejection_reason: Optional[str]
    contamination_flags: Optional[str]


# ═══════════════════════════════════════════════════════════════════════
# GOLD LAYER 3: narrative_state_v1
# ═══════════════════════════════════════════════════════════════════════

@dataclass
class NarrativeStateV1:
    """Causal narrative state at time t for a mint. Computed from all content
    with publish_time <= t. No future information. This is the feature layer
    that joins to Slinky/LaserStream on-chain state."""
    # ─── Provenance ─────────────────────────────────────────────────
    narrative_state_id: str
    schema_version: str
    pipeline_version: str
    run_uuid: str
    git_sha: str

    # ─── Identity ───────────────────────────────────────────────────
    mint: str                        # Solana mint address
    state_time_ms: int               # the time t (causal boundary)

    # ─── Mention velocity / acceleration ────────────────────────────
    mention_count_1h: Optional[int]       # mentions in last 1h
    mention_count_6h: Optional[int]
    mention_count_24h: Optional[int]
    mention_velocity_1h: Optional[float]  # mentions/hour in last 1h
    mention_velocity_6h: Optional[float]
    mention_acceleration: Optional[float] # velocity change (1h vs prior 1h)

    # ─── Independent creator breadth ────────────────────────────────
    independent_creators_1h: Optional[int]    # unique non-echo creators
    independent_creators_24h: Optional[int]
    platform_diversity_1h: Optional[int]      # unique platforms in last 1h
    platform_diversity_24h: Optional[int]
    breadth_score: Optional[float]            # weighted diversity metric

    # ─── Amplification / consensus ─────────────────────────────────
    echo_ratio_1h: Optional[float]           # reposts/original in last 1h
    amplification_factor: Optional[float]    # total reach / independent reach
    consensus_direction: Optional[str]       # bullish|bearish|mixed|none
    consensus_strength: Optional[float]      # 0.0-1.0 agreement weighted by reliability

    # ─── Novelty / age / saturation ────────────────────────────────
    first_mention_time_ms: Optional[int]     # first ever mention of this mint
    age_at_t_seconds: Optional[float]        # t - first_mention
    novelty_score: Optional[float]           # how new is this narrative (0=stale, 1=just-emerged)
    saturation_score: Optional[float]        # how saturated (0=fresh, 1=fully crowded)

    # ─── Alpha / tracker activity ──────────────────────────────────
    alpha_call_count_1h: Optional[int]       # entry calls in last 1h
    alpha_call_count_24h: Optional[int]
    tracker_alert_count_1h: Optional[int]    # tracker bot alerts
    tracker_divergence: Optional[float]      # tracker signal vs social consensus

    # ─── Reliability-as-of-t ───────────────────────────────────────
    weighted_reliability_1h: Optional[float] # avg creator reliability (as-of-t)
    weighted_reliability_24h: Optional[float]
    n_high_reliability_creators: Optional[int] # creators with track_record > threshold

    # ─── Social ↔ onchain divergence ───────────────────────────────
    social_onchain_divergence: Optional[float]  # narrative vs price/flow direction
    divergence_direction: Optional[str]          # social-ahead|onchain-ahead|aligned|none
    divergence_magnitude: Optional[float]

    # ─── Wallet / lineage signals (from on-chain, as-of t) ────────
    dev_wallet_active: Optional[bool]
    connected_wallet_count: Optional[int]
    precursor_wallet_lineage: Optional[str]     # pipe-separated wallet IDs with lineage
    first_buyer_count: Optional[int]
    bundle_count: Optional[int]
    first_buyer_overlap_count: Optional[int]    # overlap with other mints' first buyers

    # ─── Competition / remake patterns ─────────────────────────────
    remake_count: Optional[int]               # remake tokens referencing same theme
    og_beta_competition: Optional[float]      # OG vs beta token attention split
    account_reuse_count: Optional[int]        # accounts mentioning multiple related mints

    # ─── Copytrade / crowding / shill risk ─────────────────────────
    copytrade_risk_score: Optional[float]     # 0=no risk, 1=high crowding
    coordinated_shill_score: Optional[float]  # coordinated campaign detection
    crowding_score: Optional[float]           # how late/crowded the entry is

    # ─── Untreated controls (REQUIRED for causal validity) ────────
    control_group_mints: Optional[str]        # pipe-separated matched unmentioned mints
    control_same_regime: Optional[bool]       # matched on same onchain regime
    treatment_differential: Optional[float]   # narrative signal diff vs controls

    # ─── QA / admission ────────────────────────────────────────────
    admission_status: str            # GOLD|REJECTED|UNRESOLVED
    rejection_reason: Optional[str]
    contamination_flags: Optional[str]
    coverage_note: Optional[str]     # "missing_source != zero_mentions" notes


# ═══════════════════════════════════════════════════════════════════════
# GOLD LAYER 4: narrative_validation_v1
# ═══════════════════════════════════════════════════════════════════════

@dataclass
class NarrativeValidationV1:
    """Links claims/states to Slinky/LaserStream on-chain outcomes + executable
    economics. This is the TRUTH layer — on-chain authority > creator claims.
    Lead/lag, MFE/MAE, net-SOL EV/PnL/capacity/toxic outcomes."""
    # ─── Provenance ─────────────────────────────────────────────────
    validation_id: str               # SHA256(claim_id|narrative_state_id|outcome_source)
    schema_version: str
    pipeline_version: str
    run_uuid: str
    git_sha: str

    # ─── Links ─────────────────────────────────────────────────────
    claim_id: Optional[str]          # link to creator_claim_v1 (if validating a claim)
    narrative_state_id: Optional[str] # link to narrative_state_v1 (if validating a state)
    mint: str
    claim_time_ms: int               # when the claim/state was (publish_time or state_time)
    outcome_source: str              # slinky_gold_v3|laserstream_gold_v1|onchain_truth

    # ─── Outcome (from Slinky/LaserStream, matched by mint + time) ─
    slinky_state_id: Optional[str]   # matched Slinky state_id (if any)
    outcome_time_ms: Optional[int]   # outcome measurement time
    lead_lag_seconds: Optional[int]  # outcome_time - claim_time (positive=claim ahead)

    # ─── Price outcomes ────────────────────────────────────────────
    ret_1s_bp: Optional[int]
    ret_5s_bp: Optional[int]
    ret_30s_bp: Optional[int]
    ret_60s_bp: Optional[int]
    ret_300s_bp: Optional[int]
    mfe_bp: Optional[int]
    mae_bp: Optional[int]

    # ─── Executable economics (net SOL after fees/slippage/latency) ─
    net_pnl_sol: Optional[float]     # net PnL for 0.5 SOL entry at claim time
    net_return_bp: Optional[int]
    capacity_sol: Optional[float]    # max executable size without slippage blowout
    execution_feasible: Optional[bool]

    # ─── Toxic outcomes ────────────────────────────────────────────
    is_toxic: Optional[bool]         # BAD/TOXIC economic class
    is_rugpull: Optional[bool]       # rug pattern detected
    is_graduated: Optional[bool]     # graduated to PumpSwap
    survived_60s: Optional[bool]
    survived_300s: Optional[bool]

    # ─── Validation verdict ────────────────────────────────────────
    validation_status: str           # SUPPORTED|CONTRADICTED|MIXED|UNRESOLVED
    validation_reason: str           # executable-evidence-based reasoning
    validation_confidence: Optional[float]  # 0.0-1.0 based on evidence quality

    # ─── Propagation tracking ──────────────────────────────────────
    propagation_stage: Optional[str] # event|alpha|tracker|wallet|onchain|ct|retail
    propagation_lead_seconds: Optional[int] # how far ahead of on-chain confirmation

    # ─── Control comparison ────────────────────────────────────────
    control_outcome_avg_ret_bp: Optional[int]    # avg return of control group mints
    control_outcome_avg_net_pnl: Optional[float] # avg net PnL of controls
    treatment_vs_control_lift: Optional[float]   # treatment - control (executable)

    # ─── QA / admission ────────────────────────────────────────────
    admission_status: str            # GOLD|REJECTED|UNRESOLVED
    rejection_reason: Optional[str]


# ═══════════════════════════════════════════════════════════════════════
# GOLD LAYER 5: strategy_card_v1
# ═══════════════════════════════════════════════════════════════════════

@dataclass
class StrategyCardV1:
    """Synthesized strategy cards from validated claims + narrative states +
    on-chain outcomes. Merges duplicates preserving disagreement. Creator
    assertion never validates strategy — only executable on-chain evidence does."""
    # ─── Provenance ─────────────────────────────────────────────────
    strategy_card_id: str
    schema_version: str
    pipeline_version: str
    run_uuid: str
    git_sha: str

    # ─── Strategy definition ────────────────────────────────────────
    setup: str                       # narrative/onchain setup description
    trigger: str                     # specific entry trigger condition
    entry_rules: str                 # detailed entry criteria
    invalidation_rules: str          # what invalidates the setup
    exit_rules: str                  # take-profit / time-stop / trailing rules
    stop_loss_rules: str             # stop-loss conditions
    sizing_rules: str                # position sizing logic
    capacity_rules: str              # max executable size / slippage constraints

    # ─── Context ────────────────────────────────────────────────────
    regime: Optional[str]            # market regime
    narrative_phase: Optional[str]   # narrative lifecycle phase
    horizon_seconds: Optional[int]   # typical hold time
    confirmations: Optional[str]     # required confirmations (pipe-separated)

    # ─── Failure modes ─────────────────────────────────────────────
    failure_modes: Optional[str]     # known failure scenarios (pipe-separated)

    # ─── Evidence (executable, on-chain) ───────────────────────────
    sample_size: int                 # number of validated instances
    net_sol_ev: Optional[float]      # average net SOL EV per instance
    net_sol_ev_ci_lower: Optional[float]  # confidence interval lower
    net_sol_ev_ci_upper: Optional[float]  # confidence interval upper
    mfe_bp_avg: Optional[int]        # avg MFE
    mae_bp_avg: Optional[int]        # avg MAE
    tail_loss_pct: Optional[float]   # % of instances with >50% loss
    max_drawdown_sol: Optional[float]
    win_rate: Optional[float]        # % profitable instances
    profit_factor: Optional[float]   # gross profit / gross loss

    # ─── Confidence / status ───────────────────────────────────────
    confidence: Optional[float]      # evidence-based confidence 0.0-1.0
    status: str                      # PROVEN|TENTATIVE|DEPRECATED|CONTRADICTED|UNTESTED
    status_reason: Optional[str]

    # ─── Merge provenance ──────────────────────────────────────────
    source_claim_ids: str            # pipe-separated claim_ids merged into this card
    source_narrative_state_ids: Optional[str]  # pipe-separated state IDs
    n_creators_agreeing: Optional[int]         # independent creators supporting
    n_creators_disagreeing: Optional[int]      # independent creators contradicting
    disagreement_notes: Optional[str]          # preserved disagreement details

    # ─── Ablation ──────────────────────────────────────────────────
    onchain_only_ev: Optional[float]  # net SOL EV using onchain signals only
    narrative_lift: Optional[float]   # net SOL EV with narrative - onchain_only
    narrative_lift_significant: Optional[bool]  # statistical significance flag

    # ─── QA / admission ────────────────────────────────────────────
    admission_status: str            # GOLD|REJECTED|UNRESOLVED
    rejection_reason: Optional[str]
