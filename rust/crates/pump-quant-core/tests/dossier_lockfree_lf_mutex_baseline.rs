#![allow(unused_imports)]
use pump_quant_core::lockfree::*;
#[test]
fn prop_mutex_queue_fifo_bounded() {
    let q: MutexQueue<u64, 4> = MutexQueue::new();
    for i in 0..4 { assert!(q.push(i).is_ok()); }
    assert_eq!(q.push(99), Err(99));
    for i in 0..4 { assert_eq!(q.pop(), Some(i)); }
    assert_eq!(q.pop(), None);
}
