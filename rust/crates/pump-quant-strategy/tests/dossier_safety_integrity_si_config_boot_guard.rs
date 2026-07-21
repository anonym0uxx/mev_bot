#![allow(unused_imports)]
use pump_quant_strategy::safety_integrity::*;

#[test]
fn live_armed_committed_rejected() {
    let cfg = BootConfig { live_armed: true, committed_to_source: true, shadow: false, live: false };
    assert_eq!(validate_boot_config(&cfg), Err(BootError::LiveArmedCommitted));
}
#[test]
fn contradictory_rejected() {
    let cfg = BootConfig { live_armed: false, committed_to_source: true, shadow: true, live: true };
    assert_eq!(validate_boot_config(&cfg), Err(BootError::Contradictory));
}
#[test]
fn clean_config_accepted() {
    let cfg = BootConfig { live_armed: false, committed_to_source: true, shadow: true, live: false };
    let v = validate_boot_config(&cfg).expect("clean config should validate");
    assert!(v.shadow());
    assert!(!v.live());
}
