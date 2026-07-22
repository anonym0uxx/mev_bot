//! Leaf: schema. Verifies the versioned table-definition constants satisfy their
//! structural invariants and cover exactly the §29.9 table set.

use pump_quant_memory::schema::{
    validate_schema, ColumnDef, ColumnType, SchemaError, TableDefinition, ALL_TABLES,
    SCHEMA_VERSION,
};

#[test]
fn all_ten_tables_present_with_expected_names() {
    // §29.9 enumerates exactly these tables (the store subset owned by this crate).
    let expected = [
        "hypotheses",
        "experiments",
        "results",
        "social_calls",
        "call_markouts",
        "source_quality_ledger",
        "amplification_edges",
        "meta_categories",
        "category_assignments",
        "meta_rotation_snapshots",
    ];
    let got: Vec<&str> = ALL_TABLES.iter().map(|t| t.name).collect();
    assert_eq!(got.len(), 10, "expected 10 tables");
    assert_eq!(got, expected, "table set/order must match §29.9");
}

#[test]
fn whole_schema_validates() {
    assert_eq!(validate_schema(), Ok(()));
}

#[test]
fn every_table_is_well_formed() {
    for t in ALL_TABLES {
        assert_eq!(t.validate(), Ok(()), "table {} invalid", t.name);
        assert_eq!(t.version, SCHEMA_VERSION);
        // Exactly-one-or-more primary key, all non-null.
        let pk_count = t.columns.iter().filter(|c| c.primary_key).count();
        assert!(pk_count >= 1, "table {} has no primary key", t.name);
        assert!(
            t.columns.iter().all(|c| !(c.primary_key && c.nullable)),
            "table {} has a nullable pk",
            t.name
        );
        // Column names unique.
        let mut names: Vec<&str> = t.columns.iter().map(|c| c.name).collect();
        let n = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), n, "table {} has duplicate columns", t.name);
        // No floating point anywhere in the schema (§22) — the enum has no float
        // variant, so this is a structural guarantee; assert the column count is
        // non-trivial to catch an accidentally-empty table.
        assert!(!t.columns.is_empty());
    }
}

#[test]
fn duplicate_column_is_rejected() {
    const BAD: TableDefinition = TableDefinition {
        name: "bad",
        version: SCHEMA_VERSION,
        columns: &[
            ColumnDef {
                name: "id",
                ty: ColumnType::U64,
                primary_key: true,
                nullable: false,
            },
            ColumnDef {
                name: "id",
                ty: ColumnType::U64,
                primary_key: false,
                nullable: false,
            },
        ],
    };
    assert_eq!(BAD.validate(), Err(SchemaError::DuplicateColumn));
}

#[test]
fn missing_primary_key_is_rejected() {
    const BAD: TableDefinition = TableDefinition {
        name: "bad",
        version: SCHEMA_VERSION,
        columns: &[ColumnDef {
            name: "x",
            ty: ColumnType::U64,
            primary_key: false,
            nullable: false,
        }],
    };
    assert_eq!(BAD.validate(), Err(SchemaError::NoPrimaryKey));
}

#[test]
fn nullable_primary_key_is_rejected() {
    const BAD: TableDefinition = TableDefinition {
        name: "bad",
        version: SCHEMA_VERSION,
        columns: &[ColumnDef {
            name: "id",
            ty: ColumnType::U64,
            primary_key: true,
            nullable: true,
        }],
    };
    assert_eq!(BAD.validate(), Err(SchemaError::NullablePrimaryKey));
}

#[test]
fn version_mismatch_is_rejected() {
    const BAD: TableDefinition = TableDefinition {
        name: "bad",
        version: SCHEMA_VERSION + 1,
        columns: &[ColumnDef {
            name: "id",
            ty: ColumnType::U64,
            primary_key: true,
            nullable: false,
        }],
    };
    assert_eq!(BAD.validate(), Err(SchemaError::VersionMismatch));
}

#[test]
fn empty_table_is_rejected() {
    const BAD: TableDefinition = TableDefinition {
        name: "bad",
        version: SCHEMA_VERSION,
        columns: &[],
    };
    assert_eq!(BAD.validate(), Err(SchemaError::NoColumns));
}
