//! `pump_quant_brain` — the **episodic recall memory** for the Hermes memecoin
//! scalping bot.
//!
//! This crate is the answer to the four questions a principal memecoin quant asks
//! before every single trade, answered in microseconds, locally, deterministically:
//!
//! 1. *"What happened last time a coin looked like this?"* → [`recall`]
//! 2. *"Does this match the current meta, or a past one?"* → [`meta_timeline`]
//! 3. *"Did a candle setup like this last week make net SOL?"* → [`recall`],
//!    conditioned on phase / meta / lane
//! 4. *"Who was tweeting about it, and do they actually make money?"* →
//!    [`social_recall`]
//!
//! …and the answers survive a restart ([`persist`]).
//!
//! # The reflection plane: four harder questions
//!
//! Above the per-trade path sit four questions a quant asks *about the market and
//! about himself*, on a slower clock. They are built from the same episodic
//! evidence and obey the same fail-closed contract:
//!
//! 5. *"Does this coin have strong social support, or is it a staged crowd?"* →
//!    [`social_support`] — distinct-originator breadth, trust-weighted, spread
//!    across platforms, differentiated over time, penalised for coordination.
//! 6. *"Can I trust the accounts saying it?"* → [`trust`] — trust earned
//!    **exclusively** from realized net SOL, shrunk toward a population prior,
//!    decayed in information time, demoted when public (constitution 28). Follower
//!    counts and badges are not merely ignored, they are unreachable from the data
//!    that module reads.
//! 7. *"Should I be following someone I am not?"* → [`follow_reco`] — authors whose
//!    calls **preceded** our realized winners, weighted by lead time. Research only:
//!    that module contains no posting or promotional capability and none may be
//!    added (constitution 110).
//! 8. *"Which style does this setup suit, and which style is actually paying us?"* →
//!    [`archetype`] — named, measurable style lenses over the fingerprint, each
//!    validated only against our own realized net SOL.
//!
//! # Design decision: integer feature fingerprints, not LLM/text embeddings
//!
//! The obvious 2020s reflex for "find me similar past situations" is to embed the
//! state as text and do a vector nearest-neighbour search. That is the wrong tool
//! for this job, on five independent grounds — any one of which would be
//! disqualifying on its own.
//!
//! **1. Determinism and replay-safety.** This is a deterministic trading engine:
//! the same inputs must produce byte-identical outputs, forever, on every machine,
//! so that a replay of yesterday's tape reproduces yesterday's decisions exactly.
//! Float embeddings do not offer that. Floating-point reductions reassociate under
//! different SIMD widths and thread counts; a model's own weights drift between
//! versions; a hosted embedding endpoint is not even reproducible with itself. An
//! integer fingerprint over named-const bucket ladders is bit-exact by
//! construction, which is why [`fingerprint`] contains no `f32`/`f64` anywhere —
//! nor does any other module (constitution 22).
//!
//! **2. Latency.** The decision window on a fresh mint is measured in
//! milliseconds, and recall is one of several things that must happen inside it.
//! Stage 1 of [`recall`] is a `xor` plus a `count_ones` per candidate over a
//! contiguous `u128` array — a handful of instructions per episode, no allocation,
//! no branch on data. An embedding lookup is a network round trip, or at best a
//! float dot-product over hundreds of dimensions with a model in memory. We are
//! competing with other bots for the same fill; tens of microseconds is the budget,
//! not tens of milliseconds.
//!
//! **3. Market meaning, and the ability to argue with it.** An embedding's
//! similarity is opaque: when it says two setups are alike, there is no way to ask
//! *why*, and no way to tell it that venue phase matters ten times more than
//! time-of-day. Our distance is a weighted sum over twenty named market features
//! ([`fingerprint::FeatureWeights`]), so "these two setups are 6 apart" decomposes
//! into "the OFI bucket differs by one". A quant can read it, dispute it, and
//! reweight it. Opaque similarity in a system that risks money is not a feature.
//!
//! **4. Zero third-party dependency and zero data exfiltration.** The whole
//! workspace has no external crates and this one keeps it that way: `std` only. No
//! model file, no vector database, no API key, no third party learning which mints
//! we are looking at three seconds before we buy them. In this market, telling a
//! vendor what you are about to trade *is* the trade.
//!
//! **5. Honest small-sample behaviour.** A nearest-neighbour search over
//! embeddings will always return its `k` nearest vectors, and they will always have
//! a cosine similarity, and that number will always look like evidence. Our recall
//! returns [`recall::RecallVerdict::Unknown`] — a variant that *structurally cannot
//! carry an estimate* — whenever the sample is thin or the nearest neighbour is
//! far. Small-n recall is exactly how a quant fools himself; the type system is a
//! cheaper guard than discipline.
//!
//! The cost of this choice is honest: quantization loses information inside a
//! bucket, and a nominal field that we mis-bucket is simply wrong. We accept that.
//! Bucket boundaries are named consts with §-citations precisely so they are
//! reviewable and tunable, and the encoding is chosen so that Hamming distance over
//! the packed signature is *exactly* the unweighted ordinal distance rather than an
//! artefact of bit layout (see [`fingerprint`]).
//!
//! # Purity contract (constitution 22)
//!
//! Every module except [`persist`] is a pure function of its inputs: no wall clock
//! (all times are caller-supplied *information time*), no RNG, no I/O, no floating
//! point, no unordered iteration. [`persist`] is the single, explicit exception,
//! and its I/O is fenced behind the [`persist::BlobStore`] trait so the rest of the
//! crate can be tested without a filesystem.
//!
//! # Bounded state (constitution 57/99)
//!
//! Every store here is a fixed-capacity ring with documented oldest-first eviction:
//! [`recall::EPISODE_CAP`], [`meta_timeline::META_SNAPSHOT_CAP`],
//! [`social_recall::SOCIAL_CALL_CAP`], [`social_recall::SOCIAL_MARKOUT_CAP`].
//! Memory is constant regardless of uptime. Durable history is not lost to
//! eviction: the [`persist`] journal is append-only and keeps everything on disk.
//!
//! # Fail-closed everywhere (constitution 46)
//!
//! Seven separate estimators live here — setup recall, past-meta matching, author
//! track records, social support, source trust, follow recommendation and per-lens
//! style performance — and every one of them refuses to speak below a named minimum
//! sample. They refuse by *type*, not by convention: there is no field to read.
//! `RecallVerdict::Unknown`, `AuthorTrackRecord::Unknown`,
//! `SocialSupportVerdict::Unknown`, `TrustVerdict::Unknown` and
//! `FollowRecoVerdict::Unknown` all carry counts, floors and diagnostics — and no
//! estimate of any kind.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod archetype;
pub mod episode;
pub mod fingerprint;
pub mod follow_reco;
pub mod hash;
pub mod meta_timeline;
pub mod persist;
pub mod recall;
pub mod social_recall;
pub mod social_support;
pub mod trust;
