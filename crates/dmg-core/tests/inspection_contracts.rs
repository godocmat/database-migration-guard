use dmg_core::{
    digest::ContentDigest,
    history::{
        HistoryEvidenceError, NativeChecksum, NativeHistoryCoverage, NativeHistoryEvidenceV1,
        NativeHistoryMethod, NativeHistoryRow, NativeHistoryState, Sha384Digest,
        forward_inventory_digest,
    },
    inspection::{
        CatalogField, InspectionEvidenceError, InspectionEvidenceV1, InspectionField,
        InspectionGap, InspectionGapReason, InspectionGapScope, InspectionScope, Observation,
        RelationEstimates, SchemaObservation, SchemaState, TargetId,
    },
    migration::{
        AppliedHistoryBasis, ExecutionMode, MigrationAdapter, MigrationDirection, MigrationFile,
        MigrationIdentity,
    },
    risk::{DatabaseObject, ObjectKind},
    snapshot::{
        ColumnGeneration, ColumnSnapshot, ConstraintKind, ConstraintSnapshot, DependencyEdge,
        DependencyKind, ForeignKeyTarget, IndexKey, IndexSnapshot, PartitionEdge, QualifiedName,
        SchemaSnapshotV1, SnapshotError, TableKind, TableSnapshot,
    },
    source::RepoPath,
};
use serde_json::{Value, json};

fn name(schema: &str, name: &str) -> QualifiedName {
    QualifiedName {
        schema: schema.into(),
        name: name.into(),
    }
}

fn object(name: &str, kind: ObjectKind, member: Option<&str>) -> DatabaseObject {
    DatabaseObject {
        kind,
        schema: Some("app".into()),
        name: name.into(),
        member: member.map(str::to_owned),
    }
}

fn column(name: &str, position: u16) -> ColumnSnapshot {
    ColumnSnapshot {
        name: name.into(),
        position,
        data_type: self::name("pg_catalog", "int8"),
        type_modifier: -1,
        array_dimensions: 0,
        not_null: true,
        identity: None,
        generation: None,
        has_default: false,
        default_definition: CatalogField::NotApplicable,
    }
}

fn index(name: &str, keys: &[&str]) -> IndexSnapshot {
    IndexSnapshot {
        name: name.into(),
        access_method: "btree".into(),
        unique: false,
        primary: false,
        valid: true,
        ready: true,
        nulls_not_distinct: false,
        partial: false,
        keys: keys
            .iter()
            .map(|key| IndexKey::Column {
                name: (*key).into(),
            })
            .collect(),
        included_columns: vec![],
        definition: CatalogField::Unavailable,
    }
}

fn table(table_name: &str) -> TableSnapshot {
    TableSnapshot {
        name: name("app", table_name),
        kind: TableKind::Ordinary,
        is_partition: false,
        columns: vec![column("id", 1), column("tenant_id", 3)],
        indexes: vec![],
        constraints: vec![],
    }
}

fn schemas() -> Vec<String> {
    vec!["app".into(), "public".into()]
}

fn observation(version: Option<u32>, time: u64) -> Observation {
    Observation {
        target_id: TargetId::new("staging-4.eu").unwrap(),
        server_version_num: version,
        observed_at_unix_seconds: time,
    }
}

fn gap(
    scope: InspectionGapScope,
    field: InspectionField,
    reason: InspectionGapReason,
) -> InspectionGap {
    InspectionGap {
        scope,
        field,
        reason,
    }
}

fn snapshot(tables: Vec<TableSnapshot>, gaps: Vec<InspectionGap>) -> SchemaSnapshotV1 {
    SchemaSnapshotV1::new(schemas(), tables, vec![], vec![], gaps).unwrap()
}

#[test]
fn canonical_structure_sorts_independent_rows_and_retains_key_order() {
    let mut parent = table("events");
    parent.kind = TableKind::Partitioned;
    let mut child = table("events_first");
    child.is_partition = true;
    child.indexes = vec![index("tenant_events", &["tenant_id", "id"])];
    child.constraints = vec![ConstraintSnapshot {
        name: "events_fk".into(),
        kind: ConstraintKind::ForeignKey,
        columns: vec!["tenant_id".into(), "id".into()],
        validated: true,
        enforced: true,
        deferrable: false,
        initially_deferred: false,
        references: Some(ForeignKeyTarget::Observed {
            table: parent.name.clone(),
            columns: vec!["tenant_id".into(), "id".into()],
        }),
        definition: CatalogField::Unavailable,
    }];
    let edge = PartitionEdge {
        child: child.name.clone(),
        parent: parent.name.clone(),
        detach_pending: false,
    };
    let dependency = DependencyEdge {
        dependent: object("events_first", ObjectKind::Constraint, Some("events_fk")),
        referenced: object("events", ObjectKind::Relation, None),
        kind: DependencyKind::Normal,
    };
    let make = |mut tables: Vec<TableSnapshot>, reverse: bool| {
        let mut scope = schemas();
        if reverse {
            scope.reverse();
            tables.reverse();
            tables.iter_mut().for_each(|table| table.columns.reverse());
        }
        SchemaSnapshotV1::new(
            scope,
            tables,
            vec![edge.clone()],
            vec![dependency.clone()],
            vec![],
        )
        .unwrap()
    };
    let expected = make(vec![parent.clone(), child.clone()], false);
    let reordered = make(vec![parent.clone(), child.clone()], true);
    let wire = serde_json::to_vec(&expected).unwrap();
    assert_eq!(wire, serde_json::to_vec(&reordered).unwrap());
    assert_eq!(expected.digest().unwrap(), reordered.digest().unwrap());
    let mut incoming = serde_json::to_value(&expected).unwrap();
    incoming["tables"].as_array_mut().unwrap().reverse();
    incoming["tables"][0]["columns"]
        .as_array_mut()
        .unwrap()
        .reverse();
    incoming["future"] = json!(true);
    assert_eq!(
        serde_json::from_value::<SchemaSnapshotV1>(incoming.clone()).unwrap(),
        expected
    );

    child.indexes[0].keys.reverse();
    let changed = make(vec![parent, child], false);
    assert_ne!(changed.digest().unwrap(), expected.digest().unwrap());

    incoming["schema_version"] = json!(2);
    assert!(serde_json::from_value::<SchemaSnapshotV1>(incoming).is_err());
}

#[test]
fn snapshot_distinguishes_absent_and_unknown_expressions_from_coverage() {
    let mut relation = table("events");
    let absent = snapshot(vec![relation.clone()], vec![]);
    relation.columns[0].has_default = true;
    relation.columns[0].default_definition = CatalogField::Unavailable;
    let contention = gap(
        InspectionGapScope::Object {
            object: object("events", ObjectKind::Column, Some("id")),
        },
        InspectionField::DefaultDefinition,
        InspectionGapReason::Contention,
    );
    let unknown = snapshot(
        vec![relation.clone()],
        vec![contention.clone(), contention.clone()],
    );
    let partial = snapshot(
        vec![relation.clone()],
        vec![gap(
            InspectionGapScope::Target,
            InspectionField::Relations,
            InspectionGapReason::Truncated,
        )],
    );
    assert_eq!(unknown.gaps(), &[contention]);
    assert_ne!(unknown, partial);
    assert_eq!(unknown.digest().unwrap(), partial.digest().unwrap());
    assert_ne!(unknown.digest().unwrap(), absent.digest().unwrap());

    relation.columns[0].default_definition = CatalogField::Observed { value: "3".into() };
    assert_ne!(
        snapshot(vec![relation], vec![]).digest().unwrap(),
        unknown.digest().unwrap()
    );
}

#[test]
fn rejects_ambiguous_or_invalid_snapshot_structure_on_creation_and_decode() {
    let first = table("events");
    let mut invalid = first.clone();
    invalid.columns[1].position = invalid.columns[0].position;
    assert_eq!(
        SchemaSnapshotV1::new(schemas(), vec![invalid], vec![], vec![], vec![]),
        Err(SnapshotError::InvalidColumn)
    );
    assert_eq!(
        SchemaSnapshotV1::new(
            schemas(),
            vec![first.clone(), first.clone()],
            vec![],
            vec![],
            vec![]
        ),
        Err(SnapshotError::DuplicateTable)
    );

    let mut indexed = first.clone();
    indexed.indexes.push(index("shared", &["id"]));
    let mut another = table("sessions");
    another.indexes.push(index("shared", &["id"]));
    assert_eq!(
        SchemaSnapshotV1::new(schemas(), vec![indexed, another], vec![], vec![], vec![]),
        Err(SnapshotError::InvalidIndex)
    );

    let mut generated_without_expression = first.clone();
    generated_without_expression.columns[0].generation = Some(ColumnGeneration::Stored);
    assert_eq!(
        SchemaSnapshotV1::new(
            schemas(),
            vec![generated_without_expression],
            vec![],
            vec![],
            vec![]
        ),
        Err(SnapshotError::InvalidColumn)
    );

    let mut wire = serde_json::to_value(snapshot(vec![first], vec![])).unwrap();
    let duplicate = wire["tables"][0].clone();
    wire["tables"].as_array_mut().unwrap().push(duplicate);
    assert!(serde_json::from_value::<SchemaSnapshotV1>(wire).is_err());
}

#[test]
fn inspection_scope_preserves_empty_denied_and_failed_reads() {
    for schemas in [
        vec![],
        vec!["pg_catalog".into()],
        vec!["information_schema".into()],
        vec!["app".into(), "app".into()],
    ] {
        assert_eq!(
            InspectionScope::new(schemas),
            Err(InspectionEvidenceError::InvalidScope)
        );
    }
    assert!(TargetId::new("postgresql://secret@host").is_err());

    let denied = SchemaObservation {
        schema: "hidden".into(),
        state: SchemaState::Denied,
    };
    let missing = SchemaObservation {
        schema: "missing".into(),
        state: SchemaState::Missing,
    };
    let visible = SchemaObservation {
        schema: "app".into(),
        state: SchemaState::Visible,
    };
    let schema = SchemaSnapshotV1::new(vec!["app".into()], vec![], vec![], vec![], vec![]).unwrap();
    let read = InspectionEvidenceV1::new(
        observation(Some(150020), 1),
        vec![denied.clone(), missing.clone(), visible.clone()],
        Some(schema),
        vec![],
        None,
        vec![],
    )
    .unwrap();
    assert_eq!(read.scope(), &[visible, denied, missing]);
    assert_eq!(read.snapshot().unwrap().schemas(), &["app"]);
    assert!(read.snapshot().unwrap().tables().is_empty());
    let mut wire = serde_json::to_value(&read).unwrap();
    assert_eq!(
        serde_json::from_value::<InspectionEvidenceV1>(wire.clone()).unwrap(),
        read
    );
    wire["snapshot"]["schemas"] = json!(["app", "hidden"]);
    assert!(serde_json::from_value::<InspectionEvidenceV1>(wire).is_err());

    let failure = InspectionEvidenceV1::new(
        observation(None, 2),
        vec![SchemaObservation {
            schema: "app".into(),
            state: SchemaState::Unavailable,
        }],
        None,
        vec![],
        None,
        vec![gap(
            InspectionGapScope::Target,
            InspectionField::Connection,
            InspectionGapReason::Deadline,
        )],
    )
    .unwrap();
    assert!(failure.snapshot().is_none());
    assert_eq!(failure.gaps()[0].reason, InspectionGapReason::Deadline);
}

#[test]
fn inspection_estimates_do_not_change_structural_digest() {
    let relation = table("events");
    let scope = schemas()
        .into_iter()
        .map(|schema| SchemaObservation {
            schema,
            state: SchemaState::Visible,
        })
        .collect::<Vec<_>>();
    let schema = snapshot(vec![relation.clone()], vec![]);
    let inspect = |rows, time| {
        InspectionEvidenceV1::new(
            observation(Some(180007), time),
            scope.clone(),
            Some(schema.clone()),
            vec![RelationEstimates {
                relation: relation.name.clone(),
                rows: CatalogField::Estimated { value: rows },
                heap_bytes: CatalogField::Estimated { value: 8192 },
                total_bytes: CatalogField::Unavailable,
            }],
            None,
            vec![],
        )
        .unwrap()
    };
    let initial = inspect(100, 1);
    let later = inspect(200, 2);
    assert_ne!(initial, later);
    assert_eq!(
        initial.snapshot().unwrap().digest().unwrap(),
        later.snapshot().unwrap().digest().unwrap()
    );
}

fn migration_file(version: i64, suffix: &str, direction: MigrationDirection) -> MigrationFile {
    MigrationFile {
        identity: MigrationIdentity::numeric(MigrationAdapter::Sqlx, version),
        path: RepoPath::new(format!("migrations/{version}_test.{suffix}.sql")).unwrap(),
        direction,
        digest: ContentDigest::of(suffix.as_bytes()),
        source_segments: vec![],
        execution: ExecutionMode::SQLX_TRANSACTION,
    }
}

#[test]
fn forward_inventory_binds_raw_bytes_and_paths_without_reverse_files() {
    let first = migration_file(1, "up", MigrationDirection::Forward);
    let second = migration_file(2, "up", MigrationDirection::Forward);
    let down = migration_file(1, "down", MigrationDirection::Down);
    let bind = |files: &[MigrationFile]| forward_inventory_digest(MigrationAdapter::Sqlx, files);
    let expected = bind(&[first.clone(), second.clone()]).unwrap();
    assert_eq!(
        bind(&[second.clone(), down, first.clone()]).unwrap(),
        expected
    );

    let mut changed = second.clone();
    changed.digest = ContentDigest::of(b"new bytes");
    assert_ne!(bind(&[first.clone(), changed]).unwrap(), expected);
    let mut moved = second;
    moved.path = RepoPath::new("new_dir/2_test.up.sql").unwrap();
    assert_ne!(bind(&[first.clone(), moved]).unwrap(), expected);
    assert!(matches!(
        bind(&[first.clone(), first]),
        Err(HistoryEvidenceError::InvalidInventory)
    ));
    assert_ne!(
        forward_inventory_digest(MigrationAdapter::Plain, &[]).unwrap(),
        bind(&[]).unwrap()
    );
}

#[test]
fn history_keeps_ranked_events_and_distinguishes_empty_from_unavailable() {
    let adapter = MigrationAdapter::Flyway;
    let inventory = forward_inventory_digest(adapter, &[]).unwrap();
    let identity = MigrationIdentity::flyway_repeatable("build view").unwrap();
    let row = |rank, state| NativeHistoryRow {
        identity: identity.clone(),
        direction: MigrationDirection::Forward,
        rank: Some(rank),
        state,
        checksum: Some(NativeChecksum::FlywayCrc32 { value: -200 }),
    };
    let read = |coverage, rows, gaps| {
        NativeHistoryEvidenceV1::new(
            observation(Some(170012), 1),
            adapter,
            Some(name("public", "flyway_schema_history")),
            inventory,
            coverage,
            rows,
            gaps,
        )
    };
    let old = row(2, NativeHistoryState::Superseded);
    let new = row(3, NativeHistoryState::Failed);
    let evidence = read(
        NativeHistoryCoverage::Complete,
        vec![new.clone(), old.clone()],
        vec![],
    )
    .unwrap();
    assert_eq!(evidence.rows(), &[old.clone(), new]);
    let wire = serde_json::to_value(&evidence).unwrap();
    assert_eq!(
        wire["rows"][0]["checksum"],
        json!({"kind": "flyway_crc32", "value": -200})
    );
    assert_eq!(
        serde_json::from_value::<NativeHistoryEvidenceV1>(wire).unwrap(),
        evidence
    );
    assert!(matches!(
        read(
            NativeHistoryCoverage::Complete,
            vec![old.clone(), old],
            vec![]
        ),
        Err(HistoryEvidenceError::AmbiguousRows)
    ));

    let history_gap = gap(
        InspectionGapScope::Target,
        InspectionField::History,
        InspectionGapReason::PermissionDenied,
    );
    let empty = read(NativeHistoryCoverage::Complete, vec![], vec![]).unwrap();
    let unavailable = read(
        NativeHistoryCoverage::Unavailable,
        vec![],
        vec![history_gap.clone()],
    )
    .unwrap();
    assert_ne!(unavailable.digest().unwrap(), empty.digest().unwrap());
    assert!(matches!(
        read(NativeHistoryCoverage::Complete, vec![], vec![history_gap]),
        Err(HistoryEvidenceError::InvalidCoverage)
    ));
}

#[test]
fn checksum_algorithms_and_manifest_wire_shape_stay_distinct() {
    let digest = ContentDigest::of(b"baseline");
    let manifest = AppliedHistoryBasis::Manifest { digest };
    let expected = json!({ "kind": "manifest", "digest": digest.to_string() });
    assert_eq!(serde_json::to_value(&manifest).unwrap(), expected);
    assert_eq!(
        serde_json::from_value::<AppliedHistoryBasis>(expected).unwrap(),
        manifest
    );

    let native = AppliedHistoryBasis::Native {
        digest,
        inventory_digest: ContentDigest::of(b"forward inventory"),
        adapter: MigrationAdapter::Supabase,
        method: NativeHistoryMethod::for_adapter(MigrationAdapter::Supabase),
    };
    let payload = serde_json::to_value(&native).unwrap();
    assert_eq!(payload["method"], "presence_only");
    assert_eq!(
        serde_json::from_value::<AppliedHistoryBasis>(payload).unwrap(),
        native
    );

    let sha384 = Sha384Digest::of(b"abc");
    assert_eq!(
        sha384.to_string(),
        "cb00753f45a35e8bb5a03d699ac65007272c32ab0eded1631a8b605a43ff5bed8086072ba1e7cc2358baeca134c825a7"
    );
    assert_eq!(
        serde_json::from_value::<Sha384Digest>(json!(sha384.to_string())).unwrap(),
        sha384
    );
    assert!(serde_json::from_value::<Sha384Digest>(Value::String(digest.to_string())).is_err());
    assert!(
        serde_json::from_value::<Sha384Digest>(json!(sha384.to_string().to_uppercase())).is_err()
    );
}
