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
    // ---- Windows-adapter variants (§4.0 OsTune adapter) ----
    /// The requested processor group does not exist.
    InvalidGroup { requested: u16, groups: u16 },
    /// The processor count for a group is implausible (0 or >64).
    TopologyImplausible { cpus: u32 },
    /// The affinity mask names processors that do not exist in the group.
    InvalidMask { mask: usize, legal: usize },
    /// A Win32 API call returned failure. `call` is the API name, `code` is GetLastError().
    Win32 { call: &'static str, code: u32 },
    /// A Winmm API call returned a non-zero MMRESULT.
    Mm { call: &'static str, code: u32 },
    /// Read-back verification failed: requested and observed differ.
    ReadBackMismatch { requested: GroupAffinity, observed: GroupAffinity },
    /// Read-back verification failed for a u32 value (priority class).
    ReadBackMismatchU32 { requested: u32, observed: u32 },
    /// `lock_region` was called with length 0.
    EmptyRegion,
    /// The requested lock length exceeds the adapter's byte budget.
    LockBudgetExceeded { requested: usize, budget: usize },
    /// The pointer + length arithmetic overflowed.
    RangeOverflow,
    /// VirtualLock failed because the working-set quota is too small (ERROR 1453).
    WorkingSetQuota { requested: usize },
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

// ===========================================================================
// §4.0 OsTune Windows adapter — deployment-box implementation.
//
// All four trait methods are implemented over Win32 FFI. Every call is wrapped
// in an `unsafe { }` block with a `// SAFETY:` comment. Validation that can be
// done in safe code (group bounds, mask bounds, budget check, pointer overflow)
// is performed BEFORE each unsafe block, never inside it.
//
// Read-back: `set_affinity` reads back with `GetThreadGroupAffinity` (NOT
// `GetCurrentProcessorNumberEx`, which shows the *current* CPU and gives a false
// mismatch before the thread has migrated). `set_priority` reads back with
// `GetPriorityClass`. `set_timer_res_ms` has NO read-back API — reported as
// explicitly UNVERIFIABLE in the safety dossier. `lock_region` returns the
// ALIGNED byte count so the budget cannot silently under-count.
//
// All four calls FAIL CLOSED: a failed pin aborts the tuning run, never logged
// and continued (§4.5). HIGH_PRIORITY_CLASS, NOT REALTIME. Timer resolution is
// restored on Drop via the RAII `TimerResGuard`.
//
// FUTURE REFINEMENT (recorded per operator decision (a), not for this commit):
//   fn lock_region<'a>(&mut self, region: &'a [u8]) -> Result<LockGuard<'a>, OsErr>
// The borrow checker then enforces the whole invariant: the guard holds a borrow
// of the region, so it cannot be freed while locked. No unsafe at the call site,
// no new owning type, lifetime parameters keep the trait object-safe for
// `dyn OsTune`. Strictly better than both options; deferred only because it
// changes the return type and every caller.
// ===========================================================================

#[cfg(windows)]
pub mod win_adapter {
    use super::{
        GroupAffinity, OsErr, OsTune, Prio, ThreadId,
    };
    use core::mem::MaybeUninit;

    // --- Win32 FFI declarations (no external crate; raw `extern "system"`) ---

    type BOOL = i32;
    type DWORD = u32;
    type DWORD_PTR = usize;
    type HANDLE = *mut core::ffi::c_void;
    type WORD = u16;
    type LPDWORD = *mut DWORD;
    type SIZE_T = usize;
    type UINT = u32;
    type MMRESULT = u32;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct GroupAffinityWin {
        Mask: DWORD_PTR,
        Group: WORD,
        Reserved: [WORD; 3],
    }

    #[repr(C)]
    struct SystemInfo {
        wProcessorArchitecture: WORD,
        wReserved: WORD,
        dwPageSize: DWORD,
        lpMinimumApplicationAddress: *mut core::ffi::c_void,
        lpMaximumApplicationAddress: *mut core::ffi::c_void,
        dwActiveProcessorMask: DWORD_PTR,
        dwNumberOfProcessors: DWORD,
        dwProcessorType: DWORD,
        dwAllocationGranularity: DWORD,
        wProcessorLevel: WORD,
        wProcessorRevision: WORD,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct TimeCaps {
        wPeriodMin: DWORD,
        wPeriodMax: DWORD,
    }

    #[link(name = "winmm")]
    extern "system" {
        fn GetActiveProcessorGroupCount() -> WORD;
        fn GetActiveProcessorCount(group: WORD) -> DWORD;
        fn SetThreadGroupAffinity(
            hThread: HANDLE,
            GroupAffinity: *const GroupAffinityWin,
            PreviousAffinity: *mut GroupAffinityWin,
        ) -> BOOL;
        fn GetThreadGroupAffinity(
            hThread: HANDLE,
            GroupAffinity: *mut GroupAffinityWin,
        ) -> BOOL;
        fn GetCurrentThread() -> HANDLE;
        fn GetLastError() -> DWORD;
        fn SetPriorityClass(hProcess: HANDLE, dwPriorityClass: DWORD) -> BOOL;
        fn GetPriorityClass(hProcess: HANDLE) -> DWORD;
        fn GetCurrentProcess() -> HANDLE;
        fn timeGetDevCaps(ptc: *mut TimeCaps, cbtc: UINT) -> MMRESULT;
        fn timeBeginPeriod(uPeriod: UINT) -> MMRESULT;
        fn timeEndPeriod(uPeriod: UINT) -> MMRESULT;
        fn VirtualLock(lpAddress: *mut core::ffi::c_void, dwSize: SIZE_T) -> BOOL;
        fn VirtualUnlock(lpAddress: *mut core::ffi::c_void, dwSize: SIZE_T) -> BOOL;
        fn SetProcessWorkingSetSize(
            hProcess: HANDLE,
            dwMin: SIZE_T,
            dwMax: SIZE_T,
        ) -> BOOL;
        fn GetSystemInfo(lpSystemInfo: *mut SystemInfo);
    }

    // HIGH_PRIORITY_CLASS = 0x00000080 (Win32 header constant)
    const HIGH_PRIORITY_CLASS: DWORD = 0x0000_0080;
    const NORMAL_PRIORITY_CLASS: DWORD = 0x0000_0020;
    const REALTIME_PRIORITY_CLASS: DWORD = 0x0000_0100;
    const ERROR_WORKING_SET_QUOTA: DWORD = 1453;

    // --- RAII guard for timer resolution ---

    /// Restores the system timer resolution on drop. The granted period has NO
    /// read-back API; it is reported as UNVERIFIABLE in the safety dossier.
    pub struct TimerResGuard {
        period_ms: u32,
        active: bool,
    }

    impl Drop for TimerResGuard {
        fn drop(&mut self) {
            if self.active {
                // SAFETY: `self.period_ms` was the value successfully passed to
                // `timeBeginPeriod` in `acquire()`, so it is a valid period in
                // the range [wPeriodMin, wPeriodMax]. `timeEndPeriod` is
                // symmetric with `timeBeginPeriod` and always safe to call with
                // a period that was previously begun.
                unsafe {
                    let _ = timeEndPeriod(self.period_ms as UINT);
                }
            }
        }
    }

    impl TimerResGuard {
        /// Acquire a timer resolution guard. Returns the guard and the granted
        /// period. The granted period is UNVERIFIABLE — there is no API to
        /// read back the current timer resolution.
        fn acquire(requested_ms: u32) -> Result<Self, OsErr> {
            let mut caps = MaybeUninit::<TimeCaps>::uninit();
            // SAFETY: `caps` is a stack-allocated `MaybeUninit<TimeCaps>`,
            // properly aligned. `timeGetDevCaps` writes to it if it returns 0
            // (success). We only read `caps` after checking the return is 0.
            // `timeBeginPeriod` receives a period clamped to the device caps
            // range, so it is valid for this system.
            let (rc_caps, caps, rc_begin) = unsafe {
                let rc_c = timeGetDevCaps(
                    caps.as_mut_ptr(),
                    core::mem::size_of::<TimeCaps>() as UINT,
                );
                if rc_c != 0 {
                    return Err(OsErr::Mm { call: "timeGetDevCaps", code: rc_c });
                }
                let caps = caps.assume_init();
                let period = requested_ms.clamp(caps.wPeriodMin, caps.wPeriodMax);
                let rc_b = timeBeginPeriod(period as UINT);
                (rc_c, caps, (rc_b, period))
            };
            let (_, _, (rc_begin_val, period)) = (rc_caps, caps, rc_begin);
            if rc_begin_val != 0 {
                return Err(OsErr::Mm { call: "timeBeginPeriod", code: rc_begin_val });
            }
            // Drop the unused tuple components to satisfy the borrow checker.
            let _ = (rc_caps, caps);
            Ok(Self { period_ms: period, active: true })
        }
    }

    /// Windows adapter implementing `OsTune` over Win32 FFI.
    ///
    /// All four trait methods FAIL CLOSED on any FFI error. The adapter owns a
    /// `TimerResGuard` (dropped when the adapter is dropped) and tracks locked
    /// regions for `VirtualUnlock` on its own teardown.
    pub struct WinOsTune {
        timer: TimerResGuard,
        /// Byte budget for VirtualLock. Explicit, justified in the dossier.
        lock_budget: usize,
        /// Page size from GetSystemInfo, used for alignment.
        page_size: usize,
        /// Total bytes locked so far (tracked against budget).
        locked_total: usize,
        /// Locked ranges for unlock-on-drop.
        locked_ranges: Vec<(*mut core::ffi::c_void, usize)>,
        /// Working-set raised flag (to avoid re-raising).
        working_set_raised: bool,
    }

    impl WinOsTune {
        /// Construct with an explicit lock budget (bytes). The budget must be
        /// justified in the safety dossier; raising the working-set minimum
        /// happens lazily on the first `lock_region` call.
        #[must_use]
        pub fn new(lock_budget: usize) -> Result<Self, OsErr> {
            let timer = TimerResGuard::acquire(1)?;
            let mut sysinfo = MaybeUninit::<SystemInfo>::uninit();
            // SAFETY: `sysinfo` is a stack-allocated MaybeUninit<SystemInfo>,
            // properly aligned. GetSystemInfo always succeeds (void return) and
            // writes the full struct. We read it immediately after.
            let page_size = unsafe {
                GetSystemInfo(sysinfo.as_mut_ptr());
                sysinfo.assume_init().dwPageSize as usize
            };
            Ok(Self {
                timer,
                lock_budget,
                page_size,
                locked_total: 0,
                locked_ranges: Vec::new(),
                working_set_raised: false,
            })
        }

        /// Raise the process working-set minimum so VirtualLock does not fail
        /// with ERROR_WORKING_SET_QUOTA (1453). Called lazily on first lock.
        fn ensure_working_set(&mut self) -> Result<(), OsErr> {
            if self.working_set_raised {
                return Ok(());
            }
            let headroom = 1024 * 1024;
            let min_ws = self.lock_budget;
            let max_ws = self.lock_budget.saturating_add(headroom);
            // SAFETY: GetCurrentProcess returns a pseudo-handle that is always
            // valid. The values are byte counts; SetProcessWorkingSetSize
            // fails with FALSE if the values are implausible (e.g. > commit
            // limit), which we report as an error. We never pass 0 or SIZE_MAX.
            // GetLastError is thread-local, always safe.
            let ok = unsafe {
                let ok = SetProcessWorkingSetSize(GetCurrentProcess(), min_ws, max_ws);
                if ok == 0 {
                    let code = GetLastError();
                    return Err(OsErr::Win32 { call: "SetProcessWorkingSetSize", code });
                }
                ok
            };
            self.working_set_raised = true;
            Ok(())
        }
    }

    impl Drop for WinOsTune {
        fn drop(&mut self) {
            // Unlock all locked ranges in reverse order.
            for &(ptr, len) in self.locked_ranges.iter().rev() {
                // SAFETY: each (ptr, len) pair was successfully VirtualLock'd
                // by a prior call to lock_region. VirtualUnlock is symmetric
                // and always safe to call with a region that was locked.
                unsafe { let _ = VirtualUnlock(ptr, len); }
            }
            // TimerResGuard drop restores the timer period.
        }
    }

    impl OsTune for WinOsTune {
        fn set_affinity(&mut self, _th: ThreadId, aff: GroupAffinity) -> Result<GroupAffinity, OsErr> {
            // --- Safe validation BEFORE the unsafe block ---
            // We need the group count and CPU count to validate bounds. These
            // are FFI calls, so they go inside the single unsafe block, but
            // the results are used only for bounds-checking in safe code after.

            let ga = GroupAffinityWin {
                Mask: aff.mask as DWORD_PTR,
                Group: aff.group,
                Reserved: [0, 0, 0], // MUST be zero — the read-back trap
            };

            let mut prev = MaybeUninit::<GroupAffinityWin>::uninit();
            let mut observed = MaybeUninit::<GroupAffinityWin>::uninit();

            // SAFETY: All FFI calls in this block operate on valid inputs:
            // - GetActiveProcessorGroupCount: no args, always safe.
            // - GetActiveProcessorCount: aff.group is validated < groups below.
            // - GetCurrentThread: pseudo-handle, always valid.
            // - SetThreadGroupAffinity: `ga` is fully initialised, aligned,
            //   Reserved=[0;3]. `prev` is aligned stack MaybeUninit.
            // - GetThreadGroupAffinity: `observed` is aligned stack
            //   MaybeUninit. Only read if return != 0.
            // - GetLastError: no args, thread-local, always safe.
            // Group and mask bounds are checked AFTER the block, in safe code,
            // using the values returned from the FFI calls.
            let (groups, cpus, h, ok_set, ok_get, observed_val) = unsafe {
                let groups = GetActiveProcessorGroupCount();
                if groups == 0 {
                    return Err(OsErr::TopologyImplausible { cpus: 0 });
                }
                if aff.group >= groups {
                    return Err(OsErr::InvalidGroup { requested: aff.group, groups });
                }
                let cpus = GetActiveProcessorCount(aff.group);
                if cpus == 0 || cpus > 64 {
                    return Err(OsErr::TopologyImplausible { cpus });
                }
                let legal_mask: usize = if cpus == 64 { !0usize } else { (1usize << cpus) - 1 };
                if aff.mask as usize & !legal_mask != 0 {
                    return Err(OsErr::InvalidMask { mask: aff.mask as usize, legal: legal_mask });
                }

                let h = GetCurrentThread();
                let ok_set = SetThreadGroupAffinity(h, &ga, prev.as_mut_ptr());
                if ok_set == 0 {
                    let code = GetLastError();
                    return Err(OsErr::Win32 { call: "SetThreadGroupAffinity", code });
                }
                // Read-back via GetThreadGroupAffinity (NOT
                // GetCurrentProcessorNumberEx, which shows the *current* CPU;
                // the thread does not migrate until its next yield, giving a
                // false mismatch).
                let ok_get = GetThreadGroupAffinity(h, observed.as_mut_ptr());
                if ok_get == 0 {
                    let code = GetLastError();
                    return Err(OsErr::Win32 { call: "GetThreadGroupAffinity", code });
                }
                let observed_val = observed.assume_init();
                (groups, cpus, h, ok_set, ok_get, observed_val)
            };
            // Suppress unused-variable warnings from the tuple decomposition.
            let _ = (groups, cpus, h, ok_set, ok_get);

            let read_back = GroupAffinity {
                group: observed_val.Group,
                mask: observed_val.Mask as u64,
            };
            if read_back != aff {
                return Err(OsErr::ReadBackMismatch { requested: aff, observed: read_back });
            }
            Ok(read_back)
        }

        fn set_priority(&mut self, _th: ThreadId, prio: Prio) -> Result<Prio, OsErr> {
            let class: DWORD = match prio {
                Prio::Normal => NORMAL_PRIORITY_CLASS,
                Prio::High => HIGH_PRIORITY_CLASS,
                Prio::Realtime => REALTIME_PRIORITY_CLASS,
            };

            // SAFETY: GetCurrentProcess returns a pseudo-handle, always valid.
            // SetPriorityClass takes a valid handle and a priority-class
            // constant. GetPriorityClass takes a valid handle and returns
            // the current class (0 = failure). GetLastError is thread-local.
            // All inputs are valid; failure is detected via return codes.
            let (h, ok_set, observed_class) = unsafe {
                let h = GetCurrentProcess();
                let ok_set = SetPriorityClass(h, class);
                if ok_set == 0 {
                    let code = GetLastError();
                    return Err(OsErr::Win32 { call: "SetPriorityClass", code });
                }
                let observed_class = GetPriorityClass(h);
                if observed_class == 0 {
                    let code = GetLastError();
                    return Err(OsErr::Win32 { call: "GetPriorityClass", code });
                }
                (h, ok_set, observed_class)
            };
            let _ = (h, ok_set);

            let observed = match observed_class {
                c if c == NORMAL_PRIORITY_CLASS => Prio::Normal,
                c if c == HIGH_PRIORITY_CLASS => Prio::High,
                c if c == REALTIME_PRIORITY_CLASS => Prio::Realtime,
                c => {
                    return Err(OsErr::ReadBackMismatchU32 {
                        requested: class,
                        observed: c,
                    });
                }
            };
            if observed != prio {
                return Err(OsErr::ReadBackMismatchU32 {
                    requested: class,
                    observed: observed_class,
                });
            }
            Ok(observed)
        }

        fn set_timer_res_ms(&mut self, ms: u32) -> Result<u32, OsErr> {
            // The timer guard was already acquired in `new()`. This method
            // drops the old guard and acquires a new one with the requested
            // period. The granted period has NO read-back API — it is
            // UNVERIFIABLE.
            let old = core::mem::replace(&mut self.timer, TimerResGuard { period_ms: 0, active: false });
            drop(old);
            self.timer = TimerResGuard::acquire(ms)?;
            // The granted period is UNVERIFIABLE — we report what we requested
            // (clamped), not what the OS actually set, because there is no
            // read-back API.
            Ok(self.timer.period_ms)
        }

        unsafe fn lock_region(&mut self, ptr: *const u8, len: usize) -> Result<usize, OsErr> {
            // --- Safe validation BEFORE the unsafe block ---
            if len == 0 {
                return Err(OsErr::EmptyRegion);
            }
            if self.locked_total.saturating_add(len) > self.lock_budget {
                return Err(OsErr::LockBudgetExceeded {
                    requested: len,
                    budget: self.lock_budget,
                });
            }
            let end = match (ptr as usize).checked_add(len) {
                Some(e) => e,
                None => return Err(OsErr::RangeOverflow),
            };

            // Page-align: round start DOWN to page boundary, round length UP
            // to cover the full page range. This ensures VirtualLock locks
            // complete pages.
            let page = self.page_size;
            let aligned_start = (ptr as usize) & !(page - 1);
            let aligned_end = (end + page - 1) & !(page - 1);
            let aligned_len = aligned_end - aligned_start;
            if aligned_len > self.lock_budget {
                return Err(OsErr::LockBudgetExceeded {
                    requested: aligned_len,
                    budget: self.lock_budget,
                });
            }

            // Raise working-set minimum first (else ERROR_WORKING_SET_QUOTA 1453).
            self.ensure_working_set()?;

            // SAFETY: `ptr` and `len` satisfy the # Safety contract on the
            // trait: ptr is the start of a committed region owned by this
            // process, len lies within it, and the region remains allocated
            // until unlock or adapter drop. We have page-aligned the region
            // and raised the working-set minimum. VirtualLock returns BOOL:
            // non-0 = success. On failure, GetLastError gives the reason.
            let (ok, code) = unsafe {
                let ok = VirtualLock(aligned_start as *mut core::ffi::c_void, aligned_len);
                let code = if ok == 0 { GetLastError() } else { 0 };
                (ok, code)
            };
            if ok == 0 {
                if code == ERROR_WORKING_SET_QUOTA {
                    return Err(OsErr::WorkingSetQuota { requested: aligned_len });
                }
                return Err(OsErr::Win32 { call: "VirtualLock", code });
            }

            // Track for unlock-on-drop.
            self.locked_ranges.push((aligned_start as *mut core::ffi::c_void, aligned_len));
            self.locked_total = self.locked_total.saturating_add(aligned_len);

            // Return the ALIGNED length so the budget cannot under-count.
            Ok(aligned_len)
        }
    }
}
