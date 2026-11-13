//! Schema facts established by supported forward statements.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use dmg_postgres::{
    AlterChange, AlterTable, ColumnDefinition, Constraint, ConstraintKind, CreateTable, DataType,
    RelationKind, RelationName,
};

use crate::transaction::TransactionEffect;

#[derive(Debug, Clone, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct RelationKey {
    pub(crate) schema: String,
    pub(crate) name: String,
}

pub(crate) fn resolve(name: &RelationName, search_path: &[String]) -> Option<RelationKey> {
    if name.catalog.is_some() {
        return None;
    }
    let schema = match (&name.schema, search_path) {
        (Some(schema), _) | (None, [schema]) => schema,
        (None, _) => return None,
    };
    if schema.is_empty() || matches!(schema.as_str(), "pg_temp" | "$user") {
        return None;
    }
    Some(RelationKey {
        schema: schema.clone(),
        name: name.name.clone(),
    })
}

type RelationState = Option<Arc<TableFacts>>;

#[derive(Default)]
pub(super) struct Schema {
    // A key without a value is a relation known absent after a supported DROP.
    relations: BTreeMap<RelationKey, RelationState>,
    block: Option<Block>,
    block_id: usize,
    write_epoch: usize,
}

#[derive(Default)]
struct Block {
    undo: Vec<Undo>,
}

#[derive(Default)]
struct Undo {
    marker: Option<usize>,
    before: BTreeMap<RelationKey, Option<RelationState>>,
}

impl Schema {
    pub(super) fn start_block(&mut self) {
        if self.block.is_some() {
            self.invalidate();
        }
        self.block_id += 1;
        self.block = Some(Block {
            undo: vec![Undo::default()],
        });
    }

    pub(super) fn finish_step(&mut self, effect: TransactionEffect) {
        match effect {
            TransactionEffect::None => (),
            TransactionEffect::Commit => {
                self.block = None;
            }
            TransactionEffect::Rollback => {
                if let Some(block) = self.block.take() {
                    self.restore(block.undo);
                }
            }
            TransactionEffect::Savepoint(marker) => {
                if let Some(block) = &mut self.block {
                    block.undo.push(Undo {
                        marker: Some(marker),
                        before: BTreeMap::new(),
                    });
                }
            }
            TransactionEffect::RollbackTo(marker) => self.rollback_to(marker),
            TransactionEffect::Release(marker) => self.release(marker),
            TransactionEffect::Invalidate => self.invalidate(),
        }
    }

    fn rollback_to(&mut self, marker: usize) {
        let Some((block, position)) = self.savepoint(marker) else {
            self.invalidate();
            return;
        };
        let changes = block.undo.split_off(position);
        block.undo.push(Undo {
            marker: Some(marker),
            before: BTreeMap::new(),
        });
        self.restore(changes);
    }

    fn release(&mut self, marker: usize) {
        let Some((block, position)) = self.savepoint(marker) else {
            self.invalidate();
            return;
        };
        let layers = block.undo.split_off(position);
        let previous = block.undo.last_mut().expect("savepoint has a transaction");
        for layer in layers {
            for (key, state) in layer.before {
                previous.before.entry(key).or_insert(state);
            }
        }
    }

    fn savepoint(&mut self, marker: usize) -> Option<(&mut Block, usize)> {
        let block = self.block.as_mut()?;
        let position = block
            .undo
            .iter()
            .position(|layer| layer.marker == Some(marker))?;
        Some((block, position))
    }

    fn restore(&mut self, layers: Vec<Undo>) {
        for layer in layers.into_iter().rev() {
            for (key, before) in layer.before {
                match before {
                    Some(before) => {
                        self.relations.insert(key, before);
                    }
                    None => {
                        self.relations.remove(&key);
                    }
                }
            }
        }
    }

    pub(super) fn invalidate(&mut self) {
        self.relations.clear();
        self.block = None;
    }

    pub(super) fn table(&self, key: Option<&RelationKey>) -> Option<&TableFacts> {
        self.relations.get(key?)?.as_deref()
    }

    pub(super) fn known_absent(&self, key: Option<&RelationKey>) -> bool {
        key.is_some_and(|key| self.relations.get(key).is_some_and(Option::is_none))
    }

    pub(super) fn is_private(&self, table: Option<&TableFacts>) -> bool {
        self.block.is_some()
            && table.is_some_and(|table| {
                table
                    .created
                    .is_some_and(|(block, _)| block == self.block_id)
            })
    }

    pub(super) fn was_column_absent_at_block_start(
        &self,
        key: Option<&RelationKey>,
        name: &str,
    ) -> bool {
        let Some((block, key)) = self.block.as_ref().zip(key) else {
            return false;
        };
        // The first retained change stores the relation as it was at block entry.
        block
            .undo
            .iter()
            .find_map(|layer| layer.before.get(key))
            .and_then(|relation| relation.as_ref()?.as_deref())
            .is_some_and(|table| table.column_absent(name))
    }

    pub(super) fn is_new_empty_table(&self, table: Option<&TableFacts>) -> bool {
        self.is_private(table)
            && table.is_some_and(|table| {
                table.kind == Some(RelationKind::Table)
                    && table.created == Some((self.block_id, self.write_epoch))
            })
    }

    pub(super) fn mark_data_write(&mut self) {
        self.write_epoch += 1;
    }

    pub(super) fn create_table(&mut self, key: RelationKey, table: &CreateTable) {
        if table.if_not_exists && !self.known_absent(Some(&key)) {
            // An unobserved table may already exist, with a different schema.
            return;
        }
        if self.table(Some(&key)).is_some() {
            // A second unconditional create has no established outcome.
            self.forget(&key);
            return;
        }
        let mut facts = TableFacts {
            kind: Some(table.kind),
            created: self
                .block
                .as_ref()
                .map(|_| (self.block_id, self.write_epoch)),
            columns_complete: table.kind == RelationKind::Table,
            ..TableFacts::default()
        };
        for column in &table.columns {
            facts.insert_column(column);
        }
        facts.constraints.extend(table.constraints.iter().cloned());
        self.set(key, Some(Arc::new(facts)));
    }

    pub(super) fn alter_table(&mut self, key: RelationKey, alter: &AlterTable) {
        if self.table(Some(&key)).is_none() {
            if alter.if_exists || self.known_absent(Some(&key)) {
                return;
            }
            self.set(key.clone(), Some(Arc::new(TableFacts::default())));
        }
        let table = self
            .table_mut(&key)
            .expect("known or newly established table");
        if table.kind != Some(RelationKind::Table) && alter.only {
            table.forget_proofs();
            return;
        }
        for action in &alter.actions {
            table.apply(&action.change);
        }
    }

    pub(super) fn drop_table(&mut self, key: RelationKey) {
        if self
            .table(Some(&key))
            .is_some_and(|table| table.kind == Some(RelationKind::PartitionedTable))
        {
            // A parent can remove its partitions, whose effects are not in the statement.
            self.invalidate();
        }
        self.set(key, None);
    }

    pub(super) fn rename_table(&mut self, old: &RelationKey, new_name: &str) {
        let new = RelationKey {
            schema: old.schema.clone(),
            name: new_name.into(),
        };
        match self.relations.get(old).cloned() {
            Some(Some(table)) if self.table(Some(&new)).is_none() => {
                self.set(old.clone(), None);
                self.set(new, Some(table));
            }
            Some(None) => (),
            _ => {
                self.forget(old);
                self.forget(&new);
            }
        }
    }

    pub(super) fn rename_column(&mut self, key: &RelationKey, old: &str, new_name: &str) {
        let Some(table) = self.table_mut(key) else {
            return;
        };
        if let Some(column) = table.columns.remove(old) {
            table.columns.insert(new_name.into(), column);
        }
        for constraint in &mut table.constraints {
            replace_name(&mut constraint.columns, old, new_name);
            if let ConstraintKind::Check { proves_not_null } = &mut constraint.kind {
                replace_name(proves_not_null, old, new_name);
            }
        }
    }

    pub(super) fn rename_constraint(&mut self, key: &RelationKey, old: &str, new_name: &str) {
        let Some(table) = self.table_mut(key) else {
            return;
        };
        if let Some(constraint) = table
            .constraints
            .iter_mut()
            .find(|constraint| constraint.name.as_deref() == Some(old))
        {
            constraint.name = Some(new_name.into());
        } else {
            // The old name may be the generated name of an anonymous constraint.
            table
                .constraints
                .retain(|constraint| constraint.name.is_some());
        }
    }

    fn remember(&mut self, key: &RelationKey) {
        if let Some(layer) = self.block.as_mut().and_then(|block| block.undo.last_mut()) {
            layer
                .before
                .entry(key.clone())
                .or_insert_with(|| self.relations.get(key).cloned());
        }
    }

    fn table_mut(&mut self, key: &RelationKey) -> Option<&mut TableFacts> {
        self.remember(key);
        self.relations.get_mut(key)?.as_mut().map(Arc::make_mut)
    }

    fn set(&mut self, key: RelationKey, state: RelationState) {
        self.remember(&key);
        self.relations.insert(key, state);
    }

    fn forget(&mut self, key: &RelationKey) {
        self.remember(key);
        self.relations.remove(key);
    }
}

fn replace_name(names: &mut [String], old: &str, new_name: &str) {
    for name in names {
        if name == old {
            *name = new_name.into();
        }
    }
}

#[derive(Clone, Default)]
pub(super) struct TableFacts {
    pub(super) kind: Option<RelationKind>,
    created: Option<(usize, usize)>,
    columns_complete: bool,
    columns: BTreeMap<String, ColumnFacts>,
    constraints: Vec<Constraint>,
}

#[derive(Clone)]
struct ColumnFacts {
    data_type: DataType,
    not_null: bool,
}

impl TableFacts {
    pub(super) fn has_column(&self, name: &str) -> bool {
        self.columns.contains_key(name)
    }

    pub(super) fn column_absent(&self, name: &str) -> bool {
        self.columns_complete && !self.has_column(name)
    }

    pub(super) fn column_type(&self, name: &str) -> Option<&DataType> {
        self.columns.get(name).map(|column| &column.data_type)
    }

    pub(super) fn constraint(&self, name: &str) -> Option<&Constraint> {
        self.constraints
            .iter()
            .find(|constraint| constraint.name.as_deref() == Some(name))
    }

    pub(super) fn proves_not_null(
        &self,
        name: &str,
        changed_columns: &BTreeSet<&str>,
        removed_constraints: &BTreeSet<&str>,
        removes_columns: bool,
    ) -> bool {
        if changed_columns.contains(name) {
            return false;
        }
        if removed_constraints.is_empty()
            && self.columns.get(name).is_some_and(|column| column.not_null)
        {
            return true;
        }
        if removes_columns {
            // A CHECK can depend on more columns than it proves non-null.
            return false;
        }
        self.constraints.iter().any(|constraint| {
            if !constraint.enforced
                || constraint.no_inherit
                || constraint.not_valid
                || constraint
                    .name
                    .as_deref()
                    .is_some_and(|name| removed_constraints.contains(name))
                || constraint.name.is_none() && !removed_constraints.is_empty()
            {
                return false;
            }
            match &constraint.kind {
                ConstraintKind::Check { proves_not_null } => {
                    proves_not_null.iter().any(|column| column == name)
                }
                ConstraintKind::NotNull | ConstraintKind::PrimaryKey => {
                    constraint.columns.iter().any(|column| column == name)
                }
                _ => false,
            }
        })
    }

    fn insert_column(&mut self, column: &ColumnDefinition) {
        self.columns.insert(
            column.name.clone(),
            ColumnFacts {
                data_type: column.data_type.clone(),
                not_null: column.not_null,
            },
        );
        self.constraints.extend(column.constraints.iter().cloned());
    }

    fn forget_proofs(&mut self) {
        self.constraints.clear();
        for column in self.columns.values_mut() {
            column.not_null = false;
        }
    }

    fn apply(&mut self, action: &AlterChange) {
        match action {
            AlterChange::AddColumn {
                column,
                if_not_exists,
            } if !*if_not_exists || self.column_absent(&column.name) => {
                self.insert_column(column);
            }
            AlterChange::DropColumn { name, .. } => {
                self.columns.remove(name);
                // A CHECK may mention other columns without proving them non-null.
                self.constraints.retain(|constraint| {
                    !matches!(constraint.kind, ConstraintKind::Check { .. })
                        && !constraint.columns.contains(name)
                });
            }
            AlterChange::SetNotNull { column } => {
                if let Some(column) = self.columns.get_mut(column) {
                    column.not_null = true;
                }
            }
            AlterChange::DropNotNull { column } => {
                if let Some(column) = self.columns.get_mut(column) {
                    column.not_null = false;
                }
                self.constraints.retain(|constraint| {
                    !matches!(constraint.kind, ConstraintKind::NotNull)
                        || !constraint.columns.contains(column)
                });
            }
            AlterChange::ChangeType {
                column, data_type, ..
            } => {
                if let Some(column) = self.columns.get_mut(column) {
                    column.data_type = data_type.clone();
                }
                self.constraints
                    .retain(|constraint| !matches!(constraint.kind, ConstraintKind::Check { .. }));
            }
            AlterChange::AddConstraint(constraint) => {
                self.constraints.push(constraint.clone());
            }
            AlterChange::DropConstraint { name, .. } => {
                if let Some(constraint) = self.constraint(name) {
                    if matches!(
                        constraint.kind,
                        ConstraintKind::NotNull | ConstraintKind::PrimaryKey
                    ) {
                        let columns = constraint.columns.clone();
                        for name in columns {
                            if let Some(column) = self.columns.get_mut(&name) {
                                column.not_null = false;
                            }
                        }
                    }
                } else {
                    self.forget_proofs();
                }
                self.constraints
                    .retain(|constraint| constraint.name.as_deref() != Some(name));
            }
            AlterChange::ValidateConstraint { name } => {
                if let Some(constraint) = self.constraints.iter_mut().find(|constraint| {
                    constraint.enforced && constraint.name.as_deref() == Some(name)
                }) {
                    constraint.not_valid = false;
                }
            }
            AlterChange::Unsupported => self.forget_proofs(),
            _ => (),
        }
    }
}
