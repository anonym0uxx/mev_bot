#![allow(unused_imports)]
use pump_quant_core::replay::*;
#[test]
fn prop_epoch_merge_total_order() {
    let f = |e, s| FrameMeta::test(e, s);
    let frames = vec![f(2, 0), f(1, 5), f(1, 4), f(2, 1)];
    let order = epoch_merge(&frames).unwrap();
    let seq: Vec<_> = order.iter().map(|&i| (frames[i].epoch, frames[i].seq)).collect();
    assert_eq!(seq, vec![(1, 4), (1, 5), (2, 0), (2, 1)]);
    assert!(epoch_merge(&[f(1, 1), f(1, 1)]).is_err());
}
