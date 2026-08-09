//! §27/§29.9 **survived-migration creator ledger** — the point-in-time record
//! that makes a genuinely good deployer distinguishable from an unknown one.
//!
//! ## Why this exists
//! The downstream creator vocabulary has four slots — unknown, proven, toxic,
//! serial — but without a ledger of *launch outcomes* only the negative slots
//! are reachable: rugs and serial-launch bursts are observable from a single
//! window, whereas "this creator has shipped tokens that survived" is a
//! statement about resolved history that nothing was keeping. Every deployer
//! therefore collapsed to `Unknown`, which is a real loss: a creator with two
//! migrated launches that were still alive a day later is materially different
//! from a first-time deployer, and the difference is exactly the kind of prior
//! that conditions size.
//!
//! ## The outcome pipeline
//! A launch moves through three observable, on-chain facts:
//!
//! 1. **launched** — the mint was deployed by this creator at `launch_slot`;
//! 2. **migrated** — the token graduated off the bonding curve into a pool at
//!    `migrated_slot`;
//! 3. **rugged** — an LP-pull / hard-dump signature was observed at
//!    `rugged_slot`.
//!
//! **Survival is derived, never asserted.** A launch counts as survived, as of
//! slot `T`, iff it migrated, `migrated_slot + survival_horizon_slots <= T`, and
//! no rug had been observed at or before `T`. Nothing has to tell the ledger
//! "this one survived" — survival is a function of the recorded facts and the
//! query slot, which is what makes it point-in-time exact.
//!
//! ## §20 point-in-time safety
//! [`CreatorLedger::summary_as_of`] counts a fact only when the slot at which
//! that fact became observable is `<= as_of_slot`. A launch that rugs tomorrow
//! cannot make its creator toxic today, and a launch that clears its survival
//! horizon tomorrow cannot make its creator proven today. Late-arriving facts
//! are recorded against the launch they belong to and simply do not count until
//! the query slot reaches them.
//!
//! ## §6.4 fail-closed
//! `Proven` is the only optimistic label here, so it carries every gate:
//!
//! * at least [`CreatorLedgerConfig::min_survived_for_proven`] survived launches
//!   (a named const, never a magic 1);
//! * strictly zero rugs observed at or before the query slot;
//! * an **untruncated** history — if the bounded per-creator ring has evicted
//!   any launch, the evidence base is incomplete, a dropped rug is possible, and
//!   the verdict falls back to [`CreatorTrack::Unknown`] rather than to the
//!   optimistic label.
//!
//! Thin history is `Unknown`. It is never optimistically `Proven`.
//!
//! ## §22/§99
//! Pure integer, no float, no clock, no RNG, no I/O; every slot is
//! caller-supplied. State is bounded on both axes — at most
//! [`CreatorLedgerConfig::max_creators`] creators, each with at most
//! [`CreatorLedgerConfig::max_launches_per_creator`] launches — with documented
//! eviction on each.

use crate::{TokenId, WalletId};

/// Default survival horizon: how long after migration a launch must remain
/// un-rugged before it counts as survived (§29.9).
///
/// 216 000 slots ≈ 24 h at ~400 ms/slot. A day past graduation is the horizon
/// over which the overwhelming majority of post-migration rugs and LP pulls
/// have already fired, so a launch still standing at that point is evidence
/// about the deployer rather than noise about the launch.
pub const CREATOR_SURVIVAL_HORIZON_SLOTS: u64 = 216_000;

/// Default minimum survived launches before [`CreatorTrack::Proven`] (§6.4).
///
/// Two. One survivor is indistinguishable from luck; requiring a second makes
/// the label a claim about the deployer rather than about a single token.
pub const CREATOR_MIN_SURVIVED_FOR_PROVEN: u32 = 2;

/// Default lookback for the serial-launcher burst measure (§27).
/// 216 000 slots ≈ 24 h at ~400 ms/slot.
pub const CREATOR_SERIAL_WINDOW_SLOTS: u64 = 216_000;

/// Default launches inside [`CREATOR_SERIAL_WINDOW_SLOTS`] at/above which a
/// creator reads as a serial launcher (§27).
pub const CREATOR_SERIAL_MIN_LAUNCHES: u32 = 5;

/// Default rugs at/above which a creator reads as toxic (§29.9). One observed
/// rug is sufficient: the risk-side label is deliberately the easiest to earn.
pub const CREATOR_MIN_RUGS_FOR_TOXIC: u32 = 1;

/// Default creator capacity of the ledger (§99/§57 memory law).
pub const CREATOR_LEDGER_CAP: usize = 4_096;

/// Default per-creator launch-history capacity (§99/§57 memory law).
pub const CREATOR_LAUNCH_HISTORY_CAP: usize = 32;

/// Prior-behaviour track record of a creator (§29.9).
///
/// The discriminants are the dense ordinals the downstream episodic-recall
/// fingerprint uses for its nominal creator field, so the mapping across the
/// crate boundary is the identity on [`Self::ordinal`] and cannot silently
/// drift.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum CreatorTrack {
    /// No sufficient recorded history at the query slot (§6.4). The default.
    Unknown = 0,
    /// Has shipped launches that migrated and then survived the horizon, with
    /// no observed rug and a complete history.
    Proven = 1,
    /// Has a recorded rug / LP-pull at or before the query slot.
    Toxic = 2,
    /// Launches at high frequency inside the serial window.
    Serial = 3,
}

impl CreatorTrack {
    /// Dense ordinal for one-hot encoding / compact keying.
    #[inline]
    #[must_use]
    pub const fn ordinal(self) -> u8 {
        self as u8
    }

    /// Inverse of [`Self::ordinal`]; out-of-range input yields `None`.
    #[must_use]
    pub const fn from_ordinal(o: u8) -> Option<Self> {
        match o {
            0 => Some(Self::Unknown),
            1 => Some(Self::Proven),
            2 => Some(Self::Toxic),
            3 => Some(Self::Serial),
            _ => None,
        }
    }
}

/// Named, versioned gates for the ledger (§102 — no magic number in a decision
/// path; every comparison in [`CreatorLedger::classify_as_of`] reads a field
/// here).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CreatorLedgerConfig {
    /// Slots a migrated launch must stand, un-rugged, to count as survived.
    pub survival_horizon_slots: u64,
    /// Minimum survived launches for [`CreatorTrack::Proven`].
    pub min_survived_for_proven: u32,
    /// Lookback for the serial-launch burst count, in slots.
    pub serial_window_slots: u64,
    /// Launches inside the serial window at/above which the creator is serial.
    pub serial_min_launches: u32,
    /// Rugs at/above which the creator is toxic.
    pub min_rugs_for_toxic: u32,
    /// Maximum distinct creators tracked (§99).
    pub max_creators: usize,
    /// Maximum launches retained per creator (§99).
    pub max_launches_per_creator: usize,
}

impl CreatorLedgerConfig {
    /// The named-const default configuration.
    pub const DEFAULT: Self = CreatorLedgerConfig {
        survival_horizon_slots: CREATOR_SURVIVAL_HORIZON_SLOTS,
        min_survived_for_proven: CREATOR_MIN_SURVIVED_FOR_PROVEN,
        serial_window_slots: CREATOR_SERIAL_WINDOW_SLOTS,
        serial_min_launches: CREATOR_SERIAL_MIN_LAUNCHES,
        min_rugs_for_toxic: CREATOR_MIN_RUGS_FOR_TOXIC,
        max_creators: CREATOR_LEDGER_CAP,
        max_launches_per_creator: CREATOR_LAUNCH_HISTORY_CAP,
    };

    /// Whether the configuration can yield a meaningful verdict. A
    /// `min_survived_for_proven` of zero would make every creator with an empty
    /// history "proven", which is the exact failure this ledger exists to
    /// prevent, so it is rejected and every verdict becomes `Unknown` (§6.4).
    #[must_use]
    pub const fn is_valid(&self) -> bool {
        self.min_survived_for_proven > 0
            && self.min_rugs_for_toxic > 0
            && self.serial_min_launches > 0
            && self.max_creators > 0
            && self.max_launches_per_creator > 0
    }
}

impl Default for CreatorLedgerConfig {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// One launch and the slots at which each of its terminal facts became
/// observable (§20). `None` means "not observed yet", never "did not happen".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LaunchRecord {
    /// Token launched.
    pub token: TokenId,
    /// Slot at which the launch was observed.
    pub launch_slot: u64,
    /// Slot at which migration/graduation was observed, if it has been.
    pub migrated_slot: Option<u64>,
    /// Slot at which a rug / LP-pull signature was observed, if it has been.
    pub rugged_slot: Option<u64>,
}

impl LaunchRecord {
    /// Whether this launch had migrated as of `as_of_slot` (§20).
    #[must_use]
    pub fn migrated_as_of(&self, as_of_slot: u64) -> bool {
        matches!(self.migrated_slot, Some(s) if s <= as_of_slot)
    }

    /// Whether a rug had been observed as of `as_of_slot` (§20).
    #[must_use]
    pub fn rugged_as_of(&self, as_of_slot: u64) -> bool {
        matches!(self.rugged_slot, Some(s) if s <= as_of_slot)
    }

    /// Whether this launch counts as **survived** as of `as_of_slot`: migrated,
    /// the survival horizon elapsed, and no rug observed by then (§29.9).
    ///
    /// Derived from the recorded facts and the query slot alone — nothing
    /// asserts survival, so it cannot be asserted early.
    #[must_use]
    pub fn survived_as_of(&self, as_of_slot: u64, survival_horizon_slots: u64) -> bool {
        let Some(m) = self.migrated_slot else {
            return false;
        };
        if m > as_of_slot || self.rugged_as_of(as_of_slot) {
            return false;
        }
        match m.checked_add(survival_horizon_slots) {
            Some(deadline) => deadline <= as_of_slot,
            // A horizon that overflows the slot axis can never elapse.
            None => false,
        }
    }
}

/// Point-in-time counts over one creator's launch history (§20).
///
/// Every count is "as observed at or before the query slot". `truncated` is the
/// §6.4 completeness flag: once the bounded per-creator ring has evicted a
/// launch the counts are lower bounds, and the optimistic verdict is withheld.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct CreatorTrackSummary {
    /// Launches observed at or before the query slot.
    pub launches: u32,
    /// Launches inside the serial lookback window ending at the query slot.
    pub launches_in_window: u32,
    /// Launches that had migrated by the query slot.
    pub migrated: u32,
    /// Launches that had survived the horizon un-rugged by the query slot.
    pub survived: u32,
    /// Launches with a rug observed at or before the query slot.
    pub rugged: u32,
    /// Whether this creator's history has lost launches to the capacity bound.
    pub truncated: bool,
}

impl CreatorTrackSummary {
    /// Resolved terminal outcomes: survived plus rugged. The evidence base for
    /// any outcome-derived statement about this creator.
    #[must_use]
    pub const fn resolved(&self) -> u32 {
        self.survived.saturating_add(self.rugged)
    }
}

/// Result of ingesting one ledger fact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LedgerWrite {
    /// The fact was recorded.
    Recorded,
    /// The fact referred to a launch this ledger does not hold (never seen, or
    /// evicted by the capacity bound). Nothing was invented to accommodate it.
    UnknownLaunch,
    /// The fact was refused because it would move a recorded slot backwards, or
    /// because it duplicates an already-recorded terminal fact (§20: the first
    /// observation of a terminal fact is the one that counts).
    Refused,
    /// A new creator could not be admitted: the ledger is at capacity and no
    /// eviction victim was available.
    AtCapacity,
}

/// One creator's bounded launch history.
#[derive(Clone, Debug)]
struct CreatorHistory {
    /// Launches, kept sorted ascending by `(launch_slot, token)`.
    launches: Vec<LaunchRecord>,
    /// Latest slot of any fact recorded for this creator; the eviction key.
    last_slot: u64,
    /// Launches evicted by the per-creator capacity bound.
    dropped: u64,
}

impl CreatorHistory {
    fn new() -> Self {
        CreatorHistory {
            launches: Vec::new(),
            last_slot: 0,
            dropped: 0,
        }
    }

    fn find_mut(&mut self, token: TokenId) -> Option<&mut LaunchRecord> {
        self.launches.iter_mut().find(|r| r.token == token)
    }
}

/// A bounded, point-in-time ledger of per-creator launch outcomes (§27/§29.9).
///
/// Creators are held in a `Vec` kept sorted by [`WalletId`], so lookup is a
/// binary search and iteration is deterministic. When a *new* creator arrives at
/// capacity, the creator with the oldest `last_slot` is evicted (ties broken by
/// the smaller id, so eviction is a pure function of state — no clock, no
/// insertion-order dependence) and counted in [`Self::creator_evictions`].
/// Within a creator, the oldest launch is evicted first and the loss is recorded
/// as [`CreatorTrackSummary::truncated`].
#[derive(Clone, Debug)]
pub struct CreatorLedger {
    entries: Vec<(WalletId, CreatorHistory)>,
    cfg: CreatorLedgerConfig,
    creator_evictions: u64,
}

impl CreatorLedger {
    /// Create an empty ledger with the given configuration.
    #[must_use]
    pub fn new(cfg: CreatorLedgerConfig) -> Self {
        CreatorLedger {
            entries: Vec::new(),
            cfg,
            creator_evictions: 0,
        }
    }

    /// Create an empty ledger with [`CreatorLedgerConfig::DEFAULT`].
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(CreatorLedgerConfig::DEFAULT)
    }

    /// The configuration in force.
    #[must_use]
    pub const fn config(&self) -> &CreatorLedgerConfig {
        &self.cfg
    }

    /// Number of tracked creators.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no creator is tracked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Count of whole creator histories dropped by the capacity bound.
    #[must_use]
    pub const fn creator_evictions(&self) -> u64 {
        self.creator_evictions
    }

    /// Launches evicted from `creator`'s history by the per-creator bound.
    #[must_use]
    pub fn dropped_launches(&self, creator: WalletId) -> u64 {
        self.history(creator).map_or(0, |h| h.dropped)
    }

    fn history(&self, creator: WalletId) -> Option<&CreatorHistory> {
        match self.entries.binary_search_by_key(&creator, |(k, _)| *k) {
            Ok(pos) => self.entries.get(pos).map(|(_, h)| h),
            Err(_) => None,
        }
    }

    /// Read-only view of a creator's recorded launches, ascending by launch
    /// slot. `None` for an untracked creator.
    #[must_use]
    pub fn launches(&self, creator: WalletId) -> Option<&[LaunchRecord]> {
        self.history(creator).map(|h| h.launches.as_slice())
    }

    /// Index of the eviction victim: oldest `last_slot`, ties by smaller id.
    fn evict_index(&self) -> Option<usize> {
        let mut best: Option<(usize, u64, WalletId)> = None;
        for (i, (key, hist)) in self.entries.iter().enumerate() {
            let replace = match best {
                None => true,
                Some((_, best_slot, best_key)) => {
                    hist.last_slot < best_slot || (hist.last_slot == best_slot && *key < best_key)
                }
            };
            if replace {
                best = Some((i, hist.last_slot, *key));
            }
        }
        best.map(|(i, _, _)| i)
    }

    /// Obtain a mutable history for `creator`, admitting it if new (evicting the
    /// least-recently-active creator when at capacity).
    fn history_mut(&mut self, creator: WalletId) -> Option<&mut CreatorHistory> {
        match self.entries.binary_search_by_key(&creator, |(k, _)| *k) {
            Ok(pos) => self.entries.get_mut(pos).map(|(_, h)| h),
            Err(pos) => {
                let mut pos = pos;
                if self.entries.len() >= self.cfg.max_creators {
                    let victim = self.evict_index()?;
                    self.entries.remove(victim);
                    self.creator_evictions = self.creator_evictions.saturating_add(1);
                    if victim < pos {
                        pos -= 1;
                    }
                }
                if self.entries.len() >= self.cfg.max_creators {
                    return None;
                }
                self.entries.insert(pos, (creator, CreatorHistory::new()));
                self.entries.get_mut(pos).map(|(_, h)| h)
            }
        }
    }

    /// Record a launch: `creator` deployed `token`, observed at `slot`.
    ///
    /// A repeated launch of the same token is [`LedgerWrite::Refused`] — a token
    /// is launched once, and re-recording it would double-count the creator's
    /// history. Admitting the launch may evict the oldest launch in this
    /// creator's history (flagging the summary truncated) or the
    /// least-recently-active creator.
    pub fn record_launch(&mut self, creator: WalletId, token: TokenId, slot: u64) -> LedgerWrite {
        let cap = self.cfg.max_launches_per_creator;
        let Some(hist) = self.history_mut(creator) else {
            return LedgerWrite::AtCapacity;
        };
        if hist.launches.iter().any(|r| r.token == token) {
            return LedgerWrite::Refused;
        }
        let rec = LaunchRecord {
            token,
            launch_slot: slot,
            migrated_slot: None,
            rugged_slot: None,
        };
        // Keep ascending by (launch_slot, token) so eviction of the oldest is a
        // front removal and `launches()` is deterministic.
        let pos = hist
            .launches
            .partition_point(|r| (r.launch_slot, r.token) < (slot, token));
        hist.launches.insert(pos, rec);
        if hist.launches.len() > cap {
            hist.launches.remove(0);
            hist.dropped = hist.dropped.saturating_add(1);
        }
        hist.last_slot = hist.last_slot.max(slot);
        LedgerWrite::Recorded
    }

    /// Record that `token` migrated / graduated, observed at `slot`.
    ///
    /// Refused when the migration slot precedes the recorded launch slot, or
    /// when a migration was already recorded (§20 — the first observation of a
    /// terminal fact is the one that counts, so a later restatement cannot move
    /// the survival clock).
    pub fn record_migration(
        &mut self,
        creator: WalletId,
        token: TokenId,
        slot: u64,
    ) -> LedgerWrite {
        match self.entries.binary_search_by_key(&creator, |(k, _)| *k) {
            Ok(pos) => match self.entries.get_mut(pos) {
                Some((_, hist)) => {
                    let Some(rec) = hist.find_mut(token) else {
                        return LedgerWrite::UnknownLaunch;
                    };
                    if slot < rec.launch_slot || rec.migrated_slot.is_some() {
                        return LedgerWrite::Refused;
                    }
                    rec.migrated_slot = Some(slot);
                    hist.last_slot = hist.last_slot.max(slot);
                    LedgerWrite::Recorded
                }
                None => LedgerWrite::UnknownLaunch,
            },
            Err(_) => LedgerWrite::UnknownLaunch,
        }
    }

    /// Record a rug / LP-pull signature on `token`, observed at `slot`.
    ///
    /// Refused when the rug slot precedes the recorded launch slot or a rug was
    /// already recorded. The *first* observed rug is the one that counts.
    pub fn record_rug(&mut self, creator: WalletId, token: TokenId, slot: u64) -> LedgerWrite {
        match self.entries.binary_search_by_key(&creator, |(k, _)| *k) {
            Ok(pos) => match self.entries.get_mut(pos) {
                Some((_, hist)) => {
                    let Some(rec) = hist.find_mut(token) else {
                        return LedgerWrite::UnknownLaunch;
                    };
                    if slot < rec.launch_slot || rec.rugged_slot.is_some() {
                        return LedgerWrite::Refused;
                    }
                    rec.rugged_slot = Some(slot);
                    hist.last_slot = hist.last_slot.max(slot);
                    LedgerWrite::Recorded
                }
                None => LedgerWrite::UnknownLaunch,
            },
            Err(_) => LedgerWrite::UnknownLaunch,
        }
    }

    /// Point-in-time counts for `creator` as known at `as_of_slot` (§20).
    ///
    /// `None` for an untracked creator — an absent history is reported as
    /// absent, never as a summary of zeroes that would read like a measured
    /// clean record (§6.4).
    #[must_use]
    pub fn summary_as_of(&self, creator: WalletId, as_of_slot: u64) -> Option<CreatorTrackSummary> {
        let hist = self.history(creator)?;
        let window_start = as_of_slot.saturating_sub(self.cfg.serial_window_slots);
        let mut s = CreatorTrackSummary {
            truncated: hist.dropped > 0,
            ..CreatorTrackSummary::default()
        };
        for rec in &hist.launches {
            if rec.launch_slot > as_of_slot {
                continue;
            }
            s.launches = s.launches.saturating_add(1);
            if rec.launch_slot >= window_start {
                s.launches_in_window = s.launches_in_window.saturating_add(1);
            }
            if rec.migrated_as_of(as_of_slot) {
                s.migrated = s.migrated.saturating_add(1);
            }
            if rec.rugged_as_of(as_of_slot) {
                s.rugged = s.rugged.saturating_add(1);
            }
            if rec.survived_as_of(as_of_slot, self.cfg.survival_horizon_slots) {
                s.survived = s.survived.saturating_add(1);
            }
        }
        Some(s)
    }

    /// Classify `creator`'s track record as known at `as_of_slot` (§29.9).
    ///
    /// Priority cascade, first match wins — a total, order-independent function
    /// of the summary:
    ///
    /// 1. **Toxic** — rugs observed at/above `min_rugs_for_toxic`. The risk read
    ///    dominates: a deployer who has rugged is not "also proven".
    /// 2. **Serial** — launches inside the serial window at/above
    ///    `serial_min_launches`.
    /// 3. **Proven** — survived launches at/above `min_survived_for_proven`,
    ///    zero rugs, and an untruncated history.
    /// 4. **Unknown** — everything else, including every thin history (§6.4).
    ///
    /// An untracked creator, or an invalid configuration, is `Unknown`.
    #[must_use]
    pub fn classify_as_of(&self, creator: WalletId, as_of_slot: u64) -> CreatorTrack {
        let Some(s) = self.summary_as_of(creator, as_of_slot) else {
            return CreatorTrack::Unknown;
        };
        classify_track(&s, &self.cfg)
    }
}

impl Default for CreatorLedger {
    fn default() -> Self {
        Self::with_defaults()
    }
}

/// Pure classification of a point-in-time summary (§29.9). See
/// [`CreatorLedger::classify_as_of`] for the cascade and its rationale.
///
/// Exposed separately so a caller holding a summary from elsewhere gets the
/// identical verdict — two call sites can never disagree about a boundary.
#[must_use]
pub fn classify_track(s: &CreatorTrackSummary, cfg: &CreatorLedgerConfig) -> CreatorTrack {
    if !cfg.is_valid() {
        return CreatorTrack::Unknown;
    }
    if s.rugged >= cfg.min_rugs_for_toxic {
        return CreatorTrack::Toxic;
    }
    if s.launches_in_window >= cfg.serial_min_launches {
        return CreatorTrack::Serial;
    }
    if s.survived >= cfg.min_survived_for_proven && s.rugged == 0 && !s.truncated {
        return CreatorTrack::Proven;
    }
    CreatorTrack::Unknown
}

// ---------------------------------------------------------------------------
// §27 Persistence — manual binary serialization (no serde, zero-dep crate)
// ---------------------------------------------------------------------------

/// Magic bytes for the ledger file format: `b"CLGR"` (Creator Ledger).
const LEDGER_MAGIC: [u8; 4] = [b'C', b'L', b'G', b'R'];

/// Current serialization format version. Increment on breaking change.
/// v1 = initial format (config + entries + evictions).
const LEDGER_VERSION: u32 = 1;

/// Serialization error — fail-closed, never panics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedgerSerError {
    /// Buffer too short to contain a valid header.
    Truncated,
    /// Magic bytes do not match.
    BadMagic,
    /// Format version is newer than the reader supports.
    UnsupportedVersion(u32),
    /// A varint or entry count exceeded a sane bound.
    Corrupt,
}

/// Encode a `u64` as a varint (LEB128) into `buf`. Returns bytes written.
fn encode_varint(buf: &mut Vec<u8>, val: u64) {
    let mut v = val;
    loop {
        let mut byte = (v & 0x7F) as u8;
        v >>= 7;
        if v == 0 {
            buf.push(byte);
            break;
        } else {
            byte |= 0x80;
            buf.push(byte);
        }
    }
}

/// Decode a varint (LEB128) from `buf` at position `pos`.
/// Returns `(value, bytes_consumed)` or `None` on truncation.
fn decode_varint(buf: &[u8], pos: usize) -> Option<(u64, usize)> {
    let mut result: u64 = 0;
    let mut shift: u32 = 0;
    let mut offset = 0;
    loop {
        if pos + offset >= buf.len() {
            return None; // truncated
        }
        let byte = buf[pos + offset];
        offset += 1;
        result |= ((byte & 0x7F) as u64) << shift;
        if (byte & 0x80) == 0 {
            return Some((result, offset));
        }
        shift += 7;
        if shift > 63 {
            return None; // corrupt: varint too long
        }
    }
}

impl CreatorLedgerConfig {
    /// Encode the config as 7 fixed-width little-endian fields.
    fn encode_to(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&self.survival_horizon_slots.to_le_bytes());
        buf.extend_from_slice(&self.min_survived_for_proven.to_le_bytes());
        buf.extend_from_slice(&self.serial_window_slots.to_le_bytes());
        buf.extend_from_slice(&self.serial_min_launches.to_le_bytes());
        buf.extend_from_slice(&self.min_rugs_for_toxic.to_le_bytes());
        encode_varint(buf, self.max_creators as u64);
        encode_varint(buf, self.max_launches_per_creator as u64);
    }

    /// Decode the config from `buf` at `pos`. Returns `(config, bytes_read)`.
    fn decode_from(buf: &[u8], pos: usize) -> Option<(Self, usize)> {
        if pos + 8 + 4 + 8 + 4 + 4 + 4 > buf.len() {
            return None;
        }
        let mut p = pos;
        let survival_horizon_slots = u64::from_le_bytes(
            buf[p..p + 8].try_into().ok()?,
        );
        p += 8;
        let min_survived_for_proven = u32::from_le_bytes(
            buf[p..p + 4].try_into().ok()?,
        );
        p += 4;
        let serial_window_slots = u64::from_le_bytes(
            buf[p..p + 8].try_into().ok()?,
        );
        p += 8;
        let serial_min_launches = u32::from_le_bytes(
            buf[p..p + 4].try_into().ok()?,
        );
        p += 4;
        let min_rugs_for_toxic = u32::from_le_bytes(
            buf[p..p + 4].try_into().ok()?,
        );
        p += 4;
        let (max_creators, n) = decode_varint(buf, p)?;
        p += n;
        let (max_launches_per_creator, n2) = decode_varint(buf, p)?;
        p += n2;
        Some((
            CreatorLedgerConfig {
                survival_horizon_slots,
                min_survived_for_proven,
                serial_window_slots,
                serial_min_launches,
                min_rugs_for_toxic,
                max_creators: max_creators as usize,
                max_launches_per_creator: max_launches_per_creator as usize,
            },
            p - pos,
        ))
    }
}

impl LaunchRecord {
    /// Encode as: token(varint) + launch_slot(varint) + migrated_slot(varint, 0=None) + rugged_slot(varint, 0=None).
    fn encode_to(&self, buf: &mut Vec<u8>) {
        encode_varint(buf, self.token.0);
        encode_varint(buf, self.launch_slot);
        match self.migrated_slot {
            Some(s) => {
                encode_varint(buf, s + 1); // +1 so 0=absent, s+1=present
            }
            None => encode_varint(buf, 0),
        }
        match self.rugged_slot {
            Some(s) => {
                encode_varint(buf, s + 1);
            }
            None => encode_varint(buf, 0),
        }
    }

    /// Decode from `buf` at `pos`. Returns `(record, bytes_read)` or `None`.
    fn decode_from(buf: &[u8], pos: usize) -> Option<(Self, usize)> {
        let mut p = pos;
        let (token, n) = decode_varint(buf, p)?;
        p += n;
        let (launch_slot, n) = decode_varint(buf, p)?;
        p += n;
        let (migrated_raw, n) = decode_varint(buf, p)?;
        p += n;
        let (rugged_raw, n) = decode_varint(buf, p)?;
        p += n;
        let migrated_slot = if migrated_raw == 0 {
            None
        } else {
            // Undo the +1 encoding; saturating to guard against corrupt 0-overflow.
            Some(migrated_raw.checked_sub(1)?)
        };
        let rugged_slot = if rugged_raw == 0 {
            None
        } else {
            Some(rugged_raw.checked_sub(1)?)
        };
        Some((
            LaunchRecord {
                token: TokenId(token),
                launch_slot,
                migrated_slot,
                rugged_slot,
            },
            p - pos,
        ))
    }
}

impl CreatorLedger {
    /// Serialize the entire ledger to a compact binary buffer.
    ///
    /// Format: magic(4) + version(4 LE) + config + creator_count(varint) +
    /// creator_evictions(varint) + per-creator entries.
    ///
    /// Each entry: wallet_id(varint) + last_slot(varint) + dropped(varint) +
    /// launch_count(varint) + launches.
    #[must_use]
    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(256);
        buf.extend_from_slice(&LEDGER_MAGIC);
        buf.extend_from_slice(&LEDGER_VERSION.to_le_bytes());
        self.cfg.encode_to(&mut buf);
        encode_varint(&mut buf, self.entries.len() as u64);
        encode_varint(&mut buf, self.creator_evictions);
        for (wid, hist) in &self.entries {
            encode_varint(&mut buf, wid.0);
            encode_varint(&mut buf, hist.last_slot);
            encode_varint(&mut buf, hist.dropped);
            encode_varint(&mut buf, hist.launches.len() as u64);
            for rec in &hist.launches {
                rec.encode_to(&mut buf);
            }
        }
        buf
    }

    /// Deserialize a ledger from a binary buffer.
    ///
    /// Returns the reconstructed ledger, or an error if the buffer is
    /// malformed. Fail-closed: a corrupt buffer yields an error, never a
    /// partially-populated ledger.
    pub fn deserialize(buf: &[u8]) -> Result<Self, LedgerSerError> {
        if buf.len() < 4 + 4 {
            return Err(LedgerSerError::Truncated);
        }
        if buf[..4] != LEDGER_MAGIC {
            return Err(LedgerSerError::BadMagic);
        }
        let version = u32::from_le_bytes(buf[4..8].try_into().map_err(|_| LedgerSerError::Truncated)?);
        if version != LEDGER_VERSION {
            return Err(LedgerSerError::UnsupportedVersion(version));
        }
        let mut pos = 8;
        let (cfg, n) = CreatorLedgerConfig::decode_from(buf, pos)
            .ok_or(LedgerSerError::Truncated)?;
        pos += n;
        let (entry_count, n) = decode_varint(buf, pos).ok_or(LedgerSerError::Truncated)?;
        pos += n;
        // Sanity bound: entry_count should not exceed max_creators (or a reasonable
        // upper bound for safety against corrupt inputs).
        if entry_count > 1_000_000 {
            return Err(LedgerSerError::Corrupt);
        }
        let (creator_evictions, n) = decode_varint(buf, pos).ok_or(LedgerSerError::Truncated)?;
        pos += n;

        let mut entries: Vec<(WalletId, CreatorHistory)> = Vec::with_capacity(entry_count as usize);
        for _ in 0..entry_count {
            let (wid_raw, n) = decode_varint(buf, pos).ok_or(LedgerSerError::Truncated)?;
            pos += n;
            let (last_slot, n) = decode_varint(buf, pos).ok_or(LedgerSerError::Truncated)?;
            pos += n;
            let (dropped, n) = decode_varint(buf, pos).ok_or(LedgerSerError::Truncated)?;
            pos += n;
            let (launch_count, n) = decode_varint(buf, pos).ok_or(LedgerSerError::Truncated)?;
            pos += n;
            if launch_count > 10_000 {
                return Err(LedgerSerError::Corrupt);
            }
            let mut launches: Vec<LaunchRecord> = Vec::with_capacity(launch_count as usize);
            for _ in 0..launch_count {
                let (rec, n) = LaunchRecord::decode_from(buf, pos)
                    .ok_or(LedgerSerError::Truncated)?;
                pos += n;
                launches.push(rec);
            }
            entries.push((
                WalletId(wid_raw),
                CreatorHistory {
                    launches,
                    last_slot,
                    dropped,
                },
            ));
        }

        Ok(CreatorLedger {
            entries,
            cfg,
            creator_evictions,
        })
    }
}

#[cfg(test)]
mod ser_tests {
    use super::*;

    #[test]
    fn test_roundtrip_empty_ledger() {
        let ledger = CreatorLedger::with_defaults();
        let buf = ledger.serialize();
        let restored = CreatorLedger::deserialize(&buf).unwrap();
        assert_eq!(restored.len(), 0);
        assert_eq!(restored.creator_evictions(), 0);
        assert_eq!(restored.config().survival_horizon_slots, ledger.config().survival_horizon_slots);
    }

    #[test]
    fn test_roundtrip_with_entries() {
        let mut ledger = CreatorLedger::with_defaults();
        let creator = WalletId(42);
        let token1 = TokenId(100);
        let token2 = TokenId(200);
        ledger.record_launch(creator, token1, 1_000);
        ledger.record_launch(creator, token2, 2_000);
        ledger.record_migration(creator, token1, 1_500);
        ledger.record_rug(creator, token2, 2_500);

        let buf = ledger.serialize();
        let restored = CreatorLedger::deserialize(&buf).unwrap();

        assert_eq!(restored.len(), 1);
        assert_eq!(restored.creator_evictions(), 0);

        let launches = restored.launches(creator).unwrap();
        assert_eq!(launches.len(), 2);
        assert_eq!(launches[0].token, token1);
        assert_eq!(launches[0].launch_slot, 1_000);
        assert_eq!(launches[0].migrated_slot, Some(1_500));
        assert_eq!(launches[0].rugged_slot, None);
        assert_eq!(launches[1].token, token2);
        assert_eq!(launches[1].launch_slot, 2_000);
        assert_eq!(launches[1].migrated_slot, None);
        assert_eq!(launches[1].rugged_slot, Some(2_500));
    }

    #[test]
    fn test_roundtrip_multiple_creators() {
        let mut ledger = CreatorLedger::with_defaults();
        let c1 = WalletId(10);
        let c2 = WalletId(20);
        let t1 = TokenId(100);
        let t2 = TokenId(200);
        ledger.record_launch(c1, t1, 100);
        ledger.record_launch(c2, t2, 200);
        ledger.record_migration(c1, t1, 150);

        let buf = ledger.serialize();
        let restored = CreatorLedger::deserialize(&buf).unwrap();

        assert_eq!(restored.len(), 2);
        assert!(restored.launches(c1).is_some());
        assert!(restored.launches(c2).is_some());

        let c1_launches = restored.launches(c1).unwrap();
        assert_eq!(c1_launches[0].migrated_slot, Some(150));
        let c2_launches = restored.launches(c2).unwrap();
        assert_eq!(c2_launches[0].migrated_slot, None);
    }

    #[test]
    fn test_bad_magic() {
        let mut buf = vec![0x00, 0x00, 0x00, 0x00];
        buf.extend_from_slice(&LEDGER_VERSION.to_le_bytes());
        let result = CreatorLedger::deserialize(&buf);
        assert!(matches!(result, Err(LedgerSerError::BadMagic)));
    }

    #[test]
    fn test_truncated() {
        let result = CreatorLedger::deserialize(&[0x01, 0x02]);
        assert!(matches!(result, Err(LedgerSerError::Truncated)));
    }

    #[test]
    fn test_unsupported_version() {
        let mut buf = LEDGER_MAGIC.to_vec();
        buf.extend_from_slice(&999u32.to_le_bytes());
        let result = CreatorLedger::deserialize(&buf);
        match result {
            Err(LedgerSerError::UnsupportedVersion(v)) => assert_eq!(v, 999),
            _ => panic!("expected UnsupportedVersion"),
        }
    }

    #[test]
    fn test_roundtrip_preserves_classification() {
        let mut ledger = CreatorLedger::with_defaults();
        let creator = WalletId(42);
        // Record 2 launches that both migrate and survive the horizon
        let horizon = CREATOR_SURVIVAL_HORIZON_SLOTS;
        let t1 = TokenId(1);
        let t2 = TokenId(2);
        ledger.record_launch(creator, t1, 100);
        ledger.record_launch(creator, t2, 200);
        ledger.record_migration(creator, t1, 150);
        ledger.record_migration(creator, t2, 250);

        // Classify before serialization — should be Unknown (not enough time)
        let before = ledger.classify_as_of(creator, 300);
        assert_eq!(before, CreatorTrack::Unknown);

        let buf = ledger.serialize();
        let restored = CreatorLedger::deserialize(&buf).unwrap();

        // After restoration, classify at a slot past BOTH survival horizons.
        // Token 2 migrated at 250, so it survives at 250 + horizon.
        // Need both to survive → query_slot must be >= 250 + horizon.
        let query_slot = 250 + horizon + 1;
        let after = restored.classify_as_of(creator, query_slot);
        assert_eq!(after, CreatorTrack::Proven);
    }
}
