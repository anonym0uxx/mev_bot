#![allow(unused_imports)]
use pump_quant_ingest::source_registry::*;

// Expectations are derived from the §14.5 / §15 / §16 rules independently of the
// implementation, and multiple inputs (incl. edge cases) are exercised so a
// memorized single answer would fail.

#[test]
fn jito_is_transitional_when_sunset_announced() {
    // §14.5: repository Jito ShredStream code is TRANSITIONAL (sunset is a
    // verified fact per §18.3.1 → caller passes true).
    assert_eq!(
        classify_source(SourceId::JitoShredStream, true),
        SourceClass::Transitional
    );
}

#[test]
fn earliest_shred_sunset_flag_drives_class() {
    // The sunset flag is what distinguishes Transitional from Successor for an
    // earliest-source shred feed — check both feeds under both flag values.
    assert_eq!(
        classify_source(SourceId::SuccessorShred, false),
        SourceClass::Successor
    );
    assert_eq!(
        classify_source(SourceId::SuccessorShred, true),
        SourceClass::Transitional
    );
    assert_eq!(
        classify_source(SourceId::JitoShredStream, false),
        SourceClass::Successor
    );
}

#[test]
fn fixed_class_sources_ignore_sunset_flag() {
    for sunset in [false, true] {
        assert_eq!(
            classify_source(SourceId::HeliusWsLogs, sunset),
            SourceClass::Legacy
        );
        assert_eq!(
            classify_source(SourceId::HeliusLaserStream, sunset),
            SourceClass::StructuredPrimary
        );
        assert_eq!(
            classify_source(SourceId::HeliusProviderReplay, sunset),
            SourceClass::StructuredPrimary
        );
        assert_eq!(
            classify_source(SourceId::CanonicalRpc, sunset),
            SourceClass::CanonicalRepair
        );
        assert_eq!(
            classify_source(SourceId::ReconciledExecution, sunset),
            SourceClass::ReconciledTruth
        );
    }
}

#[test]
fn single_source_mix_labels() {
    assert_eq!(
        mix_label_for(SourceId::JitoShredStream),
        Some(MixLabel::JitoTransitionalLive)
    );
    assert_eq!(
        mix_label_for(SourceId::SuccessorShred),
        Some(MixLabel::SuccessorShredLive)
    );
    assert_eq!(
        mix_label_for(SourceId::HeliusLaserStream),
        Some(MixLabel::HeliusLaserStreamLive)
    );
    assert_eq!(
        mix_label_for(SourceId::HeliusProviderReplay),
        Some(MixLabel::HeliusProviderReplay)
    );
    assert_eq!(
        mix_label_for(SourceId::CanonicalRpc),
        Some(MixLabel::CanonicalRpcRepair)
    );
    assert_eq!(
        mix_label_for(SourceId::ReconciledExecution),
        Some(MixLabel::ReconciledLiveExecution)
    );
    // Legacy Helius WS feed has no canonical dataset mix label.
    assert_eq!(mix_label_for(SourceId::HeliusWsLogs), None);
}

#[test]
fn combine_mix_labels_rules() {
    // Empty → None.
    assert_eq!(combine_mix_labels(&[]), None);

    // Single distinct (even repeated) → preserved.
    assert_eq!(
        combine_mix_labels(&[MixLabel::HeliusLaserStreamLive]),
        Some(MixLabel::HeliusLaserStreamLive)
    );
    assert_eq!(
        combine_mix_labels(&[
            MixLabel::HeliusLaserStreamLive,
            MixLabel::HeliusLaserStreamLive
        ]),
        Some(MixLabel::HeliusLaserStreamLive)
    );

    // Two or more distinct → DualOrMultiFeedRecorded (§16 non-collapse).
    assert_eq!(
        combine_mix_labels(&[
            MixLabel::HeliusLaserStreamLive,
            MixLabel::JitoTransitionalLive
        ]),
        Some(MixLabel::DualOrMultiFeedRecorded)
    );
    assert_eq!(
        combine_mix_labels(&[
            MixLabel::JitoTransitionalLive,
            MixLabel::SuccessorShredLive,
            MixLabel::CanonicalRpcRepair
        ]),
        Some(MixLabel::DualOrMultiFeedRecorded)
    );
}

#[test]
fn shadow_recorded_single_label_is_preserved() {
    // A row produced purely in shadow mode keeps its LIVE_SHADOW_RECORDED label
    // (even repeated), exactly as any single-source label is preserved.
    assert_eq!(
        combine_mix_labels(&[MixLabel::LiveShadowRecorded]),
        Some(MixLabel::LiveShadowRecorded)
    );
    assert_eq!(
        combine_mix_labels(&[MixLabel::LiveShadowRecorded, MixLabel::LiveShadowRecorded]),
        Some(MixLabel::LiveShadowRecorded)
    );
}

#[test]
fn shadow_recorded_is_absorbing_over_live_feeds() {
    // §16 safe-direction lattice: a shadow contribution taints the whole record
    // — it must NOT collapse to DualOrMultiFeedRecorded (which would overclaim
    // live fidelity). Shadow wins regardless of which/how many live feeds also
    // contributed, and regardless of ordering.
    assert_eq!(
        combine_mix_labels(&[
            MixLabel::HeliusLaserStreamLive,
            MixLabel::LiveShadowRecorded
        ]),
        Some(MixLabel::LiveShadowRecorded)
    );
    assert_eq!(
        combine_mix_labels(&[
            MixLabel::LiveShadowRecorded,
            MixLabel::HeliusLaserStreamLive
        ]),
        Some(MixLabel::LiveShadowRecorded)
    );
    assert_eq!(
        combine_mix_labels(&[
            MixLabel::JitoTransitionalLive,
            MixLabel::SuccessorShredLive,
            MixLabel::LiveShadowRecorded,
            MixLabel::CanonicalRpcRepair
        ]),
        Some(MixLabel::LiveShadowRecorded)
    );
}

#[test]
fn shadow_recorded_has_no_originating_source() {
    // LIVE_SHADOW_RECORDED is a recording-mode label, not a feed source, so no
    // SourceId maps to it: every SourceId yields something other than shadow.
    for id in [
        SourceId::JitoShredStream,
        SourceId::SuccessorShred,
        SourceId::HeliusWsLogs,
        SourceId::HeliusLaserStream,
        SourceId::HeliusProviderReplay,
        SourceId::CanonicalRpc,
        SourceId::ReconciledExecution,
    ] {
        assert_ne!(mix_label_for(id), Some(MixLabel::LiveShadowRecorded));
    }
}
