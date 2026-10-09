//! Item 3 (m1acc follow-up): BEHAVIOURAL tests of the Helius WS reconnect episode and the LaserStream respawn,
//! extracted from pq_daemon `main` into `stream_recovery` and driven by an injected connector / spawner and a
//! controlled clock. Mutation-checked (proc/m1accR1/mutants_item3.out).

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use pump_quant_junction::endpoint_retry::{RetryBound, HELIUS_WS_RECONNECT, LASERSTREAM_RESPAWN};
use pump_quant_junction::stream_recovery::{
    respawn_step, spawn_line_reader, ws_reconnect_episode, RespawnDecision, RespawnGovernor,
    Sleeper, WsOutcome, LS_QUEUE_CAP,
};

/// Controlled clock: records every requested sleep; cancels when the n-th sleep starts (if set).
#[derive(Default)]
struct FakeClock {
    slept: Vec<u64>,
    now_ms: u64,
    cancel_at_sleep: Option<usize>,
}

impl Sleeper for FakeClock {
    fn sleep_ms(&mut self, ms: u64) -> bool {
        self.slept.push(ms);
        if Some(self.slept.len()) == self.cancel_at_sleep {
            self.now_ms += ms / 2; // cut short mid-backoff
            return false;
        }
        self.now_ms += ms;
        true
    }
}

/// Fake connection tracking how many are live at once.
struct Conn {
    live: Rc<RefCell<(i32, i32)>>, // (live now, max ever)
}
impl Drop for Conn {
    fn drop(&mut self) {
        self.live.borrow_mut().0 -= 1;
    }
}

/// Connector failing the first `fail_n` calls.
fn connector(
    fail_n: u32,
    calls: Rc<RefCell<u32>>,
    live: Rc<RefCell<(i32, i32)>>,
) -> impl FnMut() -> Result<Conn, String> {
    move || {
        *calls.borrow_mut() += 1;
        if *calls.borrow() <= fail_n {
            return Err(format!("refused #{}", calls.borrow()));
        }
        let mut l = live.borrow_mut();
        l.0 += 1;
        l.1 = l.1.max(l.0);
        Ok(Conn { live: live.clone() })
    }
}

#[test]
fn ws_exhaustion_is_named_after_exactly_max_attempts_with_the_capped_ladder() {
    let calls = Rc::new(RefCell::new(0));
    let live = Rc::new(RefCell::new((0, 0)));
    let mut clk = FakeClock::default();
    let mut fails = Vec::new();
    let out = ws_reconnect_episode(
        &HELIUS_WS_RECONNECT,
        || {},
        connector(u32::MAX, calls.clone(), live.clone()),
        |n, b, _e: &String| fails.push((n, b)),
        &mut clk,
    );
    assert!(matches!(out, WsOutcome::Exhausted { attempts: 5 }));
    assert_eq!(
        *calls.borrow(),
        5,
        "attempt limit: exactly 5 connects, no more"
    );
    assert_eq!(
        clk.slept,
        vec![500, 1_000, 2_000, 4_000, 8_000],
        "a sleep after every failure"
    );
    assert_eq!(
        fails.iter().map(|f| f.0).collect::<Vec<_>>(),
        vec![1, 2, 3, 4, 5]
    );
    assert_eq!(live.borrow().1, 0);
}

#[test]
fn ws_backoff_is_capped() {
    let b = RetryBound {
        name: "t",
        max_attempts: 8,
        base_backoff_ms: 500,
        cap_backoff_ms: 10_000,
    };
    let mut clk = FakeClock::default();
    let out = ws_reconnect_episode(&b, || {}, || Err::<(), _>("x"), |_, _, _| {}, &mut clk);
    assert!(matches!(out, WsOutcome::Exhausted { attempts: 8 }));
    assert_eq!(
        clk.slept,
        vec![500, 1_000, 2_000, 4_000, 8_000, 10_000, 10_000, 10_000]
    );
}

#[test]
fn ws_recovery_after_n_failures_returns_the_connection_and_the_next_episode_starts_at_the_base() {
    let calls = Rc::new(RefCell::new(0));
    let live = Rc::new(RefCell::new((0, 0)));
    let mut clk = FakeClock::default();
    let out = ws_reconnect_episode(
        &HELIUS_WS_RECONNECT,
        || {},
        connector(2, calls.clone(), live.clone()),
        |_, _, _| {},
        &mut clk,
    );
    let WsOutcome::Connected { conn, attempt } = out else {
        panic!("expected recovery")
    };
    assert_eq!(attempt, 3);
    assert_eq!(*calls.borrow(), 3, "no attempt after success");
    assert_eq!(clk.slept, vec![500, 1_000]);
    // Next episode (the existing contract: each episode restarts the ladder at the base).
    let mut clk2 = FakeClock::default();
    let mut slot = Some(conn);
    let out2 = ws_reconnect_episode(
        &HELIUS_WS_RECONNECT,
        || drop(slot.take()),
        || Err::<Conn, _>("down"),
        |_, _, _| {},
        &mut clk2,
    );
    assert!(matches!(out2, WsOutcome::Exhausted { .. }));
    assert_eq!(clk2.slept[0], 500, "ladder reset per episode");
}

#[test]
fn ws_cancellation_mid_backoff_stops_with_no_further_attempts() {
    let calls = Rc::new(RefCell::new(0));
    let live = Rc::new(RefCell::new((0, 0)));
    let mut clk = FakeClock {
        cancel_at_sleep: Some(2),
        ..Default::default()
    };
    let out = ws_reconnect_episode(
        &HELIUS_WS_RECONNECT,
        || {},
        connector(u32::MAX, calls.clone(), live.clone()),
        |_, _, _| {},
        &mut clk,
    );
    assert!(matches!(out, WsOutcome::Cancelled { attempts: 2 }));
    assert_eq!(*calls.borrow(), 2, "no connect after cancellation");
    assert_eq!(clk.slept, vec![500, 1_000]);
}

#[test]
fn ws_old_connection_is_closed_before_the_first_new_connect_so_at_most_one_is_live() {
    let calls = Rc::new(RefCell::new(0));
    let live = Rc::new(RefCell::new((0, 0)));
    let mut conn = connector(0, calls.clone(), live.clone());
    let mut current = Some(conn().unwrap());
    for _ in 0..10 {
        let mut clk = FakeClock::default();
        let slot = &mut current;
        let out = ws_reconnect_episode(
            &HELIUS_WS_RECONNECT,
            || drop(slot.take()),
            &mut conn,
            |_, _, _| {},
            &mut clk,
        );
        let WsOutcome::Connected { conn: c, .. } = out else {
            panic!()
        };
        current = Some(c);
        assert_eq!(live.borrow().0, 1);
    }
    assert_eq!(live.borrow().1, 1, "never two live connections");
}

#[test]
fn ws_real_sleeper_honours_cancellation_promptly() {
    use pump_quant_junction::stream_recovery::RealSleeper;
    let t = std::time::Instant::now();
    let mut s = RealSleeper { cancelled: || true };
    assert!(!s.sleep_ms(10_000));
    assert!(t.elapsed() < Duration::from_millis(500));
    let mut s = RealSleeper {
        cancelled: || false,
    };
    let t = std::time::Instant::now();
    assert!(s.sleep_ms(120));
    assert!(t.elapsed() >= Duration::from_millis(120));
}

// ---- LaserStream respawn ----

struct Child {
    id: u32,
    live: Rc<RefCell<(i32, i32)>>,
    killed: bool,
}

fn kill(c: &mut Child) {
    assert!(!c.killed, "reaped twice");
    c.killed = true;
    c.live.borrow_mut().0 -= 1;
}

#[test]
fn ls_cooldown_limit_and_named_exhaustion_with_no_further_attempts() {
    let live = Rc::new(RefCell::new((1, 1)));
    let mut cur = Some(Child {
        id: 0,
        live: live.clone(),
        killed: false,
    });
    let mut gov = RespawnGovernor::default();
    let spawns = Rc::new(RefCell::new(0u32));
    let mut decisions = Vec::new();
    // Disconnect polled every 1 s for 200 s.
    for t in 0..200u64 {
        let sp = spawns.clone();
        let lv = live.clone();
        let (d, ok) = respawn_step(
            &mut gov,
            &LASERSTREAM_RESPAWN,
            t * 1_000,
            &mut cur,
            kill,
            move || {
                *sp.borrow_mut() += 1;
                let mut l = lv.borrow_mut();
                l.0 += 1;
                l.1 = l.1.max(l.0);
                Some(Child {
                    id: *sp.borrow(),
                    live: lv.clone(),
                    killed: false,
                })
            },
        );
        if d != RespawnDecision::CooldownWait {
            decisions.push((t, d, ok));
        }
    }
    let attempts: Vec<u64> = decisions
        .iter()
        .filter(|x| matches!(x.1, RespawnDecision::Attempt(_)))
        .map(|x| x.0)
        .collect();
    assert_eq!(
        attempts,
        vec![0, 15, 30, 45, 60],
        "15 s cooldown spacing, 5 attempts"
    );
    assert_eq!(*spawns.borrow(), 5);
    assert_eq!(
        decisions[5].0, 75,
        "exhaustion reported at the next eligible disconnect"
    );
    assert_eq!(decisions[5].1, RespawnDecision::Exhausted);
    assert!(decisions[6..]
        .iter()
        .all(|x| x.1 == RespawnDecision::GaveUp && x.2.is_none()));
    assert_eq!(live.borrow().1, 1, "at most one live child");
    assert_eq!(cur.as_ref().map(|c| c.id), Some(5));
}

#[test]
fn ls_success_does_not_reset_the_per_process_count() {
    let mut gov = RespawnGovernor::default();
    for k in 0..5u64 {
        assert_eq!(
            gov.on_disconnect(&LASERSTREAM_RESPAWN, k * 15_000),
            RespawnDecision::Attempt(k as u32 + 1)
        );
    }
    // Long healthy period after the last success, then another disconnect: still exhausted.
    assert_eq!(
        gov.on_disconnect(&LASERSTREAM_RESPAWN, 10_000_000),
        RespawnDecision::Exhausted
    );
}

#[test]
fn ls_failed_spawn_counts_as_an_attempt_and_leaves_no_child() {
    let live = Rc::new(RefCell::new((1, 1)));
    let mut cur = Some(Child {
        id: 0,
        live: live.clone(),
        killed: false,
    });
    let mut gov = RespawnGovernor::default();
    let (d, ok) = respawn_step(&mut gov, &LASERSTREAM_RESPAWN, 0, &mut cur, kill, || None);
    assert_eq!((d, ok), (RespawnDecision::Attempt(1), Some(false)));
    assert!(cur.is_none(), "old child killed, none live");
    assert_eq!(live.borrow().0, 0);
    let (d, _) = respawn_step(
        &mut gov,
        &LASERSTREAM_RESPAWN,
        1_000,
        &mut cur,
        kill,
        || None,
    );
    assert_eq!(d, RespawnDecision::CooldownWait);
}

#[test]
fn ls_old_child_is_killed_before_the_new_one_spawns() {
    let order = Rc::new(RefCell::new(Vec::new()));
    let live = Rc::new(RefCell::new((1, 1)));
    let mut cur = Some(Child {
        id: 0,
        live: live.clone(),
        killed: false,
    });
    let mut gov = RespawnGovernor::default();
    let o1 = order.clone();
    let o2 = order.clone();
    let lv = live.clone();
    respawn_step(
        &mut gov,
        &LASERSTREAM_RESPAWN,
        0,
        &mut cur,
        move |c| {
            o1.borrow_mut().push("kill");
            kill(c)
        },
        move || {
            o2.borrow_mut().push("spawn");
            lv.borrow_mut().0 += 1;
            Some(Child {
                id: 1,
                live: lv.clone(),
                killed: false,
            })
        },
    );
    assert_eq!(*order.borrow(), vec!["kill", "spawn"]);
}

// ---- bounded queue, no leaked reader threads ----

#[test]
fn reader_threads_exit_on_eof_and_the_queue_is_bounded() {
    // 20 respawns: each reader gets a finite stream (the killed child's pipe hits EOF); every thread ends.
    let (tx, rx) = std::sync::mpsc::sync_channel::<String>(4);
    let mut handles = Vec::new();
    for i in 0..20 {
        let data = format!("a{i}\nb{i}\n");
        handles.push(spawn_line_reader(
            std::io::Cursor::new(data.into_bytes()),
            tx.clone(),
            |l| Some(l.to_string()),
        ));
        // drain so readers can finish
        while let Ok(_m) = rx.recv_timeout(Duration::from_millis(50)) {}
        let h = handles.last().unwrap();
        let t = std::time::Instant::now();
        while !h.is_finished() && t.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    assert!(
        handles.iter().all(|h| h.is_finished()),
        "no leaked reader thread"
    );
    // Bounded: the LIBRARY reader with 100 lines and no consumer parses at most capacity + 1 (one blocked in send).
    static PARSED: AtomicUsize = AtomicUsize::new(0);
    let (tx2, rx2) = std::sync::mpsc::sync_channel::<usize>(4);
    let data: String = (0..100).map(|i| format!("{i}\n")).collect();
    let h2 = spawn_line_reader(std::io::Cursor::new(data.into_bytes()), tx2, |l| {
        PARSED.fetch_add(1, Ordering::SeqCst);
        l.parse().ok()
    });
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        PARSED.load(Ordering::SeqCst) <= 5,
        "queue bounded at its capacity"
    );
    assert!(
        !h2.is_finished(),
        "the reader blocks (back-pressure) instead of buffering"
    );
    drop(rx2);
    h2.join().unwrap();
    let _ = Arc::new(0);
    // Dropping the receiver ends a blocked library reader too - even on an ENDLESS stream (a live child that keeps
    // writing), so it cannot rely on EOF.
    let (tx3, rx3) = std::sync::mpsc::sync_channel::<String>(1);
    let h3 = spawn_line_reader(std::io::repeat(b'\n'), tx3, |l| Some(l.to_string()));
    std::thread::sleep(Duration::from_millis(50));
    drop(rx3);
    let t = std::time::Instant::now();
    while !h3.is_finished() && t.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(h3.is_finished(), "reader exits when the receiver is gone");
    assert!(LS_QUEUE_CAP > 0 && LS_QUEUE_CAP <= 65_536);
}
