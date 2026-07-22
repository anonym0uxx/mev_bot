//! Leaf ca_rotation_source: rotation-detection input-source guard (criterion 85).

use pump_quant_strategy::capital_allocator::{
    admit_rotation_trigger, RotationReject, TriggerSource,
};

#[test]
fn accepts_only_on_chain_derived() {
    assert_eq!(
        admit_rotation_trigger(TriggerSource::OnChainDerived),
        Ok(())
    );
}

#[test]
fn rejects_loss_triggered() {
    assert_eq!(
        admit_rotation_trigger(TriggerSource::LossTriggered),
        Err(RotationReject::LossTriggered)
    );
}

#[test]
fn rejects_social_led() {
    assert_eq!(
        admit_rotation_trigger(TriggerSource::SocialLed),
        Err(RotationReject::SocialLed)
    );
}

#[test]
fn exhaustive_sources() {
    for src in [
        TriggerSource::OnChainDerived,
        TriggerSource::LossTriggered,
        TriggerSource::SocialLed,
    ] {
        let got = admit_rotation_trigger(src);
        let expected = match src {
            TriggerSource::OnChainDerived => Ok(()),
            TriggerSource::LossTriggered => Err(RotationReject::LossTriggered),
            TriggerSource::SocialLed => Err(RotationReject::SocialLed),
        };
        assert_eq!(got, expected);
    }
}
