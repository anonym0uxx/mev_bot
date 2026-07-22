//! Social ingestion wiring: normalized [`SocialEvent`]s → engine inputs.
//!
//! This is the seam that turns captured social attention into the two things the
//! nervous system consumes: corroboration-tier [`AppEvent::SocialCall`]s for the
//! discovery lane, and narrative [`Mention`]s for the attention-velocity layer.
//! Capture itself (twitterapi.io / Telegram / TikTok / Firecrawl) is `[S]` server
//! I/O behind [`SocialSource`]; everything here is pure and deterministic (§22).
//!
//! # Discipline (binding)
//! * **Corroboration-tier only (§29, §71).** A social event only ever produces a
//!   `SocialCall`, which *raises rank* at the gate but can never authorise capital
//!   alone — on-chain confirmation is still required. This module cannot emit a
//!   self-authorizing event by construction.
//! * **Earned quality, never assumed.** `source_quality_bp` is supplied by the
//!   caller from the D1–D10 `SocialSourceQualityLedger` (or a single conservative
//!   config baseline until a source has earned evidence — PUBLIC_BURNED
//!   presumption). This module hard-codes no per-platform trust (§29.8, §102).
//! * **Provenance preserved (§29).** The narrative `Mention` keeps the event's
//!   measured instant, distinct author (origination unit), community, and echo
//!   flag, so distinct-originator breadth and copy-echo are computed downstream —
//!   reach is never mistaken for alpha.

use crate::event::AppEvent;
use pump_quant_domain::ids::Mint;
use pump_quant_ingest::social_parse::SocialEvent;
use pump_quant_ingest::social_source::{parse_batch, RawSocialPayload, SocialSource};
use pump_quant_narrative::attention_state::Mention;
use pump_quant_social::classification::Classification;
use pump_quant_social::ledger::SourceQualityLedger;
use pump_quant_social::types::SourceState;

/// Map a social event to a narrative [`Mention`] (attention-layer input).
///
/// Weight is the event's engagement, floored at 1 so every observed post counts
/// as at least one unit of attention; `copycat` carries the echo flag so the
/// attention state can discount reach-without-origination.
#[must_use]
pub fn to_mention(ev: &SocialEvent) -> Mention {
    Mention {
        ts_ns: ev.observed_at_ns,
        source_id: ev.author_id,
        community_id: ev.community_id,
        weight: ev.engagement.max(1),
        copycat: ev.is_echo,
    }
}

/// Emit one corroboration-tier [`AppEvent::SocialCall`] per concrete market named
/// in the event, each tagged with the caller-resolved `source_quality_bp`.
///
/// Cashtag-only events (no on-chain address) intentionally produce **no**
/// `SocialCall` — with no mint there is nothing to corroborate on-chain; they
/// still feed the attention layer via [`to_mention`], symbol-clustered. Returns an
/// iterator so callers can extend an event buffer without an intermediate `Vec`.
pub fn to_social_calls<'a>(
    ev: &'a SocialEvent,
    source_quality_bp: u32,
) -> impl Iterator<Item = AppEvent> + 'a {
    ev.mints().iter().map(move |m| AppEvent::SocialCall {
        mint: Mint::from_bytes(*m),
        source_quality_bp,
    })
}

/// The result of ingesting one capture batch: lane calls + attention mentions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IngestBatch {
    /// Corroboration-tier social calls, one per (event × named mint).
    pub calls: Vec<AppEvent>,
    /// Narrative mentions, one per event (targeted or not).
    pub mentions: Vec<Mention>,
}

/// Pull the next batch from a [`SocialSource`], decode it, and fan it out into
/// engine inputs. `quality` resolves each event's source quality (bps) — wire it
/// to the `SocialSourceQualityLedger`; a constant closure gives the config
/// baseline. Pure given the source's output (the only I/O is inside `next_batch`,
/// the `[S]` seam), so a recorded batch stream replays byte-for-byte (§54).
pub fn ingest_next<S, Q>(source: &mut S, quality: Q) -> IngestBatch
where
    S: SocialSource,
    Q: Fn(&SocialEvent) -> u32,
{
    let batch = source.next_batch();
    ingest_payloads(&batch, quality)
}

/// Decode + fan out an already-captured batch (the pure core of [`ingest_next`]).
#[must_use]
pub fn ingest_payloads<Q>(batch: &[RawSocialPayload], quality: Q) -> IngestBatch
where
    Q: Fn(&SocialEvent) -> u32,
{
    let events = parse_batch(batch);
    let mut out = IngestBatch::default();
    for ev in &events {
        out.mentions.push(to_mention(ev));
        let q = quality(ev);
        out.calls.extend(to_social_calls(ev, q));
    }
    out
}

/// Operator-set corroboration ceilings (bps) per earned source-classification state.
///
/// Named, not magic (§102): the corroboration quality a source contributes is one
/// of these ceilings scaled by the ledger's confidence, and every fade state
/// resolves to `0`. Operators set these inside their risk envelope; nothing here is
/// a per-account hardcode — trust is earned from D1–D10 or presumed burned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceQualityPolicy {
    /// Ceiling for a source that beat the D3 control from a pre-flow posture.
    pub pre_flow_alpha_bp: u32,
    /// Ceiling for a with-flow amplifier (rides moves, does not originate).
    pub flow_amplifier_bp: u32,
    /// Ceiling for an authentic organic node without proven pre-flow edge.
    pub organic_bp: u32,
    /// PUBLIC_BURNED floor for an unseen / insufficient-sample source.
    pub baseline_bp: u32,
}

impl SourceQualityPolicy {
    /// A conservative fade-first default (PUBLIC_BURNED presumption): unseen
    /// sources corroborate weakly, amplifiers modestly, only proven pre-flow alpha
    /// strongly. Fade states contribute nothing regardless of this policy.
    #[must_use]
    pub const fn conservative() -> Self {
        Self {
            pre_flow_alpha_bp: 9_000,
            flow_amplifier_bp: 4_000,
            organic_bp: 3_000,
            baseline_bp: 1_500,
        }
    }
}

/// Corroboration quality (bps) for an earned classification: positive states scale
/// their policy ceiling by confidence; `InsufficientSample` is the baseline floor;
/// every fade state (late-exit, paid-shill, engagement-farm, copy-echo) is `0`.
#[must_use]
fn quality_from_classification(c: &Classification, policy: &SourceQualityPolicy) -> u32 {
    let scale = |ceiling: u32| -> u32 {
        ((u64::from(ceiling) * u64::from(c.confidence_bps)) / 10_000) as u32
    };
    match c.state {
        SourceState::PreFlowAlpha => scale(policy.pre_flow_alpha_bp),
        SourceState::FlowAmplifier => scale(policy.flow_amplifier_bp),
        SourceState::OrganicCommunityNode => scale(policy.organic_bp),
        SourceState::InsufficientSample => policy.baseline_bp,
        // Fade states never corroborate — they can only *reduce* conviction
        // elsewhere; here they add zero rank (fade-first, §29).
        SourceState::LateExitLiquidityPromoter
        | SourceState::PaidShillSuspect
        | SourceState::EngagementFarm
        | SourceState::CopyEchoAccount => 0,
    }
}

/// Resolve a source's corroboration quality (bps) from the earned D1–D10
/// [`SourceQualityLedger`]. A source the ledger has never reconciled resolves to
/// the policy baseline (PUBLIC_BURNED presumption) — never to trust. This is the
/// compliant `quality` resolver for [`ingest_payloads`]: quality is earned, never a
/// per-platform or per-account hardcode (§29.8, §102).
#[must_use]
pub fn ledger_quality(
    ledger: &SourceQualityLedger,
    source_id: u64,
    policy: &SourceQualityPolicy,
) -> u32 {
    match ledger.get(source_id) {
        Some(c) => quality_from_classification(&c, policy),
        None => policy.baseline_bp,
    }
}

/// Detect cross-source content coordination: a `content_hash` posted by two or more
/// **distinct authors** within `window_ns` is a coordinated-campaign signal (feeds
/// COORDINATED_SPAM / CREATOR_FUNDED_PUSH, §29.7c). This is exact-hash matching — a
/// fast, deterministic lower bound on the semantic copy-echo the social crate
/// computes from shingles — so identical reposts across nominally-unrelated channels
/// surface immediately. Returns `(content_hash, distinct_author_count)` for each
/// coordinated cluster, sorted by hash (deterministic, §22). Bounded by input (§99).
#[must_use]
pub fn coordinated_content(events: &[SocialEvent], window_ns: u64) -> Vec<(u64, u32)> {
    use std::collections::BTreeMap;
    // content_hash -> (distinct authors, min ts, max ts)
    let mut groups: BTreeMap<u64, (Vec<u64>, u64, u64)> = BTreeMap::new();
    for ev in events {
        let e = groups.entry(ev.content_hash).or_insert((
            Vec::new(),
            ev.observed_at_ns,
            ev.observed_at_ns,
        ));
        if !e.0.contains(&ev.author_id) {
            e.0.push(ev.author_id);
        }
        e.1 = e.1.min(ev.observed_at_ns);
        e.2 = e.2.max(ev.observed_at_ns);
    }
    groups
        .into_iter()
        .filter(|(_, (authors, lo, hi))| authors.len() >= 2 && hi.saturating_sub(*lo) <= window_ns)
        .map(|(hash, (authors, _, _))| (hash, authors.len() as u32))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pump_quant_ingest::social_parse::{parse_social_event, SocialPlatform};
    use pump_quant_ingest::social_source::MockSocialSource;

    const USDC: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";

    fn raw(platform: &str, text: &str, likes: u64, echo: bool, at: u64) -> RawSocialPayload {
        let json = format!(
            r#"{{"platform":"{platform}","author":"kol","community":"chan","text":"{text}","likes":{likes},"echo":{echo}}}"#
        )
        .into_bytes();
        RawSocialPayload::new(json, at)
    }

    #[test]
    fn mention_carries_provenance_and_echo() {
        let ev = parse_social_event(raw("x", "$WIF", 9, true, 500).json.as_slice(), 500).unwrap();
        let m = to_mention(&ev);
        assert_eq!(m.ts_ns, 500);
        assert_eq!(m.weight, 9);
        assert!(m.copycat, "echo preserved");
        assert_eq!(m.source_id, ev.author_id);
    }

    #[test]
    fn cashtag_only_event_makes_no_call_but_a_mention() {
        let out = ingest_payloads(&[raw("telegram", "$PEPE runs", 3, false, 1)], |_| 4000);
        assert_eq!(
            out.calls.len(),
            0,
            "no CA -> nothing to corroborate on-chain"
        );
        assert_eq!(out.mentions.len(), 1);
    }

    #[test]
    fn mint_event_makes_a_corroboration_call() {
        let text = format!("aping {USDC} $USDC");
        let out = ingest_payloads(&[raw("x", &text, 50, false, 7)], |_| 6000);
        assert_eq!(out.calls.len(), 1);
        match out.calls[0] {
            AppEvent::SocialCall {
                mint,
                source_quality_bp,
            } => {
                assert_eq!(source_quality_bp, 6000);
                assert_eq!(
                    mint,
                    Mint::from_bytes(pump_quant_ingest::base58::decode_pubkey(USDC).unwrap())
                );
            }
            _ => panic!("expected SocialCall"),
        }
        assert_eq!(out.mentions.len(), 1);
    }

    #[test]
    fn quality_resolver_can_vary_by_platform() {
        // A resolver keyed on provenance (as the ledger will be): X earns more here.
        let q = |ev: &SocialEvent| match ev.platform {
            SocialPlatform::X => 7000,
            _ => 3000,
        };
        let out = ingest_payloads(&[raw("x", USDC, 1, false, 1)], q);
        match out.calls[0] {
            AppEvent::SocialCall {
                source_quality_bp, ..
            } => assert_eq!(source_quality_bp, 7000),
            _ => panic!(),
        }
    }

    #[test]
    fn ingest_next_drives_a_source_then_empties() {
        let mut src = MockSocialSource::new().with_batch(vec![raw("x", USDC, 10, false, 1)]);
        let first = ingest_next(&mut src, |_| 5000);
        assert_eq!(first.calls.len(), 1);
        let second = ingest_next(&mut src, |_| 5000);
        assert!(second.calls.is_empty() && second.mentions.is_empty());
    }

    fn classification(state: SourceState, conf: u16) -> Classification {
        Classification {
            state,
            confidence_bps: conf,
            decay_half_life_ns: 1,
        }
    }

    #[test]
    fn quality_from_classification_scales_and_fades() {
        let p = SourceQualityPolicy::conservative();
        // Pre-flow alpha at full confidence hits the ceiling; at half, half.
        assert_eq!(
            quality_from_classification(&classification(SourceState::PreFlowAlpha, 10_000), &p),
            p.pre_flow_alpha_bp
        );
        assert_eq!(
            quality_from_classification(&classification(SourceState::PreFlowAlpha, 5_000), &p),
            p.pre_flow_alpha_bp / 2
        );
        // Every fade state contributes zero corroboration regardless of confidence.
        for st in [
            SourceState::LateExitLiquidityPromoter,
            SourceState::PaidShillSuspect,
            SourceState::EngagementFarm,
            SourceState::CopyEchoAccount,
        ] {
            assert_eq!(
                quality_from_classification(&classification(st, 10_000), &p),
                0
            );
        }
        // Insufficient sample resolves to the PUBLIC_BURNED baseline floor.
        assert_eq!(
            quality_from_classification(&classification(SourceState::InsufficientSample, 0), &p),
            p.baseline_bp
        );
    }

    #[test]
    fn ledger_quality_unseen_source_is_public_burned_baseline() {
        let ledger = SourceQualityLedger::with_capacity(8);
        let p = SourceQualityPolicy::conservative();
        // A source the ledger has never reconciled → baseline, never trust.
        assert_eq!(ledger_quality(&ledger, 12345, &p), p.baseline_bp);
    }

    #[test]
    fn coordination_flags_distinct_authors_same_content_in_window() {
        // Same text from two distinct authors within the window → coordinated.
        let mk = |author: &str, at: u64| {
            let json = format!(
                r#"{{"platform":"telegram","author":"{author}","text":"BUY $PEPE now same copypasta","likes":1}}"#
            );
            parse_social_event(json.as_bytes(), at).unwrap()
        };
        let a = mk("chanA", 100);
        let b = mk("chanB", 150);
        let c = mk("chanA", 160); // same author as a → not a new distinct author
        assert_eq!(a.content_hash, b.content_hash);
        let coord = coordinated_content(&[a, b, c], 1_000);
        assert_eq!(coord.len(), 1);
        assert_eq!(coord[0], (a.content_hash, 2), "two distinct authors");
        // Outside the window → not coordinated.
        let far = mk("chanC", 10_000);
        let none = coordinated_content(&[a, far], 100);
        assert!(none.is_empty());
    }
}
