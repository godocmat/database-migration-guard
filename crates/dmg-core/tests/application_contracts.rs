use dmg_core::{
    application::{
        ApplicationAssessmentV1, ApplicationContractError, ApplicationContractV1, ColumnAccess,
        ColumnRequirement, ColumnType, ContractCheck, ContractFact, ContractStatus,
        ContractTransition, ContractUnknown, ContractViolation, FactAssessment,
        MAX_APPLICATION_CONTRACT_BYTES, NullabilityRequirement, TableRequirement,
        evaluate_contract,
    },
    coverage::CoverageStatus,
    diff::{RenameMapping, SchemaInput, ScopeState, StructuralChange, compare_snapshots},
    inspection::{
        CatalogField, InspectionEvidenceV1, InspectionField, InspectionGap, InspectionGapReason,
        InspectionGapScope, Observation, SchemaObservation, SchemaState, TargetId,
    },
    risk::{DatabaseObject, ObjectKind},
    snapshot::{
        ColumnGeneration, ColumnIdentity, ColumnSnapshot, ConstraintKind, ConstraintSnapshot,
        QualifiedName, SchemaSnapshotV1, TableKind, TableSnapshot,
    },
};
use serde_json::{Value, json};

fn qualified(schema: &str, name: &str) -> QualifiedName {
    QualifiedName {
        schema: schema.into(),
        name: name.into(),
    }
}

fn table_name() -> QualifiedName {
    qualified("app", "Items")
}

fn column(name: &str, position: u16) -> ColumnSnapshot {
    ColumnSnapshot {
        name: name.into(),
        position,
        data_type: qualified("pg_catalog", "int4"),
        type_modifier: -1,
        array_dimensions: 0,
        not_null: false,
        identity: None,
        generation: None,
        has_default: false,
        default_definition: CatalogField::NotApplicable,
    }
}

fn table(name: QualifiedName, columns: Vec<ColumnSnapshot>) -> TableSnapshot {
    TableSnapshot {
        name,
        kind: TableKind::Ordinary,
        is_partition: false,
        columns,
        indexes: Vec::new(),
        constraints: Vec::new(),
    }
}

fn column_type(column: &ColumnSnapshot) -> ColumnType {
    ColumnType {
        name: column.data_type.clone(),
        type_modifier: column.type_modifier,
        array_dimensions: column.array_dimensions,
    }
}

fn requirement(
    column: &ColumnSnapshot,
    access: ColumnAccess,
    nullability: NullabilityRequirement,
) -> ColumnRequirement {
    ColumnRequirement {
        name: column.name.clone(),
        access,
        data_type: column_type(column),
        nullability,
    }
}

fn contract(critical: bool, columns: Vec<ColumnRequirement>) -> ApplicationContractV1 {
    ApplicationContractV1::new(
        "orders-api",
        "2.4.1+build.1",
        critical,
        vec![TableRequirement {
            table: table_name(),
            kind: None,
            columns,
        }],
    )
    .unwrap()
}

fn snapshot(
    schemas: &[&str],
    tables: Vec<TableSnapshot>,
    gaps: Vec<InspectionGap>,
) -> SchemaSnapshotV1 {
    SchemaSnapshotV1::new(
        schemas.iter().map(|schema| (*schema).to_owned()).collect(),
        tables,
        Vec::new(),
        Vec::new(),
        gaps,
    )
    .unwrap()
}

fn evidence(
    major: u32,
    scopes: &[(&str, SchemaState)],
    snapshot: Option<SchemaSnapshotV1>,
    gaps: Vec<InspectionGap>,
) -> InspectionEvidenceV1 {
    InspectionEvidenceV1::new(
        Observation {
            target_id: TargetId::new("staging").unwrap(),
            server_version_num: Some(major * 10_000 + 7),
            observed_at_unix_seconds: 100,
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

fn projection_gap(
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

fn constraint(name: &str, column: &str, validated: bool, enforced: bool) -> ConstraintSnapshot {
    ConstraintSnapshot {
        name: name.into(),
        kind: ConstraintKind::NotNull,
        columns: vec![column.into()],
        validated,
        enforced,
        deferrable: false,
        initially_deferred: false,
        references: None,
        definition: CatalogField::Unavailable,
    }
}

fn member(kind: ObjectKind, name: &str) -> DatabaseObject {
    let table = table_name();
    DatabaseObject {
        kind,
        schema: Some(table.schema),
        name: table.name,
        member: Some(name.into()),
    }
}

fn violation(reason: ContractViolation) -> FactAssessment {
    FactAssessment::Violated { reason }
}

fn unknown(reason: ContractUnknown) -> FactAssessment {
    FactAssessment::Unknown { reason }
}

fn check<'a>(
    assessment: &'a ApplicationAssessmentV1,
    column: Option<&str>,
    matches_fact: impl Fn(&ContractFact) -> bool,
) -> &'a ContractCheck {
    assessment
        .checks
        .iter()
        .find(|check| check.column.as_deref() == column && matches_fact(&check.fact))
        .unwrap()
}

#[test]
fn input_validation_is_strict_and_bounded() {
    let id = column("id", 1);
    let contract = contract(
        false,
        vec![requirement(
            &id,
            ColumnAccess::Read,
            NullabilityRequirement::Any,
        )],
    );
    let document = serde_json::to_value(&contract).unwrap();
    assert_eq!(
        ApplicationContractV1::from_json(document.to_string()).unwrap(),
        contract
    );
    for path in [
        vec!["unexpected"],
        vec!["tables", "0", "table", "unqualified"],
        vec!["tables", "0", "columns", "0", "data_type", "unknown"],
        vec![
            "tables",
            "0",
            "columns",
            "0",
            "data_type",
            "name",
            "unexpected",
        ],
    ] {
        let mut candidate = document.clone();
        let (last, segments) = path.split_last().unwrap();
        let mut object = &mut candidate;
        for segment in segments {
            object = if let Ok(index) = segment.parse::<usize>() {
                &mut object[index]
            } else {
                &mut object[*segment]
            };
        }
        object[*last] = Value::Bool(true);
        assert!(
            ApplicationContractV1::from_json(candidate.to_string()).is_err(),
            "{path:?}"
        );
    }
    for (field, bad) in [
        ("schema_version", json!(2)),
        ("version", json!("")),
        ("tables", json!([])),
    ] {
        let mut candidate = document.clone();
        candidate[field] = bad;
        assert!(ApplicationContractV1::from_json(candidate.to_string()).is_err());
    }
    assert!(matches!(
        ApplicationContractV1::from_json(vec![b' '; MAX_APPLICATION_CONTRACT_BYTES + 1]),
        Err(ApplicationContractError::ByteLimit)
    ));

    let first = contract.tables()[0].clone();
    assert!(matches!(
        ApplicationContractV1::new("app", "1", false, vec![first.clone(), first.clone()]),
        Err(ApplicationContractError::DuplicateTable)
    ));
    let mut repeated = first.clone();
    repeated.columns.push(repeated.columns[0].clone());
    assert!(matches!(
        ApplicationContractV1::new("app", "1", false, vec![repeated]),
        Err(ApplicationContractError::DuplicateColumn)
    ));
    let mut invalid = first.clone();
    invalid.columns[0].name = "bad\nname".into();
    assert!(matches!(
        ApplicationContractV1::new("app", "1", false, vec![invalid]),
        Err(ApplicationContractError::InvalidIdentifier)
    ));
    let mut system = first.clone();
    system.table.schema = "pg_catalog".into();
    assert!(matches!(
        ApplicationContractV1::new("app", "1", false, vec![system]),
        Err(ApplicationContractError::InvalidScope)
    ));
    let mut oversized = first;
    oversized.columns = vec![oversized.columns[0].clone(); 257];
    assert!(matches!(
        ApplicationContractV1::new("app", "1", false, vec![oversized]),
        Err(ApplicationContractError::ColumnLimit)
    ));
}

#[test]
fn ordered_contract_and_assessment_are_deterministic() {
    let id = column("id", 2);
    let label = column("Label", 1);
    let mut requirements = vec![
        TableRequirement {
            table: table_name(),
            kind: Some(TableKind::Ordinary),
            columns: vec![
                requirement(&id, ColumnAccess::Read, NullabilityRequirement::Any),
                requirement(
                    &label,
                    ColumnAccess::ReadWrite,
                    NullabilityRequirement::Nullable,
                ),
            ],
        },
        TableRequirement {
            table: qualified("app", "Events"),
            kind: None,
            columns: Vec::new(),
        },
    ];
    let first = ApplicationContractV1::new("orders-api", "1", false, requirements.clone()).unwrap();
    requirements[0].columns.reverse();
    requirements.reverse();
    let second = ApplicationContractV1::new("orders-api", "1", false, requirements).unwrap();
    assert_eq!(first, second);
    assert_eq!(first.digest().unwrap(), second.digest().unwrap());

    let snapshot = snapshot(
        &["app"],
        vec![
            table(table_name(), vec![id, label]),
            table(qualified("app", "Events"), Vec::new()),
        ],
        Vec::new(),
    );
    let first = evaluate_contract(&first, &snapshot, &snapshot).unwrap();
    let second = evaluate_contract(&second, &snapshot, &snapshot).unwrap();
    assert_eq!(
        serde_json::to_vec(&first).unwrap(),
        serde_json::to_vec(&second).unwrap()
    );
    assert_eq!(first.current.status, ContractStatus::Satisfied);
    assert_eq!(first.target.coverage, CoverageStatus::Complete);
    assert_eq!(first.checks[0].table.name, "Events");
    assert!(
        first
            .checks
            .iter()
            .all(|check| check.transition == ContractTransition::Preserved)
    );
    let wire = serde_json::to_value(first).unwrap();
    assert_eq!(wire["schema_version"], 1);
    assert_eq!(
        wire["checks"][0]["fact"],
        json!({"kind": "table", "expected_kind": null})
    );
}

#[test]
fn exact_qualified_names_and_reviewed_renames_still_require_the_old_column() {
    let old = column("Old", 1);
    let contract = contract(
        true,
        vec![requirement(
            &old,
            ColumnAccess::Read,
            NullabilityRequirement::Any,
        )],
    );
    let current = snapshot(&["app"], vec![table(table_name(), vec![old])], Vec::new());
    let target = snapshot(
        &["app", "other"],
        vec![
            table(table_name(), vec![column("old", 1)]),
            table(qualified("app", "items"), vec![column("Old", 1)]),
            table(qualified("other", "Items"), vec![column("Old", 1)]),
        ],
        Vec::new(),
    );
    let rename = RenameMapping {
        from: member(ObjectKind::Column, "Old"),
        to: member(ObjectKind::Column, "old"),
    };
    let structural = compare_snapshots(&current, &target, &[rename]).unwrap();
    assert!(
        structural
            .changes
            .iter()
            .any(|change| matches!(change, StructuralChange::Renamed { .. }))
    );

    let assessment = evaluate_contract(&contract, &current, &target).unwrap();
    let result = check(&assessment, Some("Old"), |fact| {
        matches!(fact, ContractFact::ColumnType { .. })
    });
    assert_eq!(result.current, FactAssessment::Satisfied);
    assert_eq!(result.target, violation(ContractViolation::MissingColumn));
    assert_eq!(result.transition, ContractTransition::Regression);
    assert!(assessment.critical_target_violation);
    assert_eq!(assessment.target.coverage, CoverageStatus::Complete);
}

#[test]
fn proven_removal_and_missing_scope_have_distinct_outcomes() {
    let contract = contract(false, Vec::new());
    let baseline = snapshot(&["app"], vec![table(table_name(), Vec::new())], Vec::new());
    let visible_empty = snapshot(&["app"], Vec::new(), Vec::new());
    let outside = snapshot(&["other"], Vec::new(), Vec::new());
    let missing = evidence(
        18,
        &[("app", SchemaState::Missing)],
        None,
        vec![projection_gap(
            InspectionGapScope::Schema {
                schema: "app".into(),
            },
            InspectionField::Scope,
            InspectionGapReason::Missing,
        )],
    );
    let denied = evidence(18, &[("app", SchemaState::Denied)], None, Vec::new());
    let unavailable = evidence(18, &[("app", SchemaState::Unavailable)], None, Vec::new());
    let truncated = snapshot(
        &["app"],
        Vec::new(),
        vec![projection_gap(
            InspectionGapScope::Target,
            InspectionField::Relations,
            InspectionGapReason::Truncated,
        )],
    );

    let cases: [(SchemaInput<'_>, FactAssessment); 6] = [
        (
            (&visible_empty).into(),
            violation(ContractViolation::MissingTable),
        ),
        (
            (&missing).into(),
            violation(ContractViolation::MissingTable),
        ),
        (
            (&outside).into(),
            unknown(ContractUnknown::TableUnobserved {
                scope: ScopeState::OutsideScope,
            }),
        ),
        (
            (&denied).into(),
            unknown(ContractUnknown::TableUnobserved {
                scope: ScopeState::Denied,
            }),
        ),
        (
            (&unavailable).into(),
            unknown(ContractUnknown::TableUnobserved {
                scope: ScopeState::Unavailable,
            }),
        ),
        (
            (&truncated).into(),
            unknown(ContractUnknown::TableUnobserved {
                scope: ScopeState::Visible,
            }),
        ),
    ];
    for (target, expected) in cases {
        let assessment = evaluate_contract(&contract, &baseline, target).unwrap();
        assert_eq!(assessment.checks[0].target, expected);
        assert!(!assessment.critical_target_violation);
    }
}

#[test]
fn observed_type_mismatch_and_unobserved_columns_can_coexist() {
    let mut id = column("id", 1);
    id.data_type = qualified("pg_catalog", "_varchar");
    id.type_modifier = 20;
    id.array_dimensions = 1;
    let label = column("label", 2);
    let contract = contract(
        true,
        vec![
            requirement(&id, ColumnAccess::Read, NullabilityRequirement::Any),
            requirement(&label, ColumnAccess::Read, NullabilityRequirement::Any),
        ],
    );
    let original = snapshot(
        &["app"],
        vec![table(table_name(), vec![id.clone(), label])],
        Vec::new(),
    );
    let gap = projection_gap(
        InspectionGapScope::Target,
        InspectionField::Columns,
        InspectionGapReason::Truncated,
    );
    for (data_type, type_modifier, array_dimensions) in [
        (qualified("custom", "_varchar"), 20, 1),
        (qualified("pg_catalog", "_varchar"), 21, 1),
        (qualified("pg_catalog", "_varchar"), 20, 2),
    ] {
        id.data_type = data_type;
        id.type_modifier = type_modifier;
        id.array_dimensions = array_dimensions;
        let target = snapshot(
            &["app"],
            vec![table(table_name(), vec![id.clone()])],
            vec![gap.clone()],
        );
        let assessment = evaluate_contract(&contract, &original, &target).unwrap();
        let get = |name| {
            check(&assessment, Some(name), |fact| {
                matches!(fact, ContractFact::ColumnType { .. })
            })
        };
        assert_eq!(
            get("id").target,
            violation(ContractViolation::ColumnTypeMismatch {
                observed: column_type(&id)
            })
        );
        assert_eq!(get("id").transition, ContractTransition::Regression);
        assert_eq!(
            get("label").target,
            unknown(ContractUnknown::ColumnUnobserved)
        );
        assert_eq!(get("label").transition, ContractTransition::Indeterminate);
        assert_eq!(assessment.target.status, ContractStatus::Violated);
        assert_eq!(assessment.target.coverage, CoverageStatus::Incomplete);
        assert!(assessment.critical_target_violation);
    }
}

#[test]
fn direct_writes_fail_on_generated_and_always_identity_columns() {
    let id = column("id", 1);
    let mirror = column("mirror", 2);
    let view = column("read_only", 3);
    let mut target_columns = vec![id.clone(), mirror.clone(), view.clone()];
    let current = snapshot(
        &["app"],
        vec![table(
            table_name(),
            vec![id.clone(), mirror.clone(), view.clone()],
        )],
        Vec::new(),
    );
    target_columns[0].identity = Some(ColumnIdentity::Always);
    target_columns[0].not_null = true;
    for column in &mut target_columns[1..] {
        column.generation = Some(ColumnGeneration::Stored);
        column.has_default = true;
        column.default_definition = CatalogField::Unavailable;
    }
    let target = snapshot(
        &["app"],
        vec![table(table_name(), target_columns.clone())],
        Vec::new(),
    );
    let contract = contract(
        false,
        vec![
            requirement(&id, ColumnAccess::ReadWrite, NullabilityRequirement::Any),
            requirement(&mirror, ColumnAccess::Write, NullabilityRequirement::Any),
            requirement(&view, ColumnAccess::Read, NullabilityRequirement::Any),
        ],
    );

    let assessment = evaluate_contract(&contract, &current, &target).unwrap();
    for (name, reason) in [
        ("id", ContractViolation::AlwaysIdentity),
        ("mirror", ContractViolation::GeneratedColumn),
    ] {
        let write = check(&assessment, Some(name), |fact| {
            matches!(fact, ContractFact::ExplicitWrite)
        });
        assert_eq!(write.target, violation(reason));
        assert_eq!(write.transition, ContractTransition::Regression);
    }
    assert_eq!(
        check(&assessment, Some("read_only"), |fact| matches!(
            fact,
            ContractFact::ColumnType { .. }
        ))
        .target,
        FactAssessment::Satisfied
    );
    assert!(
        !assessment
            .checks
            .iter()
            .any(|check| check.column.as_deref() == Some("read_only")
                && matches!(check.fact, ContractFact::ExplicitWrite))
    );

    target_columns[0].identity = Some(ColumnIdentity::ByDefault);
    let by_default = snapshot(
        &["app"],
        vec![table(table_name(), target_columns)],
        Vec::new(),
    );
    let assessment = evaluate_contract(&contract, &target, &by_default).unwrap();
    assert_eq!(
        check(&assessment, Some("id"), |fact| matches!(
            fact,
            ContractFact::ExplicitWrite
        ))
        .transition,
        ContractTransition::Resolved
    );
    assert_eq!(
        check(&assessment, Some("mirror"), |fact| matches!(
            fact,
            ContractFact::ExplicitWrite
        ))
        .transition,
        ContractTransition::PreexistingViolation
    );
}

#[test]
fn not_null_uses_explicit_validation_when_the_major_can_defer_it() {
    let mut id = column("id", 1);
    id.not_null = true;
    let mut table = table(table_name(), vec![id.clone()]);
    let contract = contract(
        true,
        vec![requirement(
            &id,
            ColumnAccess::Read,
            NullabilityRequirement::NotNull,
        )],
    );
    let snapshot_without_constraints = snapshot(&["app"], vec![table.clone()], Vec::new());
    let pg17 = evidence(
        17,
        &[("app", SchemaState::Visible)],
        Some(snapshot_without_constraints.clone()),
        Vec::new(),
    );
    let pg18_missing = evidence(
        18,
        &[("app", SchemaState::Visible)],
        Some(snapshot_without_constraints.clone()),
        Vec::new(),
    );
    fn check_not_null(assessment: &ApplicationAssessmentV1) -> &ContractCheck {
        check(assessment, Some("id"), |fact| {
            matches!(fact, ContractFact::Nullability { .. })
        })
    }

    let assessment = evaluate_contract(&contract, &pg17, &pg18_missing).unwrap();
    assert_eq!(
        check_not_null(&assessment).current,
        FactAssessment::Satisfied
    );
    assert_eq!(
        check_not_null(&assessment).target,
        unknown(ContractUnknown::NotNullConstraintUnobserved)
    );
    assert_eq!(
        assessment.current.source.observation,
        Some(pg17.observation().clone())
    );
    assert_eq!(
        assessment.target.source.structural_digest,
        Some(snapshot_without_constraints.digest().unwrap())
    );
    assert!(!assessment.critical_target_violation);

    for (validated, enforced, reason) in [
        (false, true, ContractUnknown::NotNullUnvalidated),
        (true, false, ContractUnknown::NotNullUnenforced),
    ] {
        table.constraints = vec![constraint("id_not_null", "id", validated, enforced)];
        let bare = snapshot(&["app"], vec![table.clone()], Vec::new());
        let pg18 = evidence(
            18,
            &[("app", SchemaState::Visible)],
            Some(bare.clone()),
            Vec::new(),
        );
        for assessment in [
            evaluate_contract(&contract, &pg17, &pg18).unwrap(),
            evaluate_contract(&contract, &snapshot_without_constraints, &bare).unwrap(),
        ] {
            assert_eq!(check_not_null(&assessment).target, unknown(reason));
            assert_eq!(
                check_not_null(&assessment).transition,
                ContractTransition::Indeterminate
            );
            assert_eq!(assessment.target.coverage, CoverageStatus::Incomplete);
        }
    }

    table
        .constraints
        .push(constraint("validated_not_null", "id", true, true));
    let complete = snapshot(
        &["app"],
        vec![table.clone()],
        vec![
            projection_gap(
                InspectionGapScope::Target,
                InspectionField::Constraints,
                InspectionGapReason::Truncated,
            ),
            projection_gap(
                InspectionGapScope::Target,
                InspectionField::ConstraintDefinition,
                InspectionGapReason::PermissionDenied,
            ),
        ],
    );
    assert_eq!(
        evaluate_contract(&contract, &pg17, &complete)
            .unwrap()
            .target
            .status,
        ContractStatus::Satisfied,
    );
    let partial = snapshot(
        &["app"],
        vec![table],
        vec![projection_gap(
            InspectionGapScope::Object {
                object: member(ObjectKind::Constraint, "validated_not_null"),
            },
            InspectionField::Constraints,
            InspectionGapReason::InvalidValue,
        )],
    );
    assert_eq!(
        evaluate_contract(&contract, &pg17, &partial)
            .unwrap()
            .target
            .status,
        ContractStatus::Unknown,
    );
}

#[test]
fn table_kind_and_direct_nullability_are_independent_of_other_gaps() {
    let id = column("id", 1);
    let mut populated = table(table_name(), vec![id.clone()]);
    let current = snapshot(&["app"], vec![populated.clone()], Vec::new());
    let contract = ApplicationContractV1::new(
        "app",
        "1",
        false,
        vec![TableRequirement {
            table: table_name(),
            kind: Some(TableKind::Ordinary),
            columns: vec![requirement(
                &id,
                ColumnAccess::Write,
                NullabilityRequirement::Nullable,
            )],
        }],
    )
    .unwrap();
    populated.kind = TableKind::Partitioned;
    populated.columns[0].not_null = true;
    let target = snapshot(
        &["app"],
        vec![populated],
        vec![
            projection_gap(
                InspectionGapScope::Target,
                InspectionField::UnsupportedObjects,
                InspectionGapReason::Unsupported,
            ),
            projection_gap(
                InspectionGapScope::Target,
                InspectionField::Indexes,
                InspectionGapReason::Truncated,
            ),
        ],
    );
    let assessment = evaluate_contract(&contract, &current, &target).unwrap();
    assert_eq!(assessment.current.status, ContractStatus::Satisfied);
    assert_eq!(assessment.target.coverage, CoverageStatus::Complete);
    assert_eq!(
        assessment.checks[0].target,
        violation(ContractViolation::TableKindMismatch {
            observed: TableKind::Partitioned
        })
    );
    assert_eq!(
        check(&assessment, Some("id"), |fact| matches!(
            fact,
            ContractFact::Nullability { .. }
        ))
        .target,
        violation(ContractViolation::DeclaredNotNull)
    );

    let contract = self::contract(
        false,
        vec![requirement(
            &id,
            ColumnAccess::Read,
            NullabilityRequirement::NotNull,
        )],
    );
    let assessment = evaluate_contract(&contract, &current, &current).unwrap();
    assert_eq!(
        check(&assessment, Some("id"), |fact| matches!(
            fact,
            ContractFact::Nullability { .. }
        ))
        .target,
        violation(ContractViolation::MissingNotNull)
    );
}
