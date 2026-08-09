//! Section 28 **Tier-2 research and anti-leakage infrastructure**.
//!
//! This module is mandatory even if no cluster feature ever becomes alpha,
//! because without it the validation system cannot prevent creator / funder /
//! operator leakage across folds (Section 28, Section 53). It provides:
//!
//! * a deterministic offline [`UnionFind`] (connected components),
//! * a typed, discovery-time-stamped [`WalletGraph`] with creator / funding /
//!   operator [family grouping](WalletGraph::families_by_kinds),
//! * point-in-time [`families_as_of`](WalletGraph::families_as_of) grouping
//!   (never uses edges discovered after the decision time),
//! * Section 53 [`FamilyHoldout`] generation (whole families are kept intact
//!   inside a single fold, so no family straddles a train/test boundary), and
//! * Section 46 [`build_activity_matched_placebo`] cohorts.
//!
//! All logic is integer and deterministic: fold assignment is a pure function
//! of family membership, and placebo matching is a deterministic greedy pass
//! with a fixed tie-break. No RNG, no wall-clock, no float.

use std::collections::BTreeSet;

/// Kinds of edge in the wallet graph (a subset of the Section 28 edge
/// taxonomy). The kind records *why* two nodes are linked so that family
/// grouping can be restricted to a provenance subset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EdgeKind {
    /// `funded_by` — one wallet funded another.
    Funding,
    /// `transferred_to` — direct token/SOL transfer.
    Transfer,
    /// `same_creator` — same creator across launches.
    SameCreator,
    /// `same_deployer` — same deployer across launches.
    SameDeployer,
    /// `same_funding_root` — shared upstream funding root.
    SameFundingRoot,
    /// `same_fee_payer` — shared transaction fee payer.
    SameFeePayer,
    /// `same_tip_payer` — shared Jito/Nozomi tip payer.
    SameTipPayer,
    /// `same_bundle` — co-submitted in the same bundle.
    SameBundle,
    /// `co_bought_same_block` — bought the same launch in the same block.
    CoBuySameBlock,
    /// `co_bought_first_N` — both among the first-N buyers of a launch.
    CoBuyFirstN,
    /// `co_sold_same_window` — sold inside the same synchronized window.
    SellSync,
    /// `same_metadata` — reused metadata / naming / domain.
    MetadataReuse,
    /// `same_social_amplification_cluster` — coordinated amplification.
    SocialAmplification,
}

impl EdgeKind {
    /// Edge kinds that constitute a **creator/deployer** family boundary.
    #[must_use]
    pub fn creator_family_kinds() -> [EdgeKind; 2] {
        [EdgeKind::SameCreator, EdgeKind::SameDeployer]
    }

    /// Edge kinds that constitute a **funding** family boundary.
    #[must_use]
    pub fn funding_family_kinds() -> [EdgeKind; 3] {
        [
            EdgeKind::Funding,
            EdgeKind::SameFundingRoot,
            EdgeKind::SameFeePayer,
        ]
    }
}

/// A discovery-time-stamped undirected edge between two node indices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edge {
    /// First node index.
    pub a: usize,
    /// Second node index.
    pub b: usize,
    /// Provenance of the link.
    pub kind: EdgeKind,
    /// Slot at which this edge was *discovered* (used for point-in-time
    /// embargo — an edge is invisible to a decision earlier than its discovery).
    pub discovery_slot: u64,
}

/// A disjoint-set (union-find) structure with union-by-rank and path
/// compression. Deterministic: the roots depend only on the sequence of
/// unions, and component enumeration is returned in a canonical order.
#[derive(Debug, Clone)]
pub struct UnionFind {
    parent: Vec<usize>,
    rank: Vec<u8>,
    size: Vec<u32>,
}

impl UnionFind {
    /// Create a forest of `n` singleton sets `0..n`.
    #[must_use]
    pub fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
            rank: vec![0; n],
            size: vec![1; n],
        }
    }

    /// Number of elements.
    #[must_use]
    pub fn len(&self) -> usize {
        self.parent.len()
    }

    /// Whether there are no elements.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.parent.is_empty()
    }

    /// Find the canonical root of `x`, compressing the path.
    pub fn find(&mut self, x: usize) -> usize {
        let mut root = x;
        while self.parent[root] != root {
            root = self.parent[root];
        }
        // Path compression.
        let mut cur = x;
        while self.parent[cur] != root {
            let next = self.parent[cur];
            self.parent[cur] = root;
            cur = next;
        }
        root
    }

    /// Union the sets containing `x` and `y`. Returns `true` iff they were
    /// previously in different sets.
    pub fn union(&mut self, x: usize, y: usize) -> bool {
        let rx = self.find(x);
        let ry = self.find(y);
        if rx == ry {
            return false;
        }
        // Union by rank; on tie the smaller index becomes the root for
        // determinism.
        let (hi, lo) = match self.rank[rx].cmp(&self.rank[ry]) {
            std::cmp::Ordering::Greater => (rx, ry),
            std::cmp::Ordering::Less => (ry, rx),
            std::cmp::Ordering::Equal => {
                let (h, l) = if rx < ry { (rx, ry) } else { (ry, rx) };
                self.rank[h] = self.rank[h].saturating_add(1);
                (h, l)
            }
        };
        self.parent[lo] = hi;
        self.size[hi] = self.size[hi].saturating_add(self.size[lo]);
        true
    }

    /// Whether `x` and `y` are in the same set.
    pub fn connected(&mut self, x: usize, y: usize) -> bool {
        self.find(x) == self.find(y)
    }

    /// Size of the component containing `x`.
    pub fn component_size(&mut self, x: usize) -> u32 {
        let r = self.find(x);
        self.size[r]
    }

    /// Enumerate all connected components in canonical order: each component is
    /// a sorted ascending `Vec` of member indices, and components are sorted by
    /// their smallest member.
    pub fn components(&mut self) -> Vec<Vec<usize>> {
        let n = self.parent.len();
        let mut root_of = vec![0usize; n];
        for (i, slot) in root_of.iter_mut().enumerate() {
            *slot = self.find(i);
        }
        // Group by root using a BTreeMap keyed on the (min-member) canonical
        // representative for stable ordering.
        let mut roots: BTreeSet<usize> = BTreeSet::new();
        for &r in &root_of {
            roots.insert(r);
        }
        // Map root -> its members (members appended in index order => sorted).
        let mut out: Vec<Vec<usize>> = Vec::new();
        // Build root -> position map for O(n) grouping.
        let mut root_pos: std::collections::HashMap<usize, usize> =
            std::collections::HashMap::new();
        for &r in &roots {
            root_pos.insert(r, out.len());
            out.push(Vec::new());
        }
        for (i, &r) in root_of.iter().enumerate() {
            let pos = root_pos[&r];
            out[pos].push(i);
        }
        // Sort components by their smallest member (each member vec is already
        // ascending because we pushed in index order).
        out.sort_by_key(|c| c[0]);
        out
    }
}

/// A typed, discovery-time-stamped wallet graph over `n` fixed node indices.
#[derive(Debug, Clone)]
pub struct WalletGraph {
    node_count: usize,
    edges: Vec<Edge>,
}

impl WalletGraph {
    /// Create a graph over node indices `0..node_count` with no edges.
    #[must_use]
    pub fn new(node_count: usize) -> Self {
        Self {
            node_count,
            edges: Vec::new(),
        }
    }

    /// Number of nodes.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.node_count
    }

    /// All edges (in insertion order).
    #[must_use]
    pub fn edges(&self) -> &[Edge] {
        &self.edges
    }

    /// Add an undirected edge. Panics in debug builds if an endpoint is out of
    /// range; in release the edge is ignored to preserve liveness.
    pub fn add_edge(&mut self, a: usize, b: usize, kind: EdgeKind, discovery_slot: u64) {
        debug_assert!(
            a < self.node_count && b < self.node_count,
            "edge out of range"
        );
        if a < self.node_count && b < self.node_count {
            self.edges.push(Edge {
                a,
                b,
                kind,
                discovery_slot,
            });
        }
    }

    /// Grow the graph to accommodate `new_node_count` nodes, preserving all
    /// existing edges. Used by the engine when a new wallet entity is seen
    /// for the first time and needs to be added to the funding graph.
    pub fn grow(&mut self, new_node_count: usize) {
        if new_node_count > self.node_count {
            self.node_count = new_node_count;
        }
    }

    /// Build families (connected components) using **only** edges whose kind is
    /// in `kinds`. With every kind supplied this yields operator-family
    /// candidates; restricted to creator/deployer kinds it yields
    /// creator families; restricted to funding kinds, funding families.
    #[must_use]
    pub fn families_by_kinds(&self, kinds: &[EdgeKind]) -> Vec<Vec<usize>> {
        self.families_filtered(|e| kinds.contains(&e.kind))
    }

    /// Operator-family candidates: connected components over **all** edges.
    #[must_use]
    pub fn operator_families(&self) -> Vec<Vec<usize>> {
        self.families_filtered(|_| true)
    }

    /// Point-in-time family grouping: uses only edges of the given kinds whose
    /// `discovery_slot <= as_of_slot`. This enforces the constitution's rule
    /// that future wallet/cluster knowledge may never be used at an earlier
    /// decision time (Section 28, Section 6.5).
    #[must_use]
    pub fn families_as_of(&self, kinds: &[EdgeKind], as_of_slot: u64) -> Vec<Vec<usize>> {
        self.families_filtered(|e| e.discovery_slot <= as_of_slot && kinds.contains(&e.kind))
    }

    fn families_filtered<F: Fn(&Edge) -> bool>(&self, keep: F) -> Vec<Vec<usize>> {
        let mut uf = UnionFind::new(self.node_count);
        for e in &self.edges {
            if keep(e) {
                uf.union(e.a, e.b);
            }
        }
        uf.components()
    }
}

/// A Section 53 family holdout assignment: every node is mapped to a fold, and
/// **all** members of a family share the same fold (no family straddles a
/// train/test boundary), which is what prevents creator/funder/operator
/// leakage across folds.
#[derive(Debug, Clone)]
pub struct FamilyHoldout {
    node_fold: Vec<Option<u32>>,
    fold_count: u32,
}

impl FamilyHoldout {
    /// Assign each family (and thus each of its member nodes) to one of
    /// `fold_count` folds. The fold of a family is a pure function of its
    /// canonical representative (smallest member index), so the assignment is
    /// deterministic and reproducible without any RNG.
    ///
    /// `node_count` is the total number of nodes; nodes not appearing in any
    /// family are left unassigned (`None`).
    ///
    /// # Panics
    /// Panics if `fold_count == 0`.
    #[must_use]
    pub fn assign(families: &[Vec<usize>], node_count: usize, fold_count: u32) -> Self {
        assert!(fold_count > 0, "fold_count must be positive");
        let mut node_fold = vec![None; node_count];
        for fam in families {
            if fam.is_empty() {
                continue;
            }
            let rep = fam[0] as u64; // families are sorted ascending
            let fold = (rep % u64::from(fold_count)) as u32;
            for &node in fam {
                if node < node_count {
                    node_fold[node] = Some(fold);
                }
            }
        }
        Self {
            node_fold,
            fold_count,
        }
    }

    /// Fold assigned to `node`, if any.
    #[must_use]
    pub fn fold_of(&self, node: usize) -> Option<u32> {
        self.node_fold.get(node).copied().flatten()
    }

    /// Number of folds.
    #[must_use]
    pub fn fold_count(&self) -> u32 {
        self.fold_count
    }

    /// Verify that no edge crosses a fold boundary among assigned nodes — i.e.
    /// both endpoints of every supplied edge share a fold. Returns `true` iff
    /// leakage-free. Edges touching an unassigned node are ignored (they were
    /// not part of any family used to build the holdout).
    #[must_use]
    pub fn verify_no_leakage(&self, edges: &[Edge]) -> bool {
        for e in edges {
            match (self.fold_of(e.a), self.fold_of(e.b)) {
                (Some(fa), Some(fb)) if fa != fb => return false,
                _ => {}
            }
        }
        true
    }
}

/// One treatment↔control pairing in an activity-matched placebo cohort.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatchedPair {
    /// Treatment (cluster/cohort) node index.
    pub treatment: usize,
    /// Matched control node index drawn from the placebo pool.
    pub control: usize,
    /// Absolute difference in activity between the two (0 = exact match).
    pub activity_gap: u64,
}

/// Build a Section 46 activity-matched placebo cohort.
///
/// For each treatment wallet (processed in the given order), the closest unused
/// pool wallet by absolute activity difference is selected; ties break toward
/// the smallest control node index for determinism. Each pool wallet is used at
/// most once. If the pool is exhausted, remaining treatment wallets go
/// unmatched (the returned vector is shorter than `treatment`).
///
/// The purpose (Section 28 causality discipline) is that if activity-matched
/// non-cluster wallets perform similarly, no cluster-specific edge may be
/// claimed. This function only builds the matching; the performance comparison
/// lives in the evaluator.
#[must_use]
pub fn build_activity_matched_placebo(
    treatment: &[(usize, u64)],
    pool: &[(usize, u64)],
) -> Vec<MatchedPair> {
    let mut used = vec![false; pool.len()];
    let mut out = Vec::with_capacity(treatment.len());
    for &(t_node, t_act) in treatment {
        let mut best: Option<(u64, usize, usize)> = None; // (gap, control_node, pool_idx)
        for (idx, &(p_node, p_act)) in pool.iter().enumerate() {
            if used[idx] {
                continue;
            }
            let gap = t_act.abs_diff(p_act);
            let cand = (gap, p_node, idx);
            best = match best {
                None => Some(cand),
                Some(cur) => {
                    // Prefer smaller gap, then smaller control node index.
                    if (cand.0, cand.1) < (cur.0, cur.1) {
                        Some(cand)
                    } else {
                        Some(cur)
                    }
                }
            };
        }
        if let Some((gap, control, idx)) = best {
            used[idx] = true;
            out.push(MatchedPair {
                treatment: t_node,
                control,
                activity_gap: gap,
            });
        } else {
            break; // pool exhausted
        }
    }
    out
}
