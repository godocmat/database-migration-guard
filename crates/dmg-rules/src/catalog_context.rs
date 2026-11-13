//! Attach positive pre-deployment catalog facts without changing static findings.

use std::collections::{BTreeMap, BTreeSet};

use dmg_core::{
    coverage::CoverageStatus,
    digest::ContentDigest,
    inspection::{CatalogField, InspectionEvidenceV1, RelationEstimates},
    risk::{DatabaseObject, EvidenceKind, Finding, FindingEvidence, ObjectKind},
    snapshot::{QualifiedName, TableKind, TableSnapshot},
    source::{ByteRange, RepoPath},
};
use dmg_postgres::{AlterChange, Operation, ParsedPlan, RelationName, Statement};

use crate::{Analysis, finding::Selection};

/// The caller verifies the artifact digest, scope authorization, target, and freshness.
pub(crate) fn add_catalog_context(
    analysis: &mut Analysis,
    plan: &ParsedPlan,
    evidence: &InspectionEvidenceV1,
    digest: ContentDigest,
) {
    let Some(snapshot) = evidence.snapshot() else {
        return;
    };
    if plan.coverage.status() != CoverageStatus::Complete {
        return;
    }

    // Analysis orders findings by original source span. Consume each finding at most once.
    let mut by_path = BTreeMap::<RepoPath, Vec<&mut Finding>>::new();
    for finding in &mut analysis.findings {
        by_path
            .entry(finding.span.path.clone())
            .or_default()
            .push(finding);
    }

    let mut incoming = BTreeSet::<&DatabaseObject>::new();
    for edge in snapshot.dependencies() {
        incoming.insert(&edge.referenced);
    }

    let selection = Selection::new(plan);
    let mut state = CatalogTimeline::default();
    for migration in &plan.migrations {
        if !selection.contains(&migration.path) {
            continue;
        }
        let findings = by_path.remove(&migration.path).unwrap_or_default();
        let mut findings = findings.into_iter().peekable();
        for statement in &migration.statements {
            if state.uncertain {
                return;
            }
            let span = statement.span.bytes;
            while let Some(next) = findings.peek() {
                if next.span.bytes.start() >= span.end() {
                    break;
                }
                let finding = findings.next().expect("peeked finding");
                if finding.migration != migration.identity || !encloses(span, finding.span.bytes) {
                    continue;
                }
                let Some(object) = &finding.object else {
                    continue;
                };
                let Some(schema) = object.schema.as_deref() else {
                    continue;
                };
                if !names_table(statement, object) || !state.can_match(schema, &object.name) {
                    continue;
                }
                let name = QualifiedName {
                    schema: schema.into(),
                    name: object.name.clone(),
                };
                let Some(table) = snapshot.table(&name) else {
                    continue;
                };
                let estimates = evidence.relation_estimates();
                let size = estimates
                    .binary_search_by(|entry| entry.relation.cmp(&name))
                    .ok()
                    .map(|index| &estimates[index]);
                finding.evidence.extend(observed_context(
                    table,
                    object,
                    &state,
                    incoming.contains(object),
                    size,
                    digest,
                ));
                finding.evidence.sort();
                finding.evidence.dedup();
            }
            state.advance(&statement.operation);
        }
    }
}

fn encloses(outer: ByteRange, inner: ByteRange) -> bool {
    outer.start() <= inner.start() && inner.end() <= outer.end()
}

fn names_table(statement: &Statement, object: &DatabaseObject) -> bool {
    if !matches!(
        object.kind,
        ObjectKind::Relation | ObjectKind::Column | ObjectKind::Constraint
    ) {
        return false;
    }
    let matches = |name: &RelationName| {
        name.catalog.is_none() && name.schema == object.schema && name.name == object.name
    };
    match &statement.operation {
        Operation::AlterTable(table) => matches(&table.relation),
        Operation::Rename(rename)
            if matches!(
                rename.kind,
                ObjectKind::Relation | ObjectKind::Column | ObjectKind::Constraint
            ) =>
        {
            matches(&rename.object)
        }
        Operation::Drop(drop) if drop.kind == ObjectKind::Relation => {
            drop.objects.iter().any(matches)
        }
        Operation::Truncate(truncate) => truncate.relations.iter().any(matches),
        Operation::CreateIndex(index) => matches(&index.relation),
        Operation::Update(change) | Operation::Delete(change) => matches(&change.relation),
        _ => false,
    }
}

#[derive(Default)]
struct CatalogTimeline<'a> {
    changed_relations: BTreeSet<(&'a str, &'a str)>,
    unqualified_changes: BTreeSet<&'a str>,
    changed_schemas: BTreeSet<&'a str>,
    schema_changed: bool,
    data_changed: bool,
    uncertain: bool,
}

impl<'a> CatalogTimeline<'a> {
    fn can_match(&self, schema: &str, relation: &str) -> bool {
        !self.changed_relations.contains(&(schema, relation))
            && !self.unqualified_changes.contains(relation)
            && !self.changed_schemas.contains(schema)
    }

    fn change_relation(&mut self, schema: Option<&'a str>, relation: &'a str) {
        if let Some(schema) = schema {
            self.changed_relations.insert((schema, relation));
        } else {
            self.unqualified_changes.insert(relation);
        }
    }

    fn advance(&mut self, operation: &'a Operation) {
        match operation {
            Operation::Select | Operation::Transaction(_) => return,
            Operation::Insert(_)
            | Operation::Update(_)
            | Operation::Delete(_)
            | Operation::Truncate(_) => {
                // Triggers and partition routing may affect a second table.
                self.data_changed = true;
                return;
            }
            Operation::CreateSchema { name, .. } => {
                self.changed_schemas.insert(name);
            }
            Operation::CreateTable(table) => {
                self.change_relation(table.relation.schema.as_deref(), &table.relation.name);
                if let Some(parent) = &table.partition_of {
                    self.change_relation(parent.schema.as_deref(), &parent.name);
                }
            }
            Operation::AlterTable(alter) => {
                self.change_relation(alter.relation.schema.as_deref(), &alter.relation.name);
                if alter.actions.iter().any(|action| {
                    matches!(
                        action.change,
                        AlterChange::DropColumn { cascade: true, .. }
                            | AlterChange::DropConstraint { cascade: true, .. }
                            | AlterChange::Unsupported
                    )
                }) {
                    self.uncertain = true;
                }
            }
            Operation::Rename(rename)
                if matches!(
                    rename.kind,
                    ObjectKind::Relation | ObjectKind::Column | ObjectKind::Constraint
                ) =>
            {
                let schema = rename.object.schema.as_deref();
                self.change_relation(schema, &rename.object.name);
                if rename.kind == ObjectKind::Relation {
                    self.change_relation(schema, &rename.new_name);
                }
            }
            Operation::Drop(drop) if drop.cascade || drop.kind == ObjectKind::Relation => {
                // A drop can remove partitions or dependent objects under other names.
                self.uncertain = true;
            }
            Operation::Drop(drop) if drop.kind == ObjectKind::Schema => {
                self.changed_schemas
                    .extend(drop.objects.iter().map(|schema| schema.name.as_str()));
            }
            Operation::CreateIndex(index) => {
                self.change_relation(index.relation.schema.as_deref(), &index.relation.name);
            }
            Operation::Drop(drop) if drop.kind == ObjectKind::Index => self.data_changed = true,
            Operation::Unsupported => self.uncertain = true,
            _ => (),
        }
        self.schema_changed = true;
    }
}

fn observed_context(
    table: &TableSnapshot,
    object: &DatabaseObject,
    state: &CatalogTimeline<'_>,
    has_dependency: bool,
    estimates: Option<&RelationEstimates>,
    digest: ContentDigest,
) -> impl Iterator<Item = FindingEvidence> {
    let observed = match table.kind {
        TableKind::Ordinary if table.is_partition => "a table partition",
        TableKind::Ordinary => "an ordinary table",
        TableKind::Partitioned => "a partitioned table",
    };
    let mut context =
        format!("The pre-deployment catalog observed {observed} at this qualified name.");
    if !state.schema_changed
        && let Some(member) = object.member.as_deref()
    {
        let observed_member = match object.kind {
            ObjectKind::Column => table.columns.iter().any(|column| column.name == member),
            ObjectKind::Constraint => table
                .constraints
                .iter()
                .any(|constraint| constraint.name == member),
            _ => false,
        };
        if observed_member {
            context.push_str(" The catalog also observed this member on the table.");
        }
    }
    if !state.schema_changed && has_dependency {
        context
            .push_str(" The scoped catalog recorded an incoming dependency for this exact object.");
    }

    let mut estimates_context = Vec::new();
    if let Some(estimates) =
        estimates.filter(|_| !state.data_changed && table.kind == TableKind::Ordinary)
    {
        if let CatalogField::Observed { value } = estimates.total_bytes {
            context.push_str(&format!(
                " Total relation size was {value} bytes, including indexes and TOAST."
            ));
        }
        if let CatalogField::Estimated { value } = estimates.rows {
            estimates_context.push(format!("Planner estimate of {value} rows."));
        }
        if let CatalogField::Estimated { value } = estimates.heap_bytes {
            estimates_context.push(format!("Heap size estimate of {value} bytes."));
        }
    }
    let observed = FindingEvidence {
        kind: EvidenceKind::InputFact,
        detail: context,
        artifact_digest: Some(digest),
    };
    let estimate = (!estimates_context.is_empty()).then(|| FindingEvidence {
        kind: EvidenceKind::CatalogEstimate,
        detail: format!("Pre-deployment values. {}", estimates_context.join(" ")),
        artifact_digest: Some(digest),
    });
    [Some(observed), estimate].into_iter().flatten()
}
