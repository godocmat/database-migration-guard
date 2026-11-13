use std::collections::VecDeque;

use dmg_core::inspection::{CatalogField, InspectionField, InspectionGapReason};
use tokio_postgres::{Transaction, types::Type};

use crate::TransportError;

use super::{
    CatalogValue, Collection, Oid, isolated_query,
    structure::{column_object, constraint_object, index_object, object_gap, relation_gap},
    target_gap, value,
};

const DEFAULT_DEFINITION: &str = include_str!("sql/default_definition.sql");
const INDEX_DEFINITION: &str = include_str!("sql/index_definition.sql");
const CONSTRAINT_DEFINITION: &str = include_str!("sql/constraint_definition.sql");
const TOTAL_BYTES: &str = include_str!("sql/total_bytes.sql");

const DEFINITION_BYTES: i32 = 8 * 1024;
const MAX_DEFINITION_READS: usize = 48;
const MAX_SIZE_READS: usize = 32;

#[derive(Debug, Default)]
pub(super) struct PendingDetails {
    definitions: VecDeque<DefinitionRead>,
    sizes: VecDeque<Oid>,
    truncated: Vec<InspectionField>,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum DefinitionKind {
    Default(usize),
    Index(usize),
    Constraint(usize),
}

impl DefinitionKind {
    fn query(self) -> &'static str {
        match self {
            Self::Default(_) => DEFAULT_DEFINITION,
            Self::Index(_) => INDEX_DEFINITION,
            Self::Constraint(_) => CONSTRAINT_DEFINITION,
        }
    }

    fn field(self) -> InspectionField {
        match self {
            Self::Default(_) => InspectionField::DefaultDefinition,
            Self::Index(_) => InspectionField::IndexDefinition,
            Self::Constraint(_) => InspectionField::ConstraintDefinition,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct DefinitionRead {
    pub(super) relation: Oid,
    pub(super) oid: Oid,
    pub(super) kind: DefinitionKind,
}

impl Collection {
    pub(super) fn queue_definition(&mut self, read: DefinitionRead) {
        if self.options.details.definitions {
            if self.pending_details.definitions.len() < MAX_DEFINITION_READS {
                self.pending_details.definitions.push_back(read);
            } else {
                self.record_detail_truncation(read.kind.field());
            }
        }
    }

    pub(super) fn queue_size(&mut self, relation: Oid) {
        if self.options.details.total_bytes {
            if self.pending_details.sizes.len() < MAX_SIZE_READS {
                self.pending_details.sizes.push_back(relation);
            } else {
                self.record_detail_truncation(InspectionField::TotalRelationBytes);
            }
        }
    }

    fn record_detail_truncation(&mut self, field: InspectionField) {
        if !self.pending_details.truncated.contains(&field) {
            self.pending_details.truncated.push(field);
            self.structural_gaps
                .push(target_gap(field, InspectionGapReason::Truncated));
        }
    }

    pub(super) async fn collect_details(
        &mut self,
        tx: &mut Transaction<'_>,
    ) -> Result<(), TransportError> {
        if self.options.details.total_bytes {
            for relation in self.relations.values() {
                if relation.table.kind == dmg_core::snapshot::TableKind::Partitioned {
                    self.structural_gaps.push(relation_gap(
                        &relation.table.name,
                        InspectionField::TotalRelationBytes,
                        InspectionGapReason::Unsupported,
                    ));
                }
            }
        }

        while let Some(read) = self.pending_details.definitions.front().copied() {
            let result = read_definition(tx, read).await?;
            self.record_definition(read, result);
            self.pending_details.definitions.pop_front();
        }
        while let Some(oid) = self.pending_details.sizes.front().copied() {
            let result = read_size(tx, oid).await?;
            self.record_size(oid, result);
            self.pending_details.sizes.pop_front();
        }
        Ok(())
    }

    pub(super) fn note_interrupted_details(&mut self, reason: InspectionGapReason) {
        while let Some(read) = self.pending_details.definitions.pop_front() {
            self.record_definition(read, Err(reason));
        }
        while let Some(oid) = self.pending_details.sizes.pop_front() {
            self.record_size(oid, Err(reason));
        }
    }

    fn record_definition(&mut self, read: DefinitionRead, result: CatalogValue<String>) {
        let Some(relation) = self.relations.get_mut(&read.relation) else {
            self.structural_gaps.push(target_gap(
                read.kind.field(),
                InspectionGapReason::InvalidValue,
            ));
            return;
        };
        let table = &mut relation.table;
        let field = read.kind.field();
        match result {
            Ok(value) => {
                let definition = match read.kind {
                    DefinitionKind::Default(position) => {
                        &mut table.columns[position].default_definition
                    }
                    DefinitionKind::Index(position) => &mut table.indexes[position].definition,
                    DefinitionKind::Constraint(position) => {
                        &mut table.constraints[position].definition
                    }
                };
                *definition = CatalogField::Observed { value };
            }
            Err(reason) => {
                let object = match read.kind {
                    DefinitionKind::Default(position) => {
                        column_object(&table.name, &table.columns[position].name)
                    }
                    DefinitionKind::Index(position) => {
                        index_object(&table.name, &table.indexes[position].name)
                    }
                    DefinitionKind::Constraint(position) => {
                        constraint_object(&table.name, &table.constraints[position].name)
                    }
                };
                self.structural_gaps.push(object_gap(object, field, reason));
            }
        }
    }

    fn record_size(&mut self, oid: Oid, result: CatalogValue<u64>) {
        let field = InspectionField::TotalRelationBytes;
        let Some(relation) = self.relations.get_mut(&oid) else {
            self.structural_gaps
                .push(target_gap(field, InspectionGapReason::InvalidValue));
            return;
        };
        match result {
            Ok(value) => relation.estimates.total_bytes = CatalogField::Observed { value },
            Err(reason) => {
                self.structural_gaps
                    .push(relation_gap(&relation.table.name, field, reason))
            }
        }
    }
}

async fn read_definition(
    tx: &mut Transaction<'_>,
    read: DefinitionRead,
) -> Result<CatalogValue<String>, TransportError> {
    let rows = isolated_query(
        tx,
        read.kind.query(),
        &[(&read.oid, Type::OID), (&DEFINITION_BYTES, Type::INT4)],
    )
    .await?;
    Ok(rows.and_then(|rows| {
        let [row] = rows.as_slice() else {
            return Err(InspectionGapReason::Missing);
        };
        if value::<Option<bool>>(row, 1)? == Some(true) {
            return Err(InspectionGapReason::Truncated);
        }
        let definition: Option<String> = value(row, 0)?;
        let definition = definition.ok_or(InspectionGapReason::Missing)?;
        if definition.len() > DEFINITION_BYTES as usize {
            return Err(InspectionGapReason::Truncated);
        }
        if definition.is_empty() || definition.contains('\0') {
            return Err(InspectionGapReason::InvalidValue);
        }
        Ok(definition)
    }))
}

async fn read_size(
    tx: &mut Transaction<'_>,
    oid: Oid,
) -> Result<CatalogValue<u64>, TransportError> {
    let rows = isolated_query(tx, TOTAL_BYTES, &[(&oid, Type::OID)]).await?;
    Ok(rows.and_then(|rows| {
        let [row] = rows.as_slice() else {
            return Err(InspectionGapReason::Missing);
        };
        let bytes: Option<i64> = value(row, 0)?;
        u64::try_from(bytes.ok_or(InspectionGapReason::Missing)?)
            .map_err(|_| InspectionGapReason::InvalidValue)
    }))
}
