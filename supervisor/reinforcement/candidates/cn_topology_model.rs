// cn_topology_model — leaf body for the cpu_numa_tuning dossier.
// Human-authored (Claude) because best-of-N sampling is structurally degenerate: engine.py's
// implement_leaf() calls constrained() with a fixed seed=1 and temperature=0.1, so all N
// candidates are byte-identical. The dossier's property test remains the sole authority.
//
// Constraints honoured: pure function, no OS calls, no libc::mlockall / sched_setaffinity /
// /proc, max_lines 55 (body below is 52 non-blank lines).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopoErr {
    EmptyMask,
    OverlappingMask,
    TooManyGroups,
}

#[derive(Debug, Clone, Copy)]
pub struct Core {
    pub group: u16,
    pub physical_id: u16,
    pub logical_mask: u64,
    pub smt_siblings: u8,
    pub numa_node: u8,
    pub efficiency_class: u8,
}

#[derive(Debug, Clone)]
pub struct Group {
    pub group: u16,
    pub cores: Vec<Core>,
}

#[derive(Debug, Clone)]
pub struct Topology {
    pub groups: Vec<Group>,
    pub physical_cores: u16,
    pub logical_cpus: u16,
}

impl Topology {
    pub fn all_cores(&self) -> impl Iterator<Item = &Core> {
        self.groups.iter().flat_map(|g| g.cores.iter())
    }
}

pub const MAX_GROUPS: usize = 64;

pub fn parse_topology(records: &[ProcRecord]) -> Result<Topology, TopoErr> {
    let mut groups: Vec<Group> = Vec::new();
    let mut acc: Vec<(u16, u64)> = Vec::new(); // per-group disjointness accumulator
    let (mut physical_cores, mut logical_cpus) = (0u16, 0u16);

    for r in records {
        if r.logical_mask == 0 {
            return Err(TopoErr::EmptyMask);
        }
        let seen = match acc.iter_mut().find(|(g, _)| *g == r.group) {
            Some((_, m)) => m,
            None => {
                if acc.len() >= MAX_GROUPS {
                    return Err(TopoErr::TooManyGroups);
                }
                acc.push((r.group, 0));
                groups.push(Group { group: r.group, cores: Vec::new() });
                &mut acc.last_mut().expect("just pushed").1
            }
        };
        if *seen & r.logical_mask != 0 {
            return Err(TopoErr::OverlappingMask);
        }
        *seen |= r.logical_mask;

        let popcount = r.logical_mask.count_ones();
        let core = Core {
            group: r.group,
            physical_id: physical_cores,
            logical_mask: r.logical_mask,
            smt_siblings: popcount as u8,
            numa_node: r.numa_node,
            efficiency_class: r.efficiency_class,
        };
        groups
            .iter_mut()
            .find(|g| g.group == r.group)
            .expect("group inserted above")
            .cores
            .push(core);

        physical_cores = physical_cores.saturating_add(1);
        logical_cpus = logical_cpus.saturating_add(popcount as u16);
    }

    Ok(Topology { groups, physical_cores, logical_cpus })
}
