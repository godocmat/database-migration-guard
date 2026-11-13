use std::{env, fs};

use dmg_core::{
    inspection::{
        CatalogField, InspectionField as Field, InspectionGapReason as Reason, InspectionGapScope,
    },
    snapshot::{
        ColumnGeneration, ColumnIdentity, ConstraintKind, ForeignKeyTarget, IndexKey,
        QualifiedName, TableKind,
    },
};

use super::*;

const SCHEMA: &str = "catalog_scope";

#[test]
fn options_require_a_supported_major_and_a_bounded_projection() {
    assert_eq!(CatalogOptions::new(14), Err(CatalogError::UnsupportedMajor));
    assert_eq!(CatalogOptions::new(19), Err(CatalogError::UnsupportedMajor));
    let options = CatalogOptions::new(15).unwrap();
    assert_eq!(
        options.with_row_limit(0),
        Err(CatalogError::InvalidRowLimit)
    );
    assert_eq!(
        options.with_row_limit(MAX_ROW_LIMIT + 1),
        Err(CatalogError::InvalidRowLimit)
    );
    assert!(options.with_row_limit(MAX_ROW_LIMIT).is_ok());
    assert_eq!(options.details, CatalogDetails::default());
}

#[tokio::test]
#[ignore = "requires the disposable PostgreSQL catalog fixture"]
async fn native_catalog() {
    let major: u32 = env::var("DMG_CATALOG_TEST_MAJOR").unwrap().parse().unwrap();
    let scenario = env::var("DMG_CATALOG_TEST_CASE").unwrap();
    let password = env::var("DMG_TEST_POSTGRES_PASSWORD").unwrap();
    let target = InspectionTarget::new(
        "localhost",
        env::var("DMG_TEST_POSTGRES_PORT").unwrap().parse().unwrap(),
        "postgres",
        "dmg_catalog_reader",
        password.as_bytes().to_vec(),
        &fs::read(env::var_os("DMG_TEST_POSTGRES_CA").unwrap()).unwrap(),
    )
    .unwrap();
    let scope = InspectionScope::new(
        [
            SCHEMA,
            "catalog_hidden",
            "catalog_missing",
            "catalog_public",
            "catalog_empty",
            "catalog_Quoted",
        ]
        .map(str::to_owned)
        .to_vec(),
    )
    .unwrap();
    let id = TargetId::new("fixture").unwrap();
    let options = CatalogOptions::new(major).unwrap();
    let details = options.with_details(CatalogDetails {
        definitions: true,
        total_bytes: true,
    });
    let timeouts = InspectionTimeouts::default();

    if scenario == "available" {
        let raw = target
            .inspect_catalog(id.clone(), &scope, options, timeouts)
            .await
            .unwrap();
        assert_common(&raw, major, &password);
        let accounts = raw.snapshot().unwrap().table(&name("accounts")).unwrap();
        let quota = accounts.columns.iter().find(|c| c.name == "quota").unwrap();
        assert!(quota.has_default);
        assert_eq!(quota.default_definition, CatalogField::Unavailable);
        assert_eq!(
            accounts.columns[0].default_definition,
            CatalogField::NotApplicable
        );
        assert!(
            accounts
                .indexes
                .iter()
                .all(|i| i.definition == CatalogField::Unavailable)
        );

        let complete = target
            .inspect_catalog(id.clone(), &scope, details, timeouts)
            .await
            .unwrap();
        assert_common(&complete, major, &password);
        let snapshot = complete.snapshot().unwrap();
        let accounts = snapshot.table(&name("accounts")).unwrap();
        assert_eq!(
            accounts.columns[1].default_definition,
            CatalogField::Observed { value: "3".into() }
        );
        assert!(matches!(
            accounts.indexes[0].definition,
            CatalogField::Observed { .. }
        ));
        assert!(matches!(
            accounts.constraints[0].definition,
            CatalogField::Observed { .. }
        ));
        assert!(
            matches!(estimate(&complete, "accounts").total_bytes, CatalogField::Observed { value } if value >= 16384)
        );
        assert_eq!(
            estimate(&complete, "events").total_bytes,
            CatalogField::Unavailable
        );
        assert!(has_gap(
            snapshot.gaps(),
            Field::DefaultDefinition,
            Reason::Truncated,
            Some("long_definition")
        ));
        assert!(has_gap(
            snapshot.gaps(),
            Field::ConstraintDefinition,
            Reason::OutsideScope,
            Some("links")
        ));

        let simple_scope = InspectionScope::new(vec!["catalog_simple".into()]).unwrap();
        let simple = target
            .inspect_catalog(id.clone(), &simple_scope, details, timeouts)
            .await
            .unwrap();
        let snapshot = simple.snapshot().unwrap();
        assert_eq!(snapshot.tables().len(), 1);
        assert!(!has_gap(
            snapshot.gaps(),
            Field::UnsupportedObjects,
            Reason::Unsupported,
            None
        ));
        assert!(!has_gap(
            snapshot.gaps(),
            Field::Dependencies,
            Reason::Unsupported,
            None
        ));
        assert!(snapshot.dependencies().is_empty());

        let limited_scope = InspectionScope::new(vec![SCHEMA.into()]).unwrap();
        let limited = target
            .inspect_catalog(
                id.clone(),
                &limited_scope,
                options.with_row_limit(1).unwrap(),
                timeouts,
            )
            .await
            .unwrap();
        let snapshot = limited.snapshot().unwrap();
        assert_eq!(snapshot.tables().len(), 1);
        assert_eq!(snapshot.tables()[0].columns.len(), 1);
        for field in [
            Field::Relations,
            Field::Columns,
            Field::Indexes,
            Field::Constraints,
        ] {
            assert!(
                has_gap(snapshot.gaps(), field, Reason::Truncated, None),
                "{field:?}"
            );
        }

        let wrong = CatalogOptions::new(if major == 18 { 17 } else { 18 }).unwrap();
        let mismatch = target
            .inspect_catalog(id.clone(), &scope, wrong, timeouts)
            .await
            .unwrap();
        assert_eq!(
            mismatch.observation().server_version_num.unwrap() / 10_000,
            major
        );
        assert!(mismatch.snapshot().is_none());
        assert!(
            mismatch
                .scope()
                .iter()
                .all(|schema| schema.state == SchemaState::Unavailable)
        );
        assert!(has_gap(
            mismatch.gaps(),
            Field::ServerVersion,
            Reason::Unsupported,
            None
        ));

        let wrong_target = InspectionTarget::new(
            "localhost",
            env::var("DMG_TEST_POSTGRES_PORT").unwrap().parse().unwrap(),
            "postgres",
            "dmg_catalog_reader",
            b"incorrect disposable fixture password".to_vec(),
            &fs::read(env::var_os("DMG_TEST_POSTGRES_CA").unwrap()).unwrap(),
        )
        .unwrap();
        let failure = wrong_target
            .inspect_catalog(id, &scope, options, timeouts)
            .await
            .unwrap();
        assert_eq!(failure.observation().server_version_num, None);
        assert!(failure.snapshot().is_none());
        assert!(has_gap(
            failure.gaps(),
            Field::Connection,
            Reason::PermissionDenied,
            None
        ));
        return;
    }

    let timeouts = match scenario.as_str() {
        "lock" => InspectionTimeouts::new(5000, 1000, 40, 3000).unwrap(),
        "cancel" => InspectionTimeouts::new(5000, 40, 1000, 3000).unwrap(),
        "deadline" => InspectionTimeouts::new(700, 3000, 3000, 3000).unwrap(),
        "denied" | "projection-denied" => timeouts,
        _ => panic!("unknown catalog fixture scenario"),
    };
    let evidence = target
        .inspect_catalog(id, &scope, details, timeouts)
        .await
        .unwrap();
    let snapshot = evidence.snapshot().unwrap();
    if scenario == "projection-denied" {
        assert!(has_gap(
            snapshot.gaps(),
            Field::Columns,
            Reason::PermissionDenied,
            None
        ));
        let accounts = snapshot.table(&name("accounts")).unwrap();
        assert!(accounts.columns.is_empty());
        assert!(accounts.indexes.is_empty());
        assert!(has_gap(
            snapshot.gaps(),
            Field::Indexes,
            Reason::InvalidValue,
            Some("accounts")
        ));
        assert!(
            accounts
                .constraints
                .iter()
                .any(|constraint| constraint.name == "quota_positive")
        );
        return;
    }

    assert_common(&evidence, major, &password);
    let reason = match scenario.as_str() {
        "lock" => Reason::Contention,
        "cancel" => Reason::Cancelled,
        "deadline" => Reason::Deadline,
        "denied" => Reason::PermissionDenied,
        _ => unreachable!(),
    };
    assert!(has_gap(
        snapshot.gaps(),
        Field::DefaultDefinition,
        reason,
        Some("accounts")
    ));
    assert!(has_gap(
        snapshot.gaps(),
        Field::IndexDefinition,
        reason,
        Some("accounts_pkey")
    ));
    assert!(has_gap(
        snapshot.gaps(),
        Field::ConstraintDefinition,
        reason,
        Some("accounts")
    ));
    assert!(has_gap(
        snapshot.gaps(),
        Field::TotalRelationBytes,
        reason,
        Some("accounts")
    ));
    assert_eq!(
        estimate(&evidence, "accounts").total_bytes,
        CatalogField::Unavailable
    );
    if scenario == "deadline" {
        assert!(has_gap(
            evidence.gaps(),
            Field::Connection,
            Reason::Deadline,
            None
        ));
    }
    if scenario == "lock" || scenario == "cancel" {
        assert!(matches!(
            estimate(&evidence, "details").total_bytes,
            CatalogField::Observed { .. }
        ));
        let details = snapshot.table(&name("details")).unwrap();
        assert!(matches!(
            details.columns[2].default_definition,
            CatalogField::Observed { .. }
        ));
    }
}

fn assert_common(evidence: &InspectionEvidenceV1, major: u32, password: &str) {
    assert_eq!(
        evidence.observation().server_version_num.unwrap() / 10_000,
        major
    );
    for (schema, state) in [
        (SCHEMA, SchemaState::Visible),
        ("catalog_hidden", SchemaState::Denied),
        ("catalog_missing", SchemaState::Missing),
        ("catalog_public", SchemaState::Visible),
        ("catalog_empty", SchemaState::Visible),
        ("catalog_Quoted", SchemaState::Visible),
    ] {
        assert!(
            evidence
                .scope()
                .iter()
                .any(|entry| entry.schema == schema && entry.state == state)
        );
    }
    let snapshot = evidence.snapshot().unwrap();
    assert!(
        snapshot
            .schemas()
            .iter()
            .any(|schema| schema == "catalog_empty")
    );
    assert!(
        !snapshot
            .tables()
            .iter()
            .any(|table| table.name.schema == "catalog_empty")
    );
    assert_eq!(
        snapshot
            .table(&QualifiedName {
                schema: "catalog_Quoted".into(),
                name: "Table.With.Dot".into()
            })
            .unwrap()
            .columns[0]
            .name,
        "column\"quote"
    );
    assert!(has_gap(
        snapshot.gaps(),
        Field::UnsupportedObjects,
        Reason::Unsupported,
        None
    ));

    let accounts = snapshot.table(&name("accounts")).unwrap();
    assert_eq!(accounts.kind, TableKind::Ordinary);
    let quota = accounts
        .columns
        .iter()
        .find(|column| column.name == "quota")
        .unwrap();
    assert_eq!(
        quota.data_type,
        QualifiedName {
            schema: "pg_catalog".into(),
            name: "int4".into()
        }
    );
    let check = accounts
        .constraints
        .iter()
        .find(|constraint| constraint.name == "quota_positive")
        .unwrap();
    assert!(!check.validated);
    assert!(check.enforced);
    assert_eq!(
        accounts
            .constraints
            .iter()
            .any(|constraint| constraint.kind == ConstraintKind::NotNull),
        major == 18
    );

    let details = snapshot.table(&name("details")).unwrap();
    let positions: Vec<_> = details
        .columns
        .iter()
        .map(|column| column.position)
        .collect();
    let expected_positions = if major == 18 {
        &[1, 2, 3, 4, 5, 6, 8, 9][..]
    } else {
        &[1, 2, 3, 4, 5, 6, 8]
    };
    assert_eq!(positions, expected_positions);
    if major == 18 {
        assert_eq!(
            details.columns[7].generation,
            Some(ColumnGeneration::Virtual)
        );
        assert!(
            details
                .constraints
                .iter()
                .any(|constraint| constraint.name == "virtual_positive" && !constraint.enforced)
        );
        assert!(has_gap(
            snapshot.gaps(),
            Field::Constraints,
            Reason::Unsupported,
            Some("ranges")
        ));
    }
    assert_eq!(details.columns[0].identity, Some(ColumnIdentity::ByDefault));
    assert_eq!(details.columns[2].type_modifier, 16);
    assert_eq!(details.columns[3].array_dimensions, 1);
    assert_eq!(details.columns[4].data_type, name("status"));
    assert_eq!(
        details.columns[5].generation,
        Some(ColumnGeneration::Stored)
    );
    let index = details
        .indexes
        .iter()
        .find(|index| index.name == "details_lookup")
        .unwrap();
    assert_eq!(index.access_method, "btree");
    assert!(index.valid && index.ready && index.partial);
    assert_eq!(
        index.keys,
        [
            IndexKey::Expression,
            IndexKey::Column {
                name: "quoted key".into()
            }
        ]
    );
    assert_eq!(index.included_columns, ["note"]);
    assert!(
        details
            .indexes
            .iter()
            .any(|index| index.name == "details_note_idx" && index.nulls_not_distinct)
    );

    let links = snapshot.table(&name("links")).unwrap();
    let inside = links
        .constraints
        .iter()
        .find(|constraint| constraint.name == "inside_fk")
        .unwrap();
    assert_eq!(
        inside.references,
        Some(ForeignKeyTarget::Observed {
            table: name("accounts"),
            columns: vec!["id".into()]
        })
    );
    assert!(inside.deferrable && inside.initially_deferred);
    assert!(
        snapshot
            .dependencies()
            .iter()
            .any(|edge| edge.dependent.member.as_deref() == Some("inside_fk")
                && edge.referenced.name == "accounts")
    );
    let outside = links
        .constraints
        .iter()
        .find(|constraint| constraint.name == "outside_fk")
        .unwrap();
    assert_eq!(outside.references, Some(ForeignKeyTarget::OutsideScope));
    assert_eq!(outside.definition, CatalogField::Unavailable);
    let visible = links
        .constraints
        .iter()
        .find(|constraint| constraint.name == "visible_fk")
        .unwrap();
    assert_eq!(
        visible.references,
        Some(ForeignKeyTarget::Observed {
            table: QualifiedName {
                schema: "catalog_public".into(),
                name: "metadata_only".into()
            },
            columns: vec!["id".into()],
        })
    );
    let denied = links
        .constraints
        .iter()
        .find(|constraint| constraint.name == "denied_fk")
        .unwrap();
    assert_eq!(denied.references, Some(ForeignKeyTarget::OutsideScope));
    let debug = format!("{evidence:?}");
    for secret in [
        password,
        "hidden_table",
        "outside_partition",
        "external_identity",
    ] {
        assert!(!debug.contains(secret));
    }

    assert!(
        snapshot
            .partition_edges()
            .iter()
            .any(|edge| edge.child == name("events_first")
                && edge.parent == name("events")
                && !edge.detach_pending)
    );
    assert!(has_gap(
        snapshot.gaps(),
        Field::Partitions,
        Reason::OutsideScope,
        Some("events")
    ));
    assert!(has_gap(
        snapshot.gaps(),
        Field::PartitionBound,
        Reason::Unsupported,
        Some("events_first")
    ));
    assert_eq!(
        estimate(evidence, "accounts").rows,
        CatalogField::Estimated { value: 256 }
    );
    assert!(
        matches!(estimate(evidence, "accounts").heap_bytes, CatalogField::Estimated { value } if value > 0)
    );
    assert_eq!(
        estimate(evidence, "unanalyzed").rows,
        CatalogField::Unavailable
    );
    assert_eq!(
        estimate(evidence, "unanalyzed").heap_bytes,
        CatalogField::Unavailable
    );
    assert_eq!(
        estimate(evidence, "events").heap_bytes,
        CatalogField::NotApplicable
    );
}

fn name(table: &str) -> QualifiedName {
    QualifiedName {
        schema: SCHEMA.into(),
        name: table.into(),
    }
}

fn estimate<'a>(
    evidence: &'a InspectionEvidenceV1,
    relation: &str,
) -> &'a dmg_core::inspection::RelationEstimates {
    evidence
        .relation_estimates()
        .iter()
        .find(|estimate| estimate.relation == name(relation))
        .unwrap()
}

fn has_gap(
    gaps: &[InspectionGap],
    field: Field,
    reason: Reason,
    object_name: Option<&str>,
) -> bool {
    gaps.iter().any(|gap| {
        gap.field == field && gap.reason == reason && object_name.is_none_or(|name| {
        matches!(&gap.scope, InspectionGapScope::Object { object } if object.name == name)
    })
    })
}
