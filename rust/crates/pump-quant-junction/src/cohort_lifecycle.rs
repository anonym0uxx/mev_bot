//! Durable, monotonic paper-cohort lifecycle: COLLECTING → DRAINING → CLOSED.
//!
//! * COLLECTING: entries may be dispatched. The stopping rule is evaluated every tick from the
//!   DURABLE dispatch accounting: `prior_dispatch_known` (unique dispatches of earlier process
//!   generations, from their persisted records) + this process's `dispatched` counter +
//!   `dispatch_uncertainty` (the bounded number of earlier dispatches whose records could not be
//!   recovered) must stay BELOW `dispatch_cap`; or the wall-clock deadline is reached.
//! * DRAINING: new entry dispatch and every exposure-increasing acceptance (including a BUY answer
//!   to a request dispatched before the cutoff) are blocked by the engine's entry block, re-asserted
//!   every tick (a SAFETY re-arm would otherwise release it). Supervision, protection, exits,
//!   pending-fill reconciliation and feeds continue. Persisted; survives restart.
//! * CLOSED: reached only from DRAINING with flat inventory, no pending orders and no open exit
//!   intents. Terminal.
//!
//! Transitions are appended with reason, clock and counts; the file is written atomically
//! (tmp + rename). The cohort clock and decision count are never reset by a transition.
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Collecting,
    Draining,
    Closed,
}

#[derive(Clone, Debug)]
pub struct Transition {
    pub to: State,
    pub reason: String,
    pub clock_ms: i64,
    pub prior_dispatch_known: u64,
    pub own_dispatched: u64,
    pub dispatch_uncertainty: u64,
    /// `prior + own + uncertainty` at the transition; `cap` − this is the (possibly negative) headroom.
    pub conservative_total: u64,
    pub open_positions: usize,
    pub pending_orders: usize,
}

#[derive(Clone, Debug)]
pub struct CohortLifecycle {
    pub state: State,
    pub dispatch_cap: u64,
    pub deadline_ms: i64,
    pub prior_dispatch_known: u64,
    pub dispatch_uncertainty: u64,
    pub transitions: Vec<Transition>,
    path: PathBuf,
}

#[derive(Debug)]
pub enum LifecycleError {
    Io(std::io::Error),
    Parse(String),
    NotMonotonic { from: State, to: State },
}

impl std::fmt::Display for LifecycleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LifecycleError::Io(e) => write!(f, "lifecycle io: {e}"),
            LifecycleError::Parse(e) => write!(f, "lifecycle parse: {e}"),
            LifecycleError::NotMonotonic { from, to } => {
                write!(f, "lifecycle transition {from:?} -> {to:?} is not monotonic")
            }
        }
    }
}

impl CohortLifecycle {
    /// Load the persisted lifecycle (state restored verbatim, including DRAINING) or initialise
    /// COLLECTING. The cap/deadline/prior/uncertainty of an EXISTING file win over the arguments:
    /// a restart may not loosen the rule.
    pub fn load_or_init(
        path: &Path,
        dispatch_cap: u64,
        deadline_ms: i64,
        prior_dispatch_known: u64,
        dispatch_uncertainty: u64,
        now_ms: i64,
    ) -> Result<Self, LifecycleError> {
        if path.exists() {
            let text = std::fs::read_to_string(path).map_err(LifecycleError::Io)?;
            let v: Value = serde_json::from_str(&text).map_err(|e| LifecycleError::Parse(e.to_string()))?;
            return Self::from_value(&v, path);
        }
        let lc = CohortLifecycle {
            state: State::Collecting,
            dispatch_cap,
            deadline_ms,
            prior_dispatch_known,
            dispatch_uncertainty,
            transitions: vec![Transition {
                to: State::Collecting,
                reason: "init".into(),
                clock_ms: now_ms,
                prior_dispatch_known,
                own_dispatched: 0,
                dispatch_uncertainty,
                conservative_total: prior_dispatch_known + dispatch_uncertainty,
                open_positions: 0,
                pending_orders: 0,
            }],
            path: path.to_path_buf(),
        };
        lc.persist()?;
        Ok(lc)
    }

    pub fn conservative_total(&self, own_dispatched: u64) -> u64 {
        self.prior_dispatch_known + own_dispatched + self.dispatch_uncertainty
    }

    /// The stopping rule, evaluated BEFORE the next dispatch could happen: drain when one more
    /// dispatch could take the conservative total past the cap, or at the deadline.
    pub fn cutoff_reason(&self, own_dispatched: u64, now_ms: i64) -> Option<&'static str> {
        if self.state != State::Collecting {
            return None;
        }
        if self.conservative_total(own_dispatched) >= self.dispatch_cap {
            return Some("dispatch_cap");
        }
        if now_ms >= self.deadline_ms {
            return Some("deadline");
        }
        None
    }

    pub fn transition(
        &mut self,
        to: State,
        reason: &str,
        now_ms: i64,
        own_dispatched: u64,
        open_positions: usize,
        pending_orders: usize,
    ) -> Result<(), LifecycleError> {
        let ok = matches!(
            (self.state, to),
            (State::Collecting, State::Draining) | (State::Draining, State::Closed)
        );
        if !ok {
            return Err(LifecycleError::NotMonotonic { from: self.state, to });
        }
        if to == State::Closed && (open_positions != 0 || pending_orders != 0) {
            return Err(LifecycleError::Parse(format!(
                "CLOSED requires flat inventory and no pending orders (open={open_positions}, pending={pending_orders})"
            )));
        }
        self.transitions.push(Transition {
            to,
            reason: reason.to_string(),
            clock_ms: now_ms,
            prior_dispatch_known: self.prior_dispatch_known,
            own_dispatched,
            dispatch_uncertainty: self.dispatch_uncertainty,
            conservative_total: self.conservative_total(own_dispatched),
            open_positions,
            pending_orders,
        });
        self.state = to;
        self.persist()
    }

    fn state_str(s: State) -> &'static str {
        match s {
            State::Collecting => "COLLECTING",
            State::Draining => "DRAINING",
            State::Closed => "CLOSED",
        }
    }

    fn parse_state(s: &str) -> Result<State, LifecycleError> {
        match s {
            "COLLECTING" => Ok(State::Collecting),
            "DRAINING" => Ok(State::Draining),
            "CLOSED" => Ok(State::Closed),
            other => Err(LifecycleError::Parse(format!("unknown state {other}"))),
        }
    }

    fn u(v: &Value, k: &str) -> Result<u64, LifecycleError> {
        v.get(k).and_then(Value::as_u64).ok_or_else(|| LifecycleError::Parse(format!("missing {k}")))
    }
    fn i(v: &Value, k: &str) -> Result<i64, LifecycleError> {
        v.get(k).and_then(Value::as_i64).ok_or_else(|| LifecycleError::Parse(format!("missing {k}")))
    }

    fn from_value(v: &Value, path: &Path) -> Result<Self, LifecycleError> {
        let state = Self::parse_state(v.get("state").and_then(Value::as_str).unwrap_or(""))?;
        let mut transitions = Vec::new();
        for t in v.get("transitions").and_then(Value::as_array).into_iter().flatten() {
            transitions.push(Transition {
                to: Self::parse_state(t.get("to").and_then(Value::as_str).unwrap_or(""))?,
                reason: t.get("reason").and_then(Value::as_str).unwrap_or("").to_string(),
                clock_ms: Self::i(t, "clock_ms")?,
                prior_dispatch_known: Self::u(t, "prior_dispatch_known")?,
                own_dispatched: Self::u(t, "own_dispatched")?,
                dispatch_uncertainty: Self::u(t, "dispatch_uncertainty")?,
                conservative_total: Self::u(t, "conservative_total")?,
                open_positions: Self::u(t, "open_positions")? as usize,
                pending_orders: Self::u(t, "pending_orders")? as usize,
            });
        }
        Ok(CohortLifecycle {
            state,
            dispatch_cap: Self::u(v, "dispatch_cap")?,
            deadline_ms: Self::i(v, "deadline_ms")?,
            prior_dispatch_known: Self::u(v, "prior_dispatch_known")?,
            dispatch_uncertainty: Self::u(v, "dispatch_uncertainty")?,
            transitions,
            path: path.to_path_buf(),
        })
    }

    fn to_value(&self) -> Value {
        json!({
            "state": Self::state_str(self.state),
            "dispatch_cap": self.dispatch_cap,
            "deadline_ms": self.deadline_ms,
            "prior_dispatch_known": self.prior_dispatch_known,
            "dispatch_uncertainty": self.dispatch_uncertainty,
            "transitions": self.transitions.iter().map(|t| json!({
                "to": Self::state_str(t.to), "reason": t.reason, "clock_ms": t.clock_ms,
                "prior_dispatch_known": t.prior_dispatch_known, "own_dispatched": t.own_dispatched,
                "dispatch_uncertainty": t.dispatch_uncertainty, "conservative_total": t.conservative_total,
                "open_positions": t.open_positions, "pending_orders": t.pending_orders,
            })).collect::<Vec<_>>(),
        })
    }

    fn persist(&self) -> Result<(), LifecycleError> {
        let tmp = self.path.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(&self.to_value()).map_err(|e| LifecycleError::Parse(e.to_string()))?;
        std::fs::write(&tmp, text).map_err(LifecycleError::Io)?;
        std::fs::rename(&tmp, &self.path).map_err(LifecycleError::Io)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pq_lifecycle_{name}_{}", std::process::id()));
        let _ = std::fs::remove_file(&d);
        d
    }

    #[test]
    fn cap_is_evaluated_conservatively_before_the_next_dispatch() {
        let p = tmp("cap");
        let lc = CohortLifecycle::load_or_init(&p, 3_000, i64::MAX, 2_553, 8, 0).unwrap();
        assert_eq!(lc.cutoff_reason(438, 0), None, "2553+438+8 = 2999 < 3000");
        assert_eq!(lc.cutoff_reason(439, 0), Some("dispatch_cap"), "one more could exceed the cap");
    }

    #[test]
    fn deadline_cuts_off_and_lifecycle_is_monotonic_and_persisted_across_reload() {
        let p = tmp("mono");
        let mut lc = CohortLifecycle::load_or_init(&p, 3_000, 1_000, 0, 0, 0).unwrap();
        assert_eq!(lc.cutoff_reason(1, 999), None);
        assert_eq!(lc.cutoff_reason(1, 1_000), Some("deadline"));
        assert!(matches!(
            lc.transition(State::Closed, "x", 1_000, 1, 0, 0),
            Err(LifecycleError::NotMonotonic { .. })
        ), "COLLECTING may not jump to CLOSED");
        lc.transition(State::Draining, "deadline", 1_000, 1, 1, 0).unwrap();
        assert!(lc.transition(State::Closed, "flat", 1_001, 1, 1, 0).is_err(), "not flat");
        assert!(lc.transition(State::Closed, "flat", 1_001, 1, 0, 1).is_err(), "pending order");
        // restart: DRAINING restored, and a looser cap passed on restart is ignored
        let re = CohortLifecycle::load_or_init(&p, 999_999, i64::MAX, 0, 0, 5_000).unwrap();
        assert_eq!(re.state, State::Draining);
        assert_eq!(re.dispatch_cap, 3_000);
        assert_eq!(re.cutoff_reason(0, 5_000), None, "no re-evaluation once draining");
        let mut re = re;
        re.transition(State::Closed, "flat", 6_000, 1, 0, 0).unwrap();
        assert!(matches!(
            re.transition(State::Draining, "x", 7_000, 1, 0, 0),
            Err(LifecycleError::NotMonotonic { .. })
        ), "CLOSED is terminal");
        let fin = CohortLifecycle::load_or_init(&p, 1, 1, 0, 0, 0).unwrap();
        assert_eq!(fin.state, State::Closed);
        assert_eq!(fin.transitions.len(), 3);
    }
}
