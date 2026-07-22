//! Versioned QuantMemoryStore schema as checked table-definition constants
//! (§29.9, §56.9).
//!
//! Responsibility: declare the store's tables — their columns, types, nullability,
//! and primary keys — as compile-time constants, together with a `validate`
//! invariant check so the schema itself is machine-verified rather than trusted.
//! Persistence is out of scope `[S]`; these definitions are the contract a live
//! DB layer (server-side) would materialise, and the in-memory
//! [`crate::store::QuantMemoryStore`] carries the matching typed rows.
//!
//! The schema is versioned ([`SCHEMA_VERSION`]): §56.9 requires data to be
//! sealed, versioned, and manifested, and every row carries the version it was
//! written under.

/// Current schema version. Bump on any breaking table-definition change; rows
/// record the version they were written under (§56.9).
pub const SCHEMA_VERSION: u32 = 1;

/// Column scalar type. Deliberately excludes floating point — every stored
/// quantity is integer / fixed-point (§22).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnType {
    /// Unsigned 64-bit integer (ids, timestamps, seconds, sample counts).
    U64,
    /// Unsigned 32-bit integer (schema version, small counts).
    U32,
    /// Signed 128-bit integer (lamports / net-SOL quantities).
    I128,
    /// Signed 64-bit fixed-point basis points (probabilities, returns, shares).
    Bps,
    /// Fixed 32-byte content hash (token/content/name fingerprints).
    Hash32,
    /// A bounded enumerated tag (classification / lifecycle / state).
    Enum,
    /// A boolean flag.
    Bool,
}

/// Definition of a single column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColumnDef {
    /// Column name (unique within its table).
    pub name: &'static str,
    /// Scalar type.
    pub ty: ColumnType,
    /// Whether the column is part of the primary key.
    pub primary_key: bool,
    /// Whether the column may be absent/unknown (§29.5: Unknown stays Unknown).
    pub nullable: bool,
}

impl ColumnDef {
    /// Non-null primary-key column shorthand.
    const fn pk(name: &'static str, ty: ColumnType) -> Self {
        ColumnDef {
            name,
            ty,
            primary_key: true,
            nullable: false,
        }
    }
    /// Non-null value column shorthand.
    const fn col(name: &'static str, ty: ColumnType) -> Self {
        ColumnDef {
            name,
            ty,
            primary_key: false,
            nullable: false,
        }
    }
    /// Nullable value column shorthand.
    const fn nullable(name: &'static str, ty: ColumnType) -> Self {
        ColumnDef {
            name,
            ty,
            primary_key: false,
            nullable: true,
        }
    }
}

/// Definition of a table: its name, schema version, and columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableDefinition {
    /// Table name.
    pub name: &'static str,
    /// Schema version this definition belongs to.
    pub version: u32,
    /// Ordered column definitions.
    pub columns: &'static [ColumnDef],
}

/// A schema-invariant violation found by [`TableDefinition::validate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaError {
    /// The table has no columns.
    NoColumns,
    /// Two columns share a name.
    DuplicateColumn,
    /// The table declares no primary-key column.
    NoPrimaryKey,
    /// A column is both a primary key and nullable (contradiction).
    NullablePrimaryKey,
    /// The table version does not match [`SCHEMA_VERSION`].
    VersionMismatch,
}

impl TableDefinition {
    /// Check the table's structural invariants (§29.9 schema is checked, not
    /// trusted): non-empty, unique column names, at least one primary-key column,
    /// no nullable primary key, and version equal to [`SCHEMA_VERSION`].
    ///
    /// # Errors
    /// The first [`SchemaError`] found, or `Ok(())` if the table is well-formed.
    pub const fn validate(&self) -> Result<(), SchemaError> {
        if self.version != SCHEMA_VERSION {
            return Err(SchemaError::VersionMismatch);
        }
        if self.columns.is_empty() {
            return Err(SchemaError::NoColumns);
        }
        let mut has_pk = false;
        let mut i = 0;
        while i < self.columns.len() {
            let c = self.columns[i];
            if c.primary_key {
                has_pk = true;
                if c.nullable {
                    return Err(SchemaError::NullablePrimaryKey);
                }
            }
            // O(n^2) duplicate-name scan; table column counts are tiny and this
            // runs in const context / tests only.
            let mut j = i + 1;
            while j < self.columns.len() {
                if str_eq(self.columns[i].name, self.columns[j].name) {
                    return Err(SchemaError::DuplicateColumn);
                }
                j += 1;
            }
            i += 1;
        }
        if !has_pk {
            return Err(SchemaError::NoPrimaryKey);
        }
        Ok(())
    }
}

/// `const`-evaluable byte-wise string equality (`str::eq` is not `const` here).
const fn str_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

use ColumnType::{Bool, Bps, Enum, Hash32, I128, U32, U64};

/// `hypotheses` — registered research hypotheses with VOI inputs (§56.10).
pub const HYPOTHESES: TableDefinition = TableDefinition {
    name: "hypotheses",
    version: SCHEMA_VERSION,
    columns: &[
        ColumnDef::pk("id", U64),
        ColumnDef::col("schema_version", U32),
        ColumnDef::col("statement_hash", Hash32),
        ColumnDef::col("expected_impact_lamports", I128),
        ColumnDef::col("prob_true_bps", Bps),
        ColumnDef::col("cost_to_test_lamports", U64),
        ColumnDef::col("edge_half_life_secs", U64),
        ColumnDef::col("status", Enum),
    ],
};

/// `experiments` — registered, immutable-once-sealed experiments (§56.1).
pub const EXPERIMENTS: TableDefinition = TableDefinition {
    name: "experiments",
    version: SCHEMA_VERSION,
    columns: &[
        ColumnDef::pk("id", U64),
        ColumnDef::col("hypothesis_id", U64),
        ColumnDef::col("schema_version", U32),
        ColumnDef::col("title_hash", Hash32),
        ColumnDef::col("causal_mechanism_hash", Hash32),
        ColumnDef::col("dataset_hash", Hash32),
        ColumnDef::col("config_hash", U64),
        ColumnDef::col("created_at_ns", U64),
        ColumnDef::col("sealed", Bool),
        ColumnDef::nullable("seal_hash", U64),
    ],
};

/// `results` — reconciled experiment outcomes (§56.10).
pub const RESULTS: TableDefinition = TableDefinition {
    name: "results",
    version: SCHEMA_VERSION,
    columns: &[
        ColumnDef::pk("id", U64),
        ColumnDef::col("experiment_id", U64),
        ColumnDef::col("net_sol_effect_lamports", I128),
        ColumnDef::col("significance_bps", Bps),
        ColumnDef::col("outcome", Enum),
        ColumnDef::col("reconciled_at_ns", U64),
    ],
};

/// `social_calls` — attributable social/alpha calls (§29.8).
pub const SOCIAL_CALLS: TableDefinition = TableDefinition {
    name: "social_calls",
    version: SCHEMA_VERSION,
    columns: &[
        ColumnDef::pk("id", U64),
        ColumnDef::col("source_id", U64),
        ColumnDef::col("token_hash", Hash32),
        ColumnDef::col("captured_at_ns", U64),
        ColumnDef::col("content_hash", Hash32),
        ColumnDef::col("timing", Enum),
    ],
};

/// `call_markouts` — reconciled forward-executable returns per call/horizon
/// (§29.8 D1).
pub const CALL_MARKOUTS: TableDefinition = TableDefinition {
    name: "call_markouts",
    version: SCHEMA_VERSION,
    columns: &[
        ColumnDef::pk("id", U64),
        ColumnDef::col("call_id", U64),
        ColumnDef::col("horizon", Enum),
        ColumnDef::col("executable_return_bps", Bps),
    ],
};

/// `source_quality_ledger` — per-source classification scorecards (§29.8).
pub const SOURCE_QUALITY_LEDGER: TableDefinition = TableDefinition {
    name: "source_quality_ledger",
    version: SCHEMA_VERSION,
    columns: &[
        ColumnDef::pk("source_id", U64),
        ColumnDef::col("classification", Enum),
        ColumnDef::col("confidence_bps", Bps),
        ColumnDef::col("sample_size", U32),
        ColumnDef::col("mean_markout_30m_bps", Bps),
        ColumnDef::col("updated_at_ns", U64),
    ],
};

/// `amplification_edges` — timestamped directed amplification-graph edges
/// (§29.7).
pub const AMPLIFICATION_EDGES: TableDefinition = TableDefinition {
    name: "amplification_edges",
    version: SCHEMA_VERSION,
    columns: &[
        ColumnDef::pk("id", U64),
        ColumnDef::col("from_source", U64),
        ColumnDef::col("to_source", U64),
        ColumnDef::col("token_hash", Hash32),
        ColumnDef::col("observed_at_ns", U64),
        ColumnDef::col("kind", Enum),
    ],
};

/// `meta_categories` — narrative metas and their lifecycle state (§21.4 / §29.9).
pub const META_CATEGORIES: TableDefinition = TableDefinition {
    name: "meta_categories",
    version: SCHEMA_VERSION,
    columns: &[
        ColumnDef::pk("id", U64),
        ColumnDef::col("name_hash", Hash32),
        ColumnDef::col("lifecycle", Enum),
        ColumnDef::col("updated_at_ns", U64),
    ],
};

/// `category_assignments` — token-to-meta assignments with confidence (§29.9).
pub const CATEGORY_ASSIGNMENTS: TableDefinition = TableDefinition {
    name: "category_assignments",
    version: SCHEMA_VERSION,
    columns: &[
        ColumnDef::pk("id", U64),
        ColumnDef::col("category_id", U64),
        ColumnDef::col("token_hash", Hash32),
        ColumnDef::col("confidence_bps", Bps),
        ColumnDef::col("assigned_at_ns", U64),
    ],
};

/// `meta_rotation_snapshots` — point-in-time meta rotation readings (§29.9).
pub const META_ROTATION_SNAPSHOTS: TableDefinition = TableDefinition {
    name: "meta_rotation_snapshots",
    version: SCHEMA_VERSION,
    columns: &[
        ColumnDef::pk("id", U64),
        ColumnDef::col("category_id", U64),
        ColumnDef::col("taken_at_ns", U64),
        ColumnDef::col("lifecycle", Enum),
        ColumnDef::col("launch_share_bps", Bps),
    ],
};

/// All table definitions in the schema, in a stable order (§29.9). Iteration
/// order is fixed and deterministic (§22).
pub const ALL_TABLES: &[TableDefinition] = &[
    HYPOTHESES,
    EXPERIMENTS,
    RESULTS,
    SOCIAL_CALLS,
    CALL_MARKOUTS,
    SOURCE_QUALITY_LEDGER,
    AMPLIFICATION_EDGES,
    META_CATEGORIES,
    CATEGORY_ASSIGNMENTS,
    META_ROTATION_SNAPSHOTS,
];

/// Validate every table in [`ALL_TABLES`] and additionally check that no two
/// tables share a name.
///
/// # Errors
/// The first [`SchemaError`] encountered.
pub fn validate_schema() -> Result<(), SchemaError> {
    let mut i = 0;
    while i < ALL_TABLES.len() {
        ALL_TABLES[i].validate()?;
        let mut j = i + 1;
        while j < ALL_TABLES.len() {
            if str_eq(ALL_TABLES[i].name, ALL_TABLES[j].name) {
                return Err(SchemaError::DuplicateColumn);
            }
            j += 1;
        }
        i += 1;
    }
    Ok(())
}
