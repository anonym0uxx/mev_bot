//! **The OsTune conformance battery — the test the Phase-B server task actually names.**
//!
//! # What was wrong before this file
//!
//! `README.md` and `docs/SERVER_BUILD_MANIFEST.md` §1 both told a Phase-B builder that the server
//! task is *"implement this trait and satisfy this named locked test, not design from scratch."*
//! The named locked test is `dossier_cpu_numa_tuning_cn_os_apply`, and it exercises `MockOs`.
//! **It passes with zero Windows code written.** The sentence was the most misleading in the
//! corpus for a builder that cannot cross-check, because it promised a mechanical acceptance
//! criterion that did not exist.
//!
//! Two structural gaps made "implement the trait" under-determined beyond that:
//!
//! * `apply_plan` calls only `set_affinity` and `set_priority`. `set_timer_res_ms` and
//!   `lock_region` had **no caller anywhere in the repository**, so a builder could return `Ok(0)`
//!   from both and every test in the workspace would stay green.
//! * `Mismatch` had no variant for either, so even a caller could not have reported them.
//!
//! `ostune_conformance` closes both. This file proves the battery works — that it passes a
//! faithful adapter, and, more importantly, that each specific dishonesty produces its own
//! specific failure. A battery nobody has watched fail is not evidence.
//!
//! # The obligation this file places on Phase B
//!
//! These tests run against mocks and therefore prove nothing about Windows. What they establish is
//! that the battery is sound, so that when the deploy box runs it against the real
//! `SetThreadGroupAffinity` / `SetPriorityClass` / `timeBeginPeriod` / `VirtualLock` adapter, a
//! green result means something. **`ConformanceReport::conformant()` must be true and the report
//! journaled before any tuned latency number may be claimed** — see `docs/OSTUNE_BUILD_SPEC.md`
//! and manifest §1's fail-closed clause.

use pump_quant_core::cpu_numa_tuning::*;

fn probe() -> ThreadId {
    ThreadId(0x00C0_FFEE)
}

fn honest() -> GroupAffinity {
    GroupAffinity {
        group: 0,
        mask: 0b0001,
    }
}

fn region() -> Vec<u8> {
    vec![0u8; 4096]
}

// ---------------------------------------------------------------------------
// A faithful adapter, and the four ways an adapter can lie.
// ---------------------------------------------------------------------------

/// A conforming adapter: honours every request, and *refuses* the impossible one.
#[derive(Default)]
struct HonestOs {
    granted_timer_ms: u32,
    lock_bytes: Option<usize>,
}

impl OsTune for HonestOs {
    fn set_affinity(&mut self, _t: ThreadId, aff: GroupAffinity) -> Result<GroupAffinity, OsErr> {
        // The defining behaviour of a real adapter: a mask naming no processor is
        // declined by the OS, so the adapter reports the decline rather than echoing.
        if aff.mask == 0 {
            return Err(OsErr::Denied);
        }
        Ok(aff)
    }
    fn set_priority(&mut self, _t: ThreadId, prio: Prio) -> Result<Prio, OsErr> {
        Ok(prio)
    }
    fn set_timer_res_ms(&mut self, ms: u32) -> Result<u32, OsErr> {
        Ok(if self.granted_timer_ms == 0 {
            ms
        } else {
            self.granted_timer_ms
        })
    }
    unsafe fn lock_region(&mut self, _p: *const u8, len: usize) -> Result<usize, OsErr> {
        Ok(self.lock_bytes.unwrap_or(len))
    }
}

/// The shape of a stub written to make the suite green: everything returns `Ok`
/// with the argument echoed back, including a request no OS could honour.
#[derive(Default)]
struct EchoingStub;

impl OsTune for EchoingStub {
    fn set_affinity(&mut self, _t: ThreadId, aff: GroupAffinity) -> Result<GroupAffinity, OsErr> {
        Ok(aff)
    }
    fn set_priority(&mut self, _t: ThreadId, prio: Prio) -> Result<Prio, OsErr> {
        Ok(prio)
    }
    fn set_timer_res_ms(&mut self, ms: u32) -> Result<u32, OsErr> {
        Ok(ms)
    }
    unsafe fn lock_region(&mut self, _p: *const u8, len: usize) -> Result<usize, OsErr> {
        Ok(len)
    }
}

/// An adapter that reports success and silently applies nothing — the failure mode
/// the whole read-back design exists to catch.
#[derive(Default)]
struct SilentNoOpOs;

impl OsTune for SilentNoOpOs {
    fn set_affinity(&mut self, _t: ThreadId, _aff: GroupAffinity) -> Result<GroupAffinity, OsErr> {
        Ok(GroupAffinity {
            group: 0,
            mask: 0xFFFF_FFFF_FFFF_FFFF,
        })
    }
    fn set_priority(&mut self, _t: ThreadId, _p: Prio) -> Result<Prio, OsErr> {
        Ok(Prio::Normal)
    }
    fn set_timer_res_ms(&mut self, _ms: u32) -> Result<u32, OsErr> {
        Ok(15) // the Windows default period, i.e. the request did nothing
    }
    unsafe fn lock_region(&mut self, _p: *const u8, _len: usize) -> Result<usize, OsErr> {
        Ok(0)
    }
}

/// An adapter with no privileges — `SeLockMemoryPrivilege` absent, affinity denied.
#[derive(Default)]
struct UnprivilegedOs;

impl OsTune for UnprivilegedOs {
    fn set_affinity(&mut self, _t: ThreadId, _a: GroupAffinity) -> Result<GroupAffinity, OsErr> {
        Err(OsErr::Denied)
    }
    fn set_priority(&mut self, _t: ThreadId, _p: Prio) -> Result<Prio, OsErr> {
        Err(OsErr::Denied)
    }
    fn set_timer_res_ms(&mut self, _ms: u32) -> Result<u32, OsErr> {
        Err(OsErr::Denied)
    }
    unsafe fn lock_region(&mut self, _p: *const u8, _len: usize) -> Result<usize, OsErr> {
        Err(OsErr::Denied)
    }
}

// ---------------------------------------------------------------------------
// The battery.
// ---------------------------------------------------------------------------

#[test]
fn a_faithful_adapter_is_conformant() {
    let mut os = HonestOs::default();
    let r = ostune_conformance(&mut os, probe(), honest(), 1, &region());
    assert!(
        r.conformant(),
        "a faithful adapter must pass every obligation; got {r:?}"
    );
    assert!(r.surface.is_empty());
}

/// **The anti-stub probe, and the reason the battery exists.** An empty affinity mask
/// names no processor. An adapter that answers `Ok(empty)` is echoing its argument
/// instead of reading the OS back — which is precisely what an implementation written
/// to make a suite green does. Note the stub passes obligations 1–4 cleanly: without
/// this probe it would be indistinguishable from a working Windows adapter.
#[test]
fn an_echoing_stub_is_caught_by_the_impossible_request() {
    let mut os = EchoingStub;
    let r = ostune_conformance(&mut os, probe(), honest(), 1, &region());
    assert!(!r.conformant(), "an echoing stub must not pass");
    assert_eq!(
        r.failures,
        vec![ConformanceFailure::EchoesImpossibleRequest],
        "the stub honours every honest request, so the impossible one is the ONLY \
         thing distinguishing it from a real adapter"
    );
}

/// The silent no-op is what `apply_plan`'s read-back verification was designed for;
/// the battery must catch it on all four methods at once, not just the two
/// `apply_plan` happens to call.
#[test]
fn a_silent_no_op_fails_every_obligation_including_the_two_apply_plan_never_calls() {
    let mut os = SilentNoOpOs;
    let r = ostune_conformance(&mut os, probe(), honest(), 1, &region());
    assert!(!r.conformant());
    for expected in [
        ConformanceFailure::AffinityNotHonoured,
        ConformanceFailure::PriorityNotHonoured,
        // These two are the point: `set_timer_res_ms` and `lock_region` have no
        // production caller anywhere, so before this battery a no-op there was
        // completely invisible to the test suite.
        ConformanceFailure::TimerResNotHonoured,
        ConformanceFailure::LockRegionShort,
    ] {
        assert!(
            r.failures.contains(&expected),
            "{expected:?} must be reported; got {:?}",
            r.failures
        );
    }
    // And the discrepancies are quantified, not merely flagged.
    assert!(r.surface.contains(&SurfaceMismatch::TimerRes {
        requested: 1,
        observed: 15
    }));
    assert!(r.surface.contains(&SurfaceMismatch::LockRegion {
        requested: 4096,
        observed: 0
    }));
}

/// Missing privileges must land in `errors`, not `failures` — the distinction matters
/// operationally. A refusal is "run it as administrator / grant
/// SeLockMemoryPrivilege"; a failure is "your adapter is wrong". Conflating them
/// sends a Phase-B builder to debug code when the answer is a privilege token.
#[test]
fn missing_privileges_are_errors_not_failures() {
    let mut os = UnprivilegedOs;
    let r = ostune_conformance(&mut os, probe(), honest(), 1, &region());
    assert!(!r.conformant());
    assert!(
        r.failures.is_empty(),
        "a denied request is not a dishonest adapter; got {:?}",
        r.failures
    );
    assert_eq!(r.errors.len(), 4, "all four methods denied: {:?}", r.errors);
    for (_, e) in &r.errors {
        assert_eq!(*e, OsErr::Denied);
    }
}

/// A coarser-than-requested timer grant is a REAL Windows behaviour (`timeBeginPeriod`
/// may grant less than asked) and must be reported with both numbers. A finer grant is
/// fine — the contract is "no coarser than requested", not "exactly requested".
#[test]
fn a_coarser_timer_grant_is_reported_with_both_numbers_and_a_finer_one_passes() {
    let mut coarse = HonestOs {
        granted_timer_ms: 4,
        ..Default::default()
    };
    let r = ostune_conformance(&mut coarse, probe(), honest(), 1, &region());
    assert!(r
        .failures
        .contains(&ConformanceFailure::TimerResNotHonoured));
    assert!(
        r.surface.contains(&SurfaceMismatch::TimerRes {
            requested: 4,
            observed: 4
        }) || r.surface.iter().any(|s| matches!(
            s,
            SurfaceMismatch::TimerRes {
                requested: 1,
                observed: 4
            }
        ))
    );

    let mut fine = HonestOs {
        granted_timer_ms: 1,
        ..Default::default()
    };
    let r2 = ostune_conformance(&mut fine, probe(), honest(), 4, &region());
    assert!(
        r2.conformant(),
        "a FINER grant than requested is not a violation: {r2:?}"
    );
}

/// A short `VirtualLock` is the normal failure when the working-set minimum is too
/// small. Rounding it up to success is how a system claims resident-memory latency it
/// does not have.
#[test]
fn a_short_page_lock_is_never_rounded_up_to_success() {
    let mut os = HonestOs {
        lock_bytes: Some(2048),
        ..Default::default()
    };
    let r = ostune_conformance(&mut os, probe(), honest(), 1, &region());
    assert!(r.failures.contains(&ConformanceFailure::LockRegionShort));
    assert!(r.surface.contains(&SurfaceMismatch::LockRegion {
        requested: 4096,
        observed: 2048
    }));
}

// ---------------------------------------------------------------------------
// RecordingOs — the impl the manifest claimed existed.
// ---------------------------------------------------------------------------

/// `docs/SERVER_BUILD_MANIFEST.md` §1 described "the `OsTune` trait with a
/// no-op/recording impl". No recording impl existed; only `MockOs`, a test double
/// with lying modes. `RecordingOs` is now that impl, and this pins what it records —
/// so a pin plan can be exercised end-to-end on a machine with no tuning privileges
/// and a plan defect is found before the deploy box rather than on it.
#[test]
fn the_recorder_captures_the_exact_call_sequence_apply_plan_makes() {
    let recs = vec![ProcRecord::core(0, 0b0011), ProcRecord::core(0, 0b1100)];
    let topo = parse_topology(&recs).expect("two disjoint cores parse");
    let plan = derive_plan(&topo, &[HotThreadSpec::test("decision")]).expect("one hot thread fits");

    let mut rec = RecordingOs::default();
    let report = apply_plan(&mut rec, &plan, Prio::High);

    assert!(
        report.mismatches.is_empty() && report.errors.is_empty(),
        "the recorder echoes, so apply_plan sees no discrepancy: {report:?}"
    );
    assert_eq!(rec.affinity_calls.len(), plan.assignments.len());
    assert_eq!(rec.priority_calls.len(), plan.assignments.len());
    for (i, &(th, aff)) in plan.assignments.iter().enumerate() {
        assert_eq!(rec.affinity_calls[i], (th, aff));
        assert_eq!(rec.priority_calls[i], (th, Prio::High));
    }

    // **The documented gap, asserted so it cannot be forgotten.** `apply_plan` never
    // touches the timer or page-locking surface. A Phase-B builder implementing only
    // what `apply_plan` calls would ship two unimplemented trait methods and see a
    // fully green suite. That is why `ostune_conformance` exists and why manifest §1
    // now requires it separately.
    assert!(
        rec.timer_calls.is_empty() && rec.lock_calls.is_empty(),
        "apply_plan is documented as covering affinity + priority ONLY; if this \
         changed, `docs/OSTUNE_BUILD_SPEC.md` §3 and manifest §1 must say so"
    );
}

/// The recorder must never be mistakable for a working adapter. It echoes, so the
/// impossible-request probe catches it — the same mechanism that catches a stub.
#[test]
fn the_recorder_is_deliberately_not_conformant() {
    let mut rec = RecordingOs::default();
    let r = ostune_conformance(&mut rec, probe(), honest(), 1, &region());
    assert!(
        !r.conformant(),
        "a recorder that passed conformance would let a build with NO tuning report \
         a tuned machine"
    );
    assert!(r
        .failures
        .contains(&ConformanceFailure::EchoesImpossibleRequest));
}

/// `MockOs::faithful()` is the existing dossier double. It echoes too, so it is also
/// non-conformant — recorded here so nobody wires it into a server acceptance run and
/// reads the green `apply_plan` report as an OsTune pass.
#[test]
fn the_dossier_mock_is_also_not_a_conformant_adapter() {
    let mut m = MockOs::faithful();
    let r = ostune_conformance(&mut m, probe(), honest(), 1, &region());
    assert!(!r.conformant());
    assert!(r
        .failures
        .contains(&ConformanceFailure::EchoesImpossibleRequest));
}

/// The battery is portable by construction — it must compile and run identically on
/// the laptop and the deploy box, because a battery that only runs on Windows cannot
/// be regression-tested in CI. This asserts it works through a `dyn` reference too,
/// which is how `apply_plan` takes its adapter.
#[test]
fn the_battery_runs_through_a_trait_object() {
    let mut os = HonestOs::default();
    let dynref: &mut dyn OsTune = &mut os;
    let r = ostune_conformance(dynref, probe(), honest(), 1, &region());
    assert!(r.conformant());
}
