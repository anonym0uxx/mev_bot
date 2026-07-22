use pump_quant_narrative::{nv_class_classify, ClassFeatures, NarrativeClass};

fn f(spike: u64, mainstream: bool, longevity: u32, breadth: u32) -> ClassFeatures {
    ClassFeatures {
        spike_ratio_fp: spike,
        mainstream_led: mainstream,
        longevity_windows: longevity,
        source_breadth: breadth,
    }
}

// thresholds shared across cases:
const SPIKE_T: u64 = 30_000; // 3.0x
const LONG_T: u32 = 6;
const TECH_BREADTH: u32 = 20;

#[test]
fn mainstream_spike_is_news() {
    // mainstream_led + spike >= 3.0 -> News even if long-lived.
    let c = nv_class_classify(&f(40_000, true, 10, 30), SPIKE_T, LONG_T, TECH_BREADTH);
    assert_eq!(c, NarrativeClass::News);
}

#[test]
fn steady_broad_lowspike_is_tech() {
    // not mainstream spike; longevity>=6, breadth>=20, spike<3.0 -> Tech.
    let c = nv_class_classify(&f(15_000, false, 8, 25), SPIKE_T, LONG_T, TECH_BREADTH);
    assert_eq!(c, NarrativeClass::Tech);
}

#[test]
fn durable_narrow_is_culture() {
    // longevity>=6 but breadth<20 -> falls through Tech to Culture.
    let c = nv_class_classify(&f(15_000, false, 9, 5), SPIKE_T, LONG_T, TECH_BREADTH);
    assert_eq!(c, NarrativeClass::Culture);
}

#[test]
fn durable_broad_but_spiky_is_culture() {
    // longevity>=6, breadth>=20 but spike>=threshold blocks Tech -> Culture.
    let c = nv_class_classify(&f(35_000, false, 9, 25), SPIKE_T, LONG_T, TECH_BREADTH);
    assert_eq!(c, NarrativeClass::Culture);
}

#[test]
fn young_fast_is_trend_default() {
    // short-lived, not mainstream-spike -> Trend.
    let c = nv_class_classify(&f(20_000, false, 2, 5), SPIKE_T, LONG_T, TECH_BREADTH);
    assert_eq!(c, NarrativeClass::Trend);
}

#[test]
fn mainstream_without_spike_is_not_news() {
    // mainstream_led true but spike below threshold -> not News;
    // short longevity -> Trend.
    let c = nv_class_classify(&f(10_000, true, 3, 5), SPIKE_T, LONG_T, TECH_BREADTH);
    assert_eq!(c, NarrativeClass::Trend);
}

#[test]
fn boundary_spike_equal_threshold_is_news() {
    // spike == threshold counts (>=).
    let c = nv_class_classify(&f(SPIKE_T, true, 1, 1), SPIKE_T, LONG_T, TECH_BREADTH);
    assert_eq!(c, NarrativeClass::News);
}
