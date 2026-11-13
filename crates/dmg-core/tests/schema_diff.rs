use dmg_core::{
    coverage::CoverageStatus,
    diff::{
        ChangedField, DiffError, DiffGap, DiffSide, RenameMapping, SchemaComparisonV1, ScopeState,
        StructuralChange, compare_schemas, compare_snapshots,
    },
    inspection::{
        CatalogField, InspectionEvidenceV1, InspectionField, InspectionGap, InspectionGapReason,
        InspectionGapScope, Observation, SchemaObservation, SchemaState, TargetId,
    },
    risk::{DatabaseObject, ObjectKind},
    snapshot::{
        ColumnSnapshot, ConstraintKind, ConstraintSnapshot, DependencyEdge, DependencyKind,
        ForeignKeyTarget, IndexKey, IndexSnapshot, PartitionEdge, QualifiedName, SchemaSnapshotV1,
        TableKind, TableSnapshot,
    },
};

fn name(schema: &str, table: &str) -> QualifiedName {
    QualifiedName {
        schema: schema.into(),
        name: table.into(),
    }
}

fn object(kind: ObjectKind, schema: &str, table: &str, member: Option<&str>) -> DatabaseObject {
    DatabaseObject {
        kind,
        schema: Some(schema.into()),
        name: table.into(),
        member: member.map(str::to_owned),
    }
}

fn table_id(table: &TableSnapshot) -> DatabaseObject {
    object(
        ObjectKind::Relation,
        &table.name.schema,
        &table.name.name,
        None,
    )
}

fn column_id(table: &TableSnapshot, column: &str) -> DatabaseObject {
    object(
        ObjectKind::Column,
        &table.name.schema,
        &table.name.name,
        Some(column),
    )
}

fn index_id(schema: &str, index: &str) -> DatabaseObject {
    object(ObjectKind::Index, schema, index, None)
}

fn observed(value: &str) -> CatalogField<String> {
    CatalogField::Observed {
        value: value.into(),
    }
}

fn column(column: &str, position: u16) -> ColumnSnapshot {
    ColumnSnapshot {
        name: column.into(),
        position,
        data_type: name("pg_catalog", "int4"),
        type_modifier: -1,
        array_dimensions: 0,
        not_null: false,
        identity: None,
        generation: None,
        has_default: false,
        default_definition: CatalogField::NotApplicable,
    }
}

fn table(schema: &str, table: &str, columns: Vec<ColumnSnapshot>) -> TableSnapshot {
    TableSnapshot {
        name: name(schema, table),
        kind: TableKind::Ordinary,
        is_partition: false,
        columns,
        indexes: Vec::new(),
        constraints: Vec::new(),
    }
}

fn index(index: &str, key: &str, definition: CatalogField<String>) -> IndexSnapshot {
    IndexSnapshot {
        name: index.into(),
        access_method: "btree".into(),
        unique: false,
        primary: false,
        valid: true,
        ready: true,
        nulls_not_distinct: false,
        partial: false,
        keys: vec![IndexKey::Column { name: key.into() }],
        included_columns: Vec::new(),
        definition,
    }
}

fn constraint(
    name: &str,
    columns: &[&str],
    definition: CatalogField<String>,
) -> ConstraintSnapshot {
    ConstraintSnapshot {
        name: name.into(),
        kind: ConstraintKind::Check,
        columns: columns.iter().map(|value| (*value).into()).collect(),
        validated: true,
        enforced: true,
        deferrable: false,
        initially_deferred: false,
        references: None,
        definition,
    }
}

fn snapshot(
    schemas: &[&str],
    tables: Vec<TableSnapshot>,
    gaps: Vec<InspectionGap>,
) -> SchemaSnapshotV1 {
    SchemaSnapshotV1::new(
        schemas.iter().map(|schema| (*schema).into()).collect(),
        tables,
        Vec::new(),
        Vec::new(),
        gaps,
    )
    .unwrap()
}

fn inspection(
    scopes: &[(&str, SchemaState)],
    snapshot: Option<SchemaSnapshotV1>,
    gaps: Vec<InspectionGap>,
) -> InspectionEvidenceV1 {
    InspectionEvidenceV1::new(
        Observation {
            target_id: TargetId::new("staging").unwrap(),
            server_version_num: Some(180007),
            observed_at_unix_seconds: 2000,
        },
        scopes
            .iter()
            .map(|(schema, state)| SchemaObservation {
                schema: (*schema).into(),
                state: *state,
            })
            .collect(),
        snapshot,
        Vec::new(),
        None,
        gaps,
    )
    .unwrap()
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

fn object_gap(object: DatabaseObject, field: InspectionField) -> InspectionGap {
    gap(
        InspectionGapScope::Object { object },
        field,
        InspectionGapReason::Unsupported,
    )
}

#[test]
fn ordered_three_way_diff_detects_fields_and_keeps_definition_text_in_source() {
    let mut source = table("app", "Items", vec![column("id", 1), column("score", 2)]);
    source.columns[1].has_default = true;
    source.columns[1].default_definition = observed("1");
    source
        .indexes
        .push(index("Ix_score", "score", observed("private text #1")));
    source.constraints.push(constraint(
        "score_range",
        &["score"],
        observed("CHECK (score > 0)"),
    ));

    let mut current = source.clone();
    current.columns[1].not_null = true;
    current.columns[1].default_definition = observed("2");
    current.indexes[0].unique = true;
    current.indexes[0].definition = observed("private text #2");
    current.constraints[0].validated = false;

    let mut target = current.clone();
    target.columns[1].data_type = name("pg_catalog", "int8");
    target.columns.push(column("Score", 3));

    let lower_case = table("app", "items", vec![]);
    let expected = snapshot(&["audit", "app"], vec![lower_case.clone(), source], vec![]);
    let reversed = snapshot(
        &["app", "audit"],
        vec![current.clone(), lower_case.clone()],
        vec![],
    );
    let current = snapshot(&["audit", "app"], vec![lower_case.clone(), current], vec![]);
    let target = snapshot(&["app", "audit"], vec![target, lower_case], vec![]);
    let comparison = compare_schemas(&expected, &current, &target, &[], &[]).unwrap();
    assert_eq!(
        comparison,
        compare_schemas(&expected, &reversed, &target, &[], &[]).unwrap()
    );
    let drift = &comparison.expected_to_current;
    assert_eq!(drift.coverage, CoverageStatus::Complete);
    assert!(drift.same_requested_scope);
    let changes: Vec<_> = drift
        .changes
        .iter()
        .map(|change| match change {
            StructuralChange::Modified {
                before,
                after,
                fields,
            } => {
                assert_eq!(before, after);
                (after.identity.kind, fields.as_slice())
            }
            _ => panic!("expected field changes, got {change:?}"),
        })
        .collect();
    assert_eq!(
        changes,
        vec![
            (
                ObjectKind::Column,
                &[ChangedField::Nullability, ChangedField::DefaultDefinition][..]
            ),
            (
                ObjectKind::Index,
                &[ChangedField::Unique, ChangedField::IndexDefinition][..]
            ),
            (ObjectKind::Constraint, &[ChangedField::Validated][..]),
        ]
    );
    let proposed = &comparison.current_to_target;
    assert!(matches!(proposed.changes.as_slice(), [
        StructuralChange::Modified { fields, .. },
        StructuralChange::Added { object },
    ] if fields == &[ChangedField::DataType] && object.identity.member.as_deref() == Some("Score")));
    assert_eq!(
        drift.after.structural_digest,
        proposed.before.structural_digest
    );
    let json = serde_json::to_string(&comparison).unwrap();
    assert!(!json.contains("private text"));
    assert_eq!(
        serde_json::from_str::<SchemaComparisonV1>(&json).unwrap(),
        comparison
    );
    assert!(
        serde_json::from_str::<SchemaComparisonV1>(&json.replacen(
            r#""schema_version":1"#,
            r#""schema_version":2"#,
            1,
        ))
        .is_err()
    );
}

#[test]
fn denied_and_unavailable_schemas_do_not_establish_absence() {
    let expected = snapshot(
        &["app", "audit"],
        vec![
            table("app", "visible", vec![]),
            table("audit", "masked", vec![]),
        ],
        vec![],
    );
    let subset = snapshot(&["app"], vec![table("app", "visible", vec![])], vec![]);
    for state in [SchemaState::Denied, SchemaState::Unavailable] {
        let current = inspection(
            &[("app", SchemaState::Visible), ("audit", state)],
            Some(subset.clone()),
            vec![],
        );
        let diff = compare_snapshots(&expected, &current, &[]).unwrap();
        assert!(diff.same_requested_scope);
        assert_eq!(diff.schemas[1].after, ScopeState::from(state));
        assert_eq!(diff.coverage, CoverageStatus::Incomplete);
        assert!(diff.changes.is_empty());
        assert!(matches!(&diff.gaps[..], [DiffGap::PresenceUnconfirmed {
            missing_from: DiffSide::After, object,
        }] if object.identity.name == "masked"));
    }

    let diff = compare_snapshots(&expected, &subset, &[]).unwrap();
    assert!(!diff.same_requested_scope);
    assert_eq!(diff.schemas[1].after, ScopeState::OutsideScope);
    assert!(diff.changes.is_empty());

    let current = inspection(&[("app", SchemaState::Visible)], None, vec![]);
    let diff = compare_snapshots(&expected, &current, &[]).unwrap();
    assert!(diff.changes.is_empty());
    assert!(diff.gaps.contains(&DiffGap::SnapshotUnavailable {
        side: DiffSide::After
    }));
}

#[test]
fn an_observed_missing_schema_and_visible_empty_inventory_establish_removals() {
    let expected = snapshot(
        &["app", "audit"],
        vec![
            table("app", "remove_table", vec![column("id", 1)]),
            table("audit", "remove_with_schema", vec![column("id", 1)]),
        ],
        vec![],
    );
    let current = inspection(
        &[
            ("audit", SchemaState::Missing),
            ("app", SchemaState::Visible),
        ],
        Some(snapshot(&["app"], vec![], vec![])),
        vec![gap(
            InspectionGapScope::Schema {
                schema: "audit".into(),
            },
            InspectionField::Scope,
            InspectionGapReason::Missing,
        )],
    );
    let diff = compare_snapshots(&expected, &current, &[]).unwrap();
    assert_eq!(diff.coverage, CoverageStatus::Complete);
    assert!(diff.same_requested_scope);
    assert_eq!(diff.after.observation, Some(current.observation().clone()));
    assert_eq!(diff.changes.len(), 3);
    assert!(diff.changes.iter().all(|change| matches!(change,
        StructuralChange::Removed { object } if matches!(object.identity.kind, ObjectKind::Relation | ObjectKind::Schema)
    )));
}

#[test]
fn scope_gap_can_mask_absence_and_unavailable_definition_preserves_observed_changes() {
    let mut before = table(
        "app",
        "items",
        vec![column("score", 1), column("removed", 2)],
    );
    before.columns[0].has_default = true;
    before.columns[0].default_definition = observed("1");
    before
        .indexes
        .push(index("score_idx", "score", observed("definition")));
    before.constraints.push(constraint(
        "check_score",
        &["score"],
        observed("definition"),
    ));

    let mut after = before.clone();
    after.columns[0].default_definition = CatalogField::Unavailable;
    after.columns[0].not_null = true;
    after.columns.pop();
    after.indexes.clear();
    after.constraints[0].columns.clear();
    after.constraints[0].validated = false;

    let current_gaps = vec![
        gap(
            InspectionGapScope::Target,
            InspectionField::Columns,
            InspectionGapReason::Truncated,
        ),
        object_gap(table_id(&after), InspectionField::Indexes),
        object_gap(
            object(ObjectKind::Constraint, "app", "items", Some("check_score")),
            InspectionField::Constraints,
        ),
    ];
    let expected = snapshot(&["app"], vec![before], vec![]);
    let current = snapshot(&["app"], vec![after], current_gaps);
    let diff = compare_snapshots(&expected, &current, &[]).unwrap();
    assert_eq!(diff.coverage, CoverageStatus::Incomplete);
    assert!(diff.same_requested_scope);
    assert!(matches!(diff.changes.as_slice(), [
        StructuralChange::Modified { fields: column, .. },
        StructuralChange::Modified { fields: constraint, .. },
    ] if column == &[ChangedField::Nullability] && constraint == &[ChangedField::Validated]));
    for expected in [
        ChangedField::DefaultDefinition,
        ChangedField::ConstraintColumns,
    ] {
        assert!(
            diff.gaps
                .iter()
                .any(|gap| matches!(gap, DiffGap::FieldUnavailable {
            side: DiffSide::After, field, ..
        } if *field == expected))
        );
    }
    let uncertain: Vec<_> = diff
        .gaps
        .iter()
        .filter_map(|gap| match gap {
            DiffGap::PresenceUnconfirmed {
                missing_from: DiffSide::After,
                object,
            } => Some(object.identity.kind),
            _ => None,
        })
        .collect();
    assert_eq!(uncertain, vec![ObjectKind::Column, ObjectKind::Index]);
}

#[test]
fn unrelated_projection_gaps_preserve_known_member_and_table_absence() {
    let before = table("app", "items", vec![column("id", 1), column("removed", 2)]);
    let after = table("app", "items", vec![column("id", 1)]);
    let expected = snapshot(&["app"], vec![before], vec![]);
    let current = snapshot(
        &["app"],
        vec![after],
        vec![
            gap(
                InspectionGapScope::Target,
                InspectionField::Relations,
                InspectionGapReason::Truncated,
            ),
            gap(
                InspectionGapScope::Target,
                InspectionField::UnsupportedObjects,
                InspectionGapReason::Unsupported,
            ),
        ],
    );
    let diff = compare_snapshots(&expected, &current, &[]).unwrap();
    assert!(
        matches!(&diff.changes[..], [StructuralChange::Removed { object }]
        if object.identity.member.as_deref() == Some("removed"))
    );

    let expected = snapshot(&["app"], vec![table("app", "removed", vec![])], vec![]);
    let current = snapshot(
        &["app"],
        vec![],
        vec![gap(
            InspectionGapScope::Target,
            InspectionField::UnsupportedObjects,
            InspectionGapReason::Unsupported,
        )],
    );
    let diff = compare_snapshots(&expected, &current, &[]).unwrap();
    assert!(
        matches!(&diff.changes[..], [StructuralChange::Removed { object }]
        if object.identity.kind == ObjectKind::Relation)
    );
}

#[test]
fn empty_scopes_and_unavailable_fields_remain_incomplete_even_with_equal_digests() {
    let empty = snapshot(&[], vec![], vec![]);
    let diff = compare_snapshots(&empty, &empty, &[]).unwrap();
    assert_eq!(diff.coverage, CoverageStatus::Incomplete);
    assert_eq!(diff.gaps, vec![DiffGap::EmptyScope]);

    let mut value = table("app", "items", vec![column("id", 1)]);
    value
        .indexes
        .push(index("items_idx", "id", CatalogField::Unavailable));
    let incomplete = snapshot(&["app"], vec![value], vec![]);
    let diff = compare_snapshots(&incomplete, &incomplete, &[]).unwrap();
    assert_eq!(diff.before.structural_digest, diff.after.structural_digest);
    assert_eq!(diff.coverage, CoverageStatus::Incomplete);
    assert!(diff.changes.is_empty());
    assert_eq!(diff.gaps.len(), 2);
}

#[test]
fn ambiguous_identity_stays_add_remove_until_reviewed_mapping_aligns_members() {
    let mut before = table("app", "old", vec![column("old_id", 1)]);
    before
        .indexes
        .push(index("items_idx", "old_id", observed("index definition")));
    let mut after = before.clone();
    after.name.name = "new".into();
    after.columns[0].name = "new_id".into();
    after.indexes[0].keys = vec![IndexKey::Column {
        name: "new_id".into(),
    }];

    let expected = snapshot(&["app"], vec![before.clone()], vec![]);
    let current = snapshot(&["app"], vec![after.clone()], vec![]);
    let diff = compare_snapshots(&expected, &current, &[]).unwrap();
    assert!(
        matches!(&diff.changes[..], [StructuralChange::Removed { object: left }, StructuralChange::Added { object: right }]
        if left.identity == table_id(&before) && right.identity == table_id(&after))
    );

    let table_rename = RenameMapping {
        from: table_id(&before),
        to: table_id(&after),
    };
    let column_rename = RenameMapping {
        from: column_id(&before, "old_id"),
        to: column_id(&after, "new_id"),
    };
    let diff = compare_snapshots(&expected, &current, &[column_rename, table_rename]).unwrap();
    assert_eq!(diff.coverage, CoverageStatus::Complete);
    assert_eq!(diff.changes.len(), 2);
    assert!(
        diff.changes
            .iter()
            .all(|change| matches!(change, StructuralChange::Renamed { .. }))
    );
}

#[test]
fn invalid_renames_reject_coverage_conflicts_and_owner_changes() {
    let mut left = table("app", "left", vec![column("id", 1)]);
    let mut right = table("app", "right", vec![column("id", 1)]);
    left.indexes.push(index("ix_old", "id", observed("index")));
    right.indexes.push(index("ix_new", "id", observed("index")));
    let expected = snapshot(
        &["app"],
        vec![left.clone(), table("app", "right", vec![])],
        vec![],
    );
    let current = snapshot(&["app"], vec![right, table("app", "left", vec![])], vec![]);
    let moved = RenameMapping {
        from: index_id("app", "ix_old"),
        to: index_id("app", "ix_new"),
    };
    assert!(matches!(
        compare_snapshots(&expected, &current, std::slice::from_ref(&moved)),
        Err(DiffError::RenameOwnerOrKind { index: 1 })
    ));
    assert!(matches!(
        compare_snapshots(&expected, &current, &[moved.clone(), moved]),
        Err(DiffError::ConflictingRename { index: 2 })
    ));

    let old = table("app", "old", vec![]);
    let new = table("app", "new", vec![]);
    let expected = snapshot(&["app"], vec![old.clone()], vec![]);
    let current = snapshot(
        &["app"],
        vec![new.clone()],
        vec![gap(
            InspectionGapScope::Target,
            InspectionField::Relations,
            InspectionGapReason::Truncated,
        )],
    );
    let mapping = RenameMapping {
        from: table_id(&old),
        to: table_id(&new),
    };
    assert!(matches!(
        compare_snapshots(&expected, &current, std::slice::from_ref(&mapping)),
        Err(DiffError::UnverifiedRename { index: 1 })
    ));
    let overlap = snapshot(&["app"], vec![new, old], vec![]);
    assert!(matches!(
        compare_snapshots(&expected, &overlap, &[mapping]),
        Err(DiffError::UnverifiedRename { index: 1 })
    ));
}

#[test]
fn compares_partition_and_foreign_key_edges_with_field_specific_coverage() {
    let mut child = table("app", "child", vec![column("owner_id", 1)]);
    child.is_partition = true;
    let mut parent = table("app", "parent", vec![column("id", 1)]);
    parent.kind = TableKind::Partitioned;
    let other = table("app", "alternate", vec![column("id", 1)]);
    let mut fk = constraint(
        "parent_fk",
        &["owner_id"],
        observed("FOREIGN KEY (owner_id) REFERENCES parent (id)"),
    );
    fk.kind = ConstraintKind::ForeignKey;
    fk.references = Some(ForeignKeyTarget::Observed {
        table: parent.name.clone(),
        columns: vec!["id".into()],
    });
    child.constraints.push(fk);
    let partition = PartitionEdge {
        child: child.name.clone(),
        parent: parent.name.clone(),
        detach_pending: false,
    };
    let dep = DependencyEdge {
        dependent: object(ObjectKind::Constraint, "app", "child", Some("parent_fk")),
        referenced: table_id(&parent),
        kind: DependencyKind::Normal,
    };
    let expected = SchemaSnapshotV1::new(
        vec!["app".into()],
        vec![child.clone(), parent.clone(), other.clone()],
        vec![partition.clone()],
        vec![dep.clone()],
        vec![],
    )
    .unwrap();

    child.constraints[0].references = Some(ForeignKeyTarget::Observed {
        table: other.name.clone(),
        columns: vec!["id".into()],
    });
    child.constraints[0].definition = observed("FOREIGN KEY (owner_id) REFERENCES alternate (id)");
    let new_dep = DependencyEdge {
        referenced: table_id(&other),
        ..dep.clone()
    };
    let detaching = PartitionEdge {
        detach_pending: true,
        ..partition.clone()
    };
    let current = SchemaSnapshotV1::new(
        vec!["app".into()],
        vec![child.clone(), parent.clone(), other.clone()],
        vec![detaching.clone()],
        vec![new_dep.clone()],
        vec![],
    )
    .unwrap();
    let diff = compare_snapshots(&expected, &current, &[]).unwrap();
    assert!(matches!(&diff.changes[..], [
        StructuralChange::Modified { fields, .. },
        StructuralChange::PartitionModified { before, after },
        StructuralChange::DependencyRemoved { edge: removed },
        StructuralChange::DependencyAdded { edge: added },
    ] if fields == &[ChangedField::ConstraintReferences, ChangedField::ConstraintDefinition]
        && before == &partition && after == &detaching && removed == &dep && added == &new_dep));
    assert_eq!(diff.coverage, CoverageStatus::Complete);

    child.constraints[0].references = Some(ForeignKeyTarget::OutsideScope);
    child.constraints[0].definition = CatalogField::Unavailable;
    let gaps = vec![object_gap(table_id(&child), InspectionField::Partitions)];
    let current = SchemaSnapshotV1::new(
        vec!["app".into()],
        vec![child, parent, other],
        vec![],
        vec![],
        gaps,
    )
    .unwrap();
    let diff = compare_snapshots(&expected, &current, &[]).unwrap();
    assert!(diff.changes.is_empty());
    assert!(diff.gaps.contains(&DiffGap::PartitionPresenceUnconfirmed {
        missing_from: DiffSide::After,
        edge: partition
    }));
    assert!(diff.gaps.contains(&DiffGap::DependencyPresenceUnconfirmed {
        missing_from: DiffSide::After,
        edge: dep
    }));
}
