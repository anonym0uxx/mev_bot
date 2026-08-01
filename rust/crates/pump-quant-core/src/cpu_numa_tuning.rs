//! CPU topology modelling, hot-thread pin-plan derivation, and mockable OS
//! application (constitution §57, performance-engineering law §24).
//!
//! The **decision logic is pure and fully portable**: raw processor records go in,
//! a validated [`Topology`] and a disjoint [`PinPlan`] come out, with no OS calls
//! anywhere in derivation. OS interaction is confined behind the mockable [`OsTune`]
//! trait, so the plan, its read-back verification, and the jitter aggregation all
//! run and are tested on any platform (the Windows `SetThreadGroupAffinity` /
//! `SetPriorityClass` / `timeBeginPeriod` / `VirtualLock` binding is the server's
//! implementation of the trait). No Linux-only syscalls (memory-locking or
//! CPU-affinity APIs) or procfs access appear here. All arithmetic is integer (§22).

/// A raw processor-relationship record: one physical core, its group, and the
/// bitmask of logical CPUs (SMT siblings) it owns within that group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcRecord {
    /// Processor group id.
    pub group: u16,
    /// Logical-CPU affinity mask within the group (popcount = SMT width).
    pub mask: u64,
    /// NUMA node the core belongs to.
    pub numa_node: u8,
    /// Efficiency class (higher = performance core; hot threads prefer these).
    pub efficiency_class: u8,
}

impl ProcRecord {
    /// A core record in `group` owning logical CPUs `mask` (default node/class 0).
    #[must_use]
    pub fn core(group: u16, mask: u64) -> Self {
        Self {
            group,
            mask,
            numa_node: 0,
            efficiency_class: 0,
        }
    }

    /// A core record with an explicit efficiency class.
    #[must_use]
    pub fn core_class(group: u16, mask: u64, efficiency_class: u8) -> Self {
        Self {
            group,
            mask,
            numa_node: 0,
            efficiency_class,
        }
    }
}

/// One physical core in the validated topology.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Core {
    /// Processor group.
    pub group: u16,
    /// Stable physical id (assignment order).
    pub physical_id: u16,
    /// Logical-CPU mask (SMT siblings).
    pub logical_mask: u64,
    /// SMT width (popcount of the mask).
    pub smt_siblings: u8,
    /// NUMA node.
    pub numa_node: u8,
    /// Efficiency class.
    pub efficiency_class: u8,
}

/// A validated CPU topology.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Topology {
    cores: Vec<Core>,
    /// Number of distinct physical cores.
    pub physical_cores: u16,
    /// Total logical CPUs across all cores.
    pub logical_cpus: u16,
}

impl Topology {
    /// Iterate the physical cores.
    pub fn all_cores(&self) -> impl Iterator<Item = &Core> {
        self.cores.iter()
    }

    /// The union mask of every logical CPU in `group`.
    #[must_use]
    pub fn group_mask(&self, group: u16) -> u64 {
        self.cores
            .iter()
            .filter(|c| c.group == group)
            .fold(0u64, |m, c| m | c.logical_mask)
    }
}

/// Why a set of processor records is not a valid topology.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TopoErr {
    /// A record carried an empty (zero) mask.
    EmptyMask,
    /// Two records in the same group claimed overlapping logical CPUs.
    OverlappingMasks,
}

/// Parse raw records into a validated topology.
///
/// Each record is one physical core; within a group the masks must be disjoint and
/// non-empty. `physical_cores` counts records; `logical_cpus` sums the popcounts.
pub fn parse_topology(records: &[ProcRecord]) -> Result<Topology, TopoErr> {
    use std::collections::BTreeMap;
    let mut per_group: BTreeMap<u16, u64> = BTreeMap::new();
    let mut cores = Vec::with_capacity(records.len());
    let mut logical: u32 = 0;

    for (i, r) in records.iter().enumerate() {
        if r.mask == 0 {
            return Err(TopoErr::EmptyMask);
        }
        let acc = per_group.entry(r.group).or_insert(0);
        if *acc & r.mask != 0 {
            return Err(TopoErr::OverlappingMasks);
        }
        *acc |= r.mask;
        logical += r.mask.count_ones();
        cores.push(Core {
            group: r.group,
            physical_id: i as u16,
            logical_mask: r.mask,
            smt_siblings: r.mask.count_ones() as u8,
            numa_node: r.numa_node,
            efficiency_class: r.efficiency_class,
        });
    }

    Ok(Topology {
        physical_cores: cores.len() as u16,
        logical_cpus: logical as u16,
        cores,
    })
}

/// Identifier of a thread to pin.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ThreadId(pub u64);

/// A specification for a hot thread that must own a physical core.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HotThreadSpec {
    id: ThreadId,
}

impl HotThreadSpec {
    /// A spec whose id is derived deterministically from a name (FNV-1a) — used by
    /// tests and by callers that key threads by role name.
    #[must_use]
    pub fn test(name: &str) -> Self {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for &b in name.as_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        Self { id: ThreadId(h) }
    }

    /// A spec with an explicit thread id.
    #[must_use]
    pub fn with_id(id: ThreadId) -> Self {
        Self { id }
    }

    /// The thread id.
    #[must_use]
    pub fn thread_id(&self) -> ThreadId {
        self.id
    }
}

/// A single-CPU affinity for a hot thread, within one group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GroupAffinity {
    /// Processor group.
    pub group: u16,
    /// Single-bit affinity mask.
    pub mask: u64,
}

/// A multi-CPU mask within one group (control / reserved-idle sets).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GroupMask {
    /// Processor group.
    pub group: u16,
    /// Affinity mask.
    pub mask: u64,
}

/// A derived pin plan: which thread pins where, plus the reserved-idle SMT siblings
/// and the control CPUs left for tokio/OS.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PinPlan {
    /// One single-CPU assignment per hot thread, each on a distinct physical core.
    pub assignments: Vec<(ThreadId, GroupAffinity)>,
    /// Every remaining CPU (never intersects hot or reserved-idle).
    pub control_mask: GroupMask,
    /// SMT siblings of hot cores, assigned to nothing.
    pub reserved_idle: GroupMask,
}

/// Why a pin plan cannot be derived.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanError {
    /// Fewer distinct physical cores than hot threads.
    Insufficient,
    /// The hot set could not stay within a single processor group.
    MultiGroupSpan,
}

/// Derive the hot-thread pin plan. Pure function of `(Topology, hot)`.
///
/// Each hot thread gets one logical CPU on a distinct physical core (highest
/// efficiency class first); the other logical CPUs of those cores become
/// reserved-idle; everything else is the control mask. The three sets are pairwise
/// disjoint. Hot threads are kept within a single group (no group-spanning affinity).
pub fn derive_plan(t: &Topology, hot: &[HotThreadSpec]) -> Result<PinPlan, PlanError> {
    if (t.physical_cores as usize) < hot.len() {
        return Err(PlanError::Insufficient);
    }
    if hot.is_empty() {
        // No hot threads: control is everything in the first core's group (or 0).
        let group = t.cores.first().map(|c| c.group).unwrap_or(0);
        return Ok(PinPlan {
            assignments: Vec::new(),
            control_mask: GroupMask {
                group,
                mask: t.group_mask(group),
            },
            reserved_idle: GroupMask { group, mask: 0 },
        });
    }

    // Rank cores: prefer higher efficiency class, then stable by physical id.
    let mut cores: Vec<&Core> = t.all_cores().collect();
    cores.sort_by(|a, b| {
        b.efficiency_class
            .cmp(&a.efficiency_class)
            .then(a.physical_id.cmp(&b.physical_id))
    });

    let group = cores[0].group;
    let mut assignments = Vec::with_capacity(hot.len());
    let mut hot_mask = 0u64;
    let mut idle_mask = 0u64;

    for (spec, core) in hot.iter().zip(cores.iter()) {
        if core.group != group {
            return Err(PlanError::MultiGroupSpan);
        }
        // Lowest set bit of the core's mask: one logical CPU for the hot thread.
        let one = core.logical_mask & core.logical_mask.wrapping_neg();
        assignments.push((spec.thread_id(), GroupAffinity { group, mask: one }));
        hot_mask |= one;
        idle_mask |= core.logical_mask & !one; // SMT siblings -> reserved idle
    }

    let control_bits = t.group_mask(group) & !hot_mask & !idle_mask;

    Ok(PinPlan {
        assignments,
        control_mask: GroupMask {
            group,
            mask: control_bits,
        },
        reserved_idle: GroupMask {
            group,
            mask: idle_mask,
        },
    })
}

/// Thread priority class (mirrors the Windows priority classes the server binds to).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prio {
    /// Normal scheduling.
    Normal,
    /// Elevated priority for a hot thread.
    High,
    /// Time-critical (use sparingly).
    Realtime,
}

/// An OS-adapter error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OsErr {
    /// The OS refused the request.
    Denied,
    /// The target thread/handle was invalid.
    NotFound,
}

/// The mockable OS-tuning surface. The server implements this over the Windows
/// APIs; tests implement it with a mock. Every setter returns the OS's *observed*
/// value so the caller can verify the request actually took effect.
///
/// SERVER (Phase-B) TODO — see `docs/SERVER_BUILD_MANIFEST.md` task #1: the real
/// `impl OsTune` over `SetThreadGroupAffinity` / `SetPriorityClass` /
/// `timeBeginPeriod` / `VirtualLock` is a deployment-box deliverable, intentionally
/// absent on the laptop. Only `MockOs` exists here. The contract the Windows impl
/// must satisfy is locked by `dossier_cpu_numa_tuning_cn_os_apply`.
pub trait OsTune {
    /// Set thread affinity; returns the affinity the OS reports afterwards.
    fn set_affinity(&mut self, th: ThreadId, aff: GroupAffinity) -> Result<GroupAffinity, OsErr>;
    /// Set thread priority; returns the priority the OS reports afterwards.
    fn set_priority(&mut self, th: ThreadId, prio: Prio) -> Result<Prio, OsErr>;
    /// Set the global timer resolution (ms); returns the granted resolution.
    fn set_timer_res_ms(&mut self, ms: u32) -> Result<u32, OsErr>;
    /// Lock a memory region resident; returns the number of bytes locked.
    ///
    /// # Safety
    /// `ptr` must be the start of, and `len` must lie entirely within, a single
    /// committed region owned by this process. That region must remain allocated
    /// until either `unlock_region` is called for it or this adapter is dropped,
    /// whichever happens first. The obligation is NOT bound to a returned value:
    /// this method returns the ALIGNED byte count, and the adapter owns the unlock.
    unsafe fn lock_region(&mut self, ptr: *const u8, len: usize) -> Result<usize, OsErr>;
}

/// A single discrepancy between a requested and an observed OS setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mismatch {
    /// Affinity read back different from requested.
    Affinity {
        /// What was requested.
        requested: GroupAffinity,
        /// What the OS reported.
        observed: GroupAffinity,
    },
    /// Priority read back different from requested.
    Priority {
        /// What was requested.
        requested: Prio,
        /// What the OS reported.
        observed: Prio,
    },
}

/// The outcome of applying a plan: what took effect, what silently didn't, and hard
/// errors — nothing is assumed applied.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ApplyReport {
    /// Threads whose affinity + priority verified.
    pub applied: Vec<ThreadId>,
    /// Read-back discrepancies (a silent OS no-op surfaces here, never trusted).
    pub mismatches: Vec<Mismatch>,
    /// Hard adapter errors.
    pub errors: Vec<(ThreadId, OsErr)>,
}

/// Apply a pin plan through an [`OsTune`], verifying every setting by read-back.
///
/// Affinity then priority is applied per hot thread; a returned value that differs
/// from the request is recorded as a [`Mismatch`] (never dropped), and a hard error
/// is recorded without aborting the rest of the plan.
pub fn apply_plan(os: &mut dyn OsTune, plan: &PinPlan, prio: Prio) -> ApplyReport {
    let mut report = ApplyReport::default();
    for &(th, aff) in &plan.assignments {
        let mut ok = true;
        match os.set_affinity(th, aff) {
            Ok(observed) if observed == aff => {}
            Ok(observed) => {
                ok = false;
                report.mismatches.push(Mismatch::Affinity {
                    requested: aff,
                    observed,
                });
            }
            Err(e) => {
                ok = false;
                report.errors.push((th, e));
            }
        }
        match os.set_priority(th, prio) {
            Ok(observed) if observed == prio => {}
            Ok(observed) => {
                ok = false;
                report.mismatches.push(Mismatch::Priority {
                    requested: prio,
                    observed,
                });
            }
            Err(e) => {
                ok = false;
                report.errors.push((th, e));
            }
        }
        if ok {
            report.applied.push(th);
        }
    }
    report
}

/// A test mock of [`OsTune`] with configurable faithfulness.
#[derive(Clone, Copy, Debug, Default)]
pub struct MockOs {
    lie_affinity: bool,
    lie_priority: bool,
}

impl MockOs {
    /// A mock that reports back exactly what was requested.
    #[must_use]
    pub fn faithful() -> Self {
        Self {
            lie_affinity: false,
            lie_priority: false,
        }
    }

    /// A mock that silently ignores affinity requests (reports a wrong value).
    #[must_use]
    pub fn returns_wrong_affinity() -> Self {
        Self {
            lie_affinity: true,
            lie_priority: false,
        }
    }

    /// A mock that silently ignores priority requests.
    #[must_use]
    pub fn returns_wrong_priority() -> Self {
        Self {
            lie_affinity: false,
            lie_priority: true,
        }
    }
}

impl OsTune for MockOs {
    fn set_affinity(&mut self, _th: ThreadId, aff: GroupAffinity) -> Result<GroupAffinity, OsErr> {
        if self.lie_affinity {
            // Report a different mask (flip the low bit) to model a silent no-op.
            Ok(GroupAffinity {
                group: aff.group,
                mask: aff.mask ^ 1,
            })
        } else {
            Ok(aff)
        }
    }

    fn set_priority(&mut self, _th: ThreadId, prio: Prio) -> Result<Prio, OsErr> {
        if self.lie_priority {
            Ok(Prio::Normal)
        } else {
            Ok(prio)
        }
    }

    fn set_timer_res_ms(&mut self, ms: u32) -> Result<u32, OsErr> {
        Ok(ms)
    }

    unsafe fn lock_region(&mut self, _ptr: *const u8, len: usize) -> Result<usize, OsErr> {
        Ok(len)
    }
}

/// Scheduling-jitter statistics over a batch of inter-tick deltas (ns).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JitterStats {
    /// Median delta.
    pub p50_ns: u64,
    /// 99th-percentile delta.
    pub p99_ns: u64,
    /// 99.9th-percentile delta.
    pub p999_ns: u64,
    /// Maximum delta.
    pub max_ns: u64,
    /// Sample count.
    pub n: u32,
}

impl JitterStats {
    /// The empty result — no samples, nothing fabricated.
    #[must_use]
    pub fn missing() -> Self {
        Self {
            p50_ns: 0,
            p99_ns: 0,
            p999_ns: 0,
            max_ns: 0,
            n: 0,
        }
    }

    /// Whether there were no samples.
    #[must_use]
    pub fn is_missing(&self) -> bool {
        self.n == 0
    }
}

/// Aggregate scheduling-jitter deltas into percentiles by nearest-rank.
///
/// Pure aggregation (the pinned spin-loop sampler lives in the bench harness). The
/// nearest-rank index for percentile `num/den` over `n` samples is
/// `ceil(num*n/den) - 1`, clamped into range.
#[must_use]
pub fn jitter_stats(deltas_ns: &[u64]) -> JitterStats {
    if deltas_ns.is_empty() {
        return JitterStats::missing();
    }
    let mut sorted = deltas_ns.to_vec();
    sorted.sort_unstable();
    let n = sorted.len() as u64;
    let nearest = |num: u64, den: u64| -> u64 {
        // nearest-rank: ceil(num*n/den), then to a 0-based index, clamped in range.
        let idx = (num * n).div_ceil(den).saturating_sub(1).min(n - 1);
        sorted[idx as usize]
    };
    JitterStats {
        p50_ns: nearest(50, 100),
        p99_ns: nearest(99, 100),
        p999_ns: nearest(999, 1_000),
        max_ns: sorted[(n - 1) as usize],
        n: sorted.len() as u32,
    }
}

// ===========================================================================
// PHASE-B CONFORMANCE — added 2026-07-29.
//
// # The problem this section exists to fix
//
// `docs/SERVER_BUILD_MANIFEST.md` §1 and `README.md` both told a Phase-B builder
// that the server task is *"implement this trait and satisfy this named locked
// test."* The named locked test is `dossier_cpu_numa_tuning_cn_os_apply`, and it
// exercises `MockOs`. **It passes with zero Windows code written.** A builder that
// implemented `OsTune` badly — or not at all — got a green suite either way.
//
// Two further gaps made "implement the trait" under-determined:
//
// * `apply_plan` calls only `set_affinity` and `set_priority`. `set_timer_res_ms`
//   and `lock_region` have NO caller anywhere in the repository, so a builder could
//   implement them as `Ok(0)` and nothing would notice.
// * `Mismatch` had no variant for either, so even a caller could not report them.
//
// [`ostune_conformance`] is the missing authority: a portable, allocation-light
// battery that any `impl OsTune` must pass, exercising **all four** trait methods
// and the read-back-verification contract itself. It runs against `MockOs` here
// (proving the battery detects a lying adapter) and MUST be run against the Windows
// adapter on the deploy box, with its report journaled. See
// `docs/OSTUNE_BUILD_SPEC.md`.
// ===========================================================================

/// Discrepancies for the two trait methods `apply_plan` never exercised.
///
/// Split from [`Mismatch`] rather than added to it, because `Mismatch` is named in
/// the SHA-locked dossier signature for `cn_os_apply` and its shape is contractual.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfaceMismatch {
    /// The granted timer resolution differs from the request. On Windows
    /// `timeBeginPeriod` may grant a coarser period than asked; that is a real
    /// operational fact and must be reported, never rounded away.
    TimerRes {
        /// Milliseconds requested.
        requested: u32,
        /// Milliseconds the OS granted.
        observed: u32,
    },
    /// Fewer bytes were locked resident than requested. On Windows `VirtualLock`
    /// is bounded by the process working-set minimum and by
    /// `SeLockMemoryPrivilege`; a short lock is the normal failure and silently
    /// treating it as success is how a system claims tuned latency it does not have.
    LockRegion {
        /// Bytes requested.
        requested: usize,
        /// Bytes actually locked.
        observed: usize,
    },
}

/// Which conformance obligation an adapter failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConformanceFailure {
    /// `set_affinity` returned success but read back a different affinity.
    AffinityNotHonoured,
    /// `set_priority` returned success but read back a different priority.
    PriorityNotHonoured,
    /// `set_timer_res_ms` granted a period coarser than requested.
    TimerResNotHonoured,
    /// `lock_region` locked fewer bytes than requested.
    LockRegionShort,
    /// The adapter reported success for a request it cannot possibly have honoured
    /// — an affinity mask with no bits set. An adapter that answers `Ok` here is
    /// not read-back-verifying; it is echoing. **This is the check that catches a
    /// stub written to make the suite green.**
    EchoesImpossibleRequest,
}

/// The outcome of [`ostune_conformance`]. `failures` empty means the adapter
/// honoured every obligation on this machine, with this privilege set, right now.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConformanceReport {
    /// Obligations the adapter failed.
    pub failures: Vec<ConformanceFailure>,
    /// Hard adapter errors, paired with the obligation being probed.
    pub errors: Vec<(ConformanceFailure, OsErr)>,
    /// Surface discrepancies observed while probing (informational: a coarser timer
    /// grant is recorded here AND as a failure, so the report is self-contained).
    pub surface: Vec<SurfaceMismatch>,
}

impl ConformanceReport {
    /// True iff the adapter honoured every obligation. **A Phase-B build may not
    /// claim tuned latency numbers unless this is true and the report is journaled.**
    #[must_use]
    pub fn conformant(&self) -> bool {
        self.failures.is_empty() && self.errors.is_empty()
    }
}

/// The affinity a conforming adapter MUST refuse: a mask with no bits set names no
/// processor and cannot be honoured by any OS.
#[must_use]
pub const fn impossible_affinity() -> GroupAffinity {
    GroupAffinity { group: 0, mask: 0 }
}

/// The conformance battery every `impl OsTune` must pass.
///
/// `probe` is a real thread id on the target and `region` a real resident buffer;
/// on the deploy box pass the hot decision thread and the hot-set arena, so the
/// battery measures the thing that actually matters rather than a scratch case.
///
/// **What it proves and what it does not.** It proves the adapter honours what it
/// reports, exercises all four trait methods, and refuses a request it cannot
/// satisfy. It does NOT prove the pin plan improved latency — that is the jitter
/// probe's job (`jitter_stats`), and the two are separate evidence.
///
/// Integer-only, no clock, no formatting (§22, and this crate is in the enforced
/// hot + money lint scope).
pub fn ostune_conformance<T: OsTune + ?Sized>(
    os: &mut T,
    probe: ThreadId,
    honest_affinity: GroupAffinity,
    timer_ms: u32,
    region: &[u8],
) -> ConformanceReport {
    let mut r = ConformanceReport::default();

    // 1. Affinity is honoured, by read-back and not by return code.
    match os.set_affinity(probe, honest_affinity) {
        Ok(observed) if observed == honest_affinity => {}
        Ok(_) => r.failures.push(ConformanceFailure::AffinityNotHonoured),
        Err(e) => r.errors.push((ConformanceFailure::AffinityNotHonoured, e)),
    }

    // 2. Priority is honoured.
    match os.set_priority(probe, Prio::High) {
        Ok(Prio::High) => {}
        Ok(_) => r.failures.push(ConformanceFailure::PriorityNotHonoured),
        Err(e) => r.errors.push((ConformanceFailure::PriorityNotHonoured, e)),
    }

    // 3. Timer resolution: a coarser grant is a real fact, reported both ways.
    match os.set_timer_res_ms(timer_ms) {
        Ok(observed) if observed <= timer_ms => {}
        Ok(observed) => {
            r.surface.push(SurfaceMismatch::TimerRes {
                requested: timer_ms,
                observed,
            });
            r.failures.push(ConformanceFailure::TimerResNotHonoured);
        }
        Err(e) => r.errors.push((ConformanceFailure::TimerResNotHonoured, e)),
    }

    // 4. Page-locking: a short lock is the normal failure and must never be
    //    rounded up to success.
    // SAFETY: `region` is a `&[u8]` slice that is live for the duration of
    // this call. The pointer and length come directly from the slice's
    // invariants, so `ptr` is valid and `len` is within bounds. The region
    // remains allocated because the borrow keeps it alive.
    match unsafe { os.lock_region(region.as_ptr(), region.len()) } {
        Ok(observed) if observed >= region.len() => {}
        Ok(observed) => {
            r.surface.push(SurfaceMismatch::LockRegion {
                requested: region.len(),
                observed,
            });
            r.failures.push(ConformanceFailure::LockRegionShort);
        }
        Err(e) => r.errors.push((ConformanceFailure::LockRegionShort, e)),
    }

    // 5. **The anti-stub probe.** An empty mask names no processor. An adapter that
    //    returns `Ok(empty)` is echoing its argument rather than reading the OS
    //    back, which is exactly what a hurried or synthetic implementation does.
    //    `Err` is the correct answer; so is `Ok(something_else)`, because that is a
    //    genuine read-back of a request the OS declined.
    let imp = impossible_affinity();
    if let Ok(observed) = os.set_affinity(probe, imp) {
        if observed == imp {
            r.failures.push(ConformanceFailure::EchoesImpossibleRequest);
        }
    }

    r
}

/// The no-op **recording** adapter `docs/SERVER_BUILD_MANIFEST.md` §1 has always
/// claimed exists ("the `OsTune` trait with a no-op/recording impl") and which,
/// until 2026-07-29, did not.
///
/// It applies nothing and records every request in order. Its purpose is to let the
/// pin plan be exercised end-to-end on a machine with no tuning privileges — a
/// laptop, CI, a container — so a plan defect is found before the deploy box, and
/// so `apply_plan`'s call sequence is itself assertable.
///
/// **It is deliberately NOT conformant.** `set_affinity` returns the requested value
/// unchanged, so [`ostune_conformance`] flags `EchoesImpossibleRequest` against it.
/// That is the point: a recorder must never be mistakable for a working adapter, and
/// a build that wires this in and reports a green conformance run has a bug in the
/// battery, not a tuned machine.
#[derive(Clone, Debug, Default)]
pub struct RecordingOs {
    /// Affinity requests, in call order.
    pub affinity_calls: Vec<(ThreadId, GroupAffinity)>,
    /// Priority requests, in call order.
    pub priority_calls: Vec<(ThreadId, Prio)>,
    /// Timer-resolution requests, in call order.
    pub timer_calls: Vec<u32>,
    /// Lock requests as `len` only (the pointer is not retained — recording a raw
    /// address would outlive its provenance and serves no assertion).
    pub lock_calls: Vec<usize>,
}

impl OsTune for RecordingOs {
    fn set_affinity(&mut self, th: ThreadId, aff: GroupAffinity) -> Result<GroupAffinity, OsErr> {
        self.affinity_calls.push((th, aff));
        Ok(aff)
    }
    fn set_priority(&mut self, th: ThreadId, prio: Prio) -> Result<Prio, OsErr> {
        self.priority_calls.push((th, prio));
        Ok(prio)
    }
    fn set_timer_res_ms(&mut self, ms: u32) -> Result<u32, OsErr> {
        self.timer_calls.push(ms);
        Ok(ms)
    }
    unsafe fn lock_region(&mut self, _ptr: *const u8, len: usize) -> Result<usize, OsErr> {
        self.lock_calls.push(len);
        Ok(len)
    }
}
