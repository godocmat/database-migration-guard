use dmg_core::{
    coverage::{CoverageGap, CoverageGapKind},
    risk::ObjectKind,
    source::{SourceMap, SourceSpan},
};
use pg_query::protobuf::{
    AlterTableCmd, AlterTableStmt, AlterTableType, ColumnDef, ConstrType, CreateStmt, DropBehavior,
    DropStmt, Node, ObjectType, RangeVar, RenameStmt, TransactionStmt, TransactionStmtKind,
    TruncateStmt, TypeName, a_const::Val, node::Node as Ast,
};

use crate::{
    AlterAction, AlterChange, AlterTable, ColumnDefinition, Constraint, ConstraintKind,
    CreateIndex, CreateTable, DataChange, DataType, DefaultExpr, DropObjects, IndexKey, Operation,
    RelationKind, RelationName, Rename, Transaction, Truncate,
    expression::{check_proof, default_expr, predicate, returning_is_supported, simple_value},
    parse::{SqlToken, delimiter_depth, span, span_gap, token_range},
};

pub(crate) fn node(node: &Node) -> Option<&Ast> {
    node.node.as_ref()
}

pub(crate) fn names(nodes: &[Node]) -> Option<Vec<String>> {
    nodes
        .iter()
        .map(|name| match node(name)? {
            Ast::String(value) => Some(value.sval.clone()),
            _ => None,
        })
        .collect()
}

fn nonempty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}

fn relation(var: &RangeVar) -> Option<RelationName> {
    Some(RelationName {
        catalog: nonempty(&var.catalogname),
        schema: nonempty(&var.schemaname),
        name: nonempty(&var.relname)?,
    })
}

fn named_object(value: &Node) -> Option<RelationName> {
    let (catalog, schema, name) = match node(value)? {
        Ast::String(name) => (None, None, nonempty(&name.sval)?),
        Ast::List(list) => {
            let mut names = names(&list.items)?.into_iter().rev();
            let name = names.next()?;
            let schema = names.next();
            let catalog = names.next();
            if names.next().is_some() {
                return None;
            }
            (catalog, schema, name)
        }
        _ => return None,
    };
    Some(RelationName {
        catalog,
        schema,
        name,
    })
}

fn data_type(value: &TypeName) -> Option<DataType> {
    let name = names(&value.names)?;
    if name.is_empty() || value.pct_type || value.setof {
        return None;
    }
    let integer_modifiers = value
        .typmods
        .iter()
        .map(|value| {
            let Ast::AConst(value) = node(value)? else {
                return None;
            };
            match value.val.as_ref()? {
                Val::Ival(value) => Some(i64::from(value.ival)),
                _ => None,
            }
        })
        .collect();
    Some(DataType {
        name,
        integer_modifiers,
        array_dimensions: value.array_bounds.len(),
    })
}

pub(crate) struct Normalizer<'a, 'source> {
    pub map: &'a SourceMap<'source>,
    pub base: usize,
    pub gaps: &'a mut Vec<CoverageGap>,
}

impl Normalizer<'_, '_> {
    pub fn statement(
        &mut self,
        ast: Option<&Ast>,
        tokens: &[SqlToken<'_>],
        origin: usize,
        location: &SourceSpan,
    ) -> Operation {
        self.normalize(ast, tokens, origin, location)
            .unwrap_or_else(|| {
                self.unsupported(
                    location,
                    "statement is outside the supported PostgreSQL analysis subset",
                );
                Operation::Unsupported
            })
    }

    fn unsupported(&mut self, location: &SourceSpan, message: &str) {
        self.gaps.push(span_gap(
            location.clone(),
            CoverageGapKind::UnsupportedSql,
            message,
        ));
    }

    fn normalize(
        &mut self,
        ast: Option<&Ast>,
        tokens: &[SqlToken<'_>],
        origin: usize,
        location: &SourceSpan,
    ) -> Option<Operation> {
        match ast? {
            Ast::CreateSchemaStmt(schema) => {
                if !schema.schema_elts.is_empty() {
                    self.unsupported(location, "embedded statements in CREATE SCHEMA are outside the supported subset");
                }
                Some(Operation::CreateSchema {
                    name: nonempty(&schema.schemaname)?,
                    if_not_exists: schema.if_not_exists,
                })
            }
            Ast::CreateStmt(table) => self.create_table(table, location).map(Operation::CreateTable),
            Ast::AlterTableStmt(table) if table.objtype() == ObjectType::ObjectTable => {
                self.alter_table(table, tokens, origin, location).map(Operation::AlterTable)
            }
            Ast::RenameStmt(rename) => normalize_rename(rename).map(Operation::Rename),
            Ast::DropStmt(drop) => normalize_drop(drop).map(Operation::Drop),
            Ast::TruncateStmt(truncate) => normalize_truncate(truncate).map(Operation::Truncate),
            Ast::IndexStmt(index) => {
                let keys = index.index_params.iter().map(|key| match node(key) {
                    Some(Ast::IndexElem(key)) if key.expr.is_none() && !key.name.is_empty() => {
                        IndexKey::Column(key.name.clone())
                    }
                    _ => IndexKey::Expression,
                }).collect();
                if index.iswithoutoverlaps {
                    self.unsupported(location, "temporal index constraints are outside the supported subset");
                }
                Some(Operation::CreateIndex(CreateIndex {
                    relation: relation(index.relation.as_ref()?)?,
                    name: nonempty(&index.idxname),
                    keys,
                    unique: index.unique,
                    concurrently: index.concurrent,
                    if_not_exists: index.if_not_exists,
                    predicate: predicate(index.where_clause.as_deref()),
                }))
            }
            Ast::UpdateStmt(update) => {
                let complex = update.with_clause.is_some()
                    || !update.from_clause.is_empty()
                    || !returning_is_supported(update.returning_clause.as_ref())
                    || update.where_clause.as_deref().is_some_and(|value| !simple_value(value))
                    || update.target_list.iter().any(|target| {
                        !matches!(node(target), Some(Ast::ResTarget(target)) if target.indirection.is_empty() && target.val.as_deref().is_some_and(simple_value))
                    });
                if complex {
                    self.unsupported(location, "UPDATE contains expressions or source clauses outside the supported subset");
                }
                Some(Operation::Update(DataChange {
                    relation: relation(update.relation.as_ref()?)?,
                    predicate: predicate(update.where_clause.as_deref()),
                    additional_relations: !update.from_clause.is_empty(),
                }))
            }
            Ast::DeleteStmt(delete) => {
                if delete.with_clause.is_some()
                    || !delete.using_clause.is_empty()
                    || !returning_is_supported(delete.returning_clause.as_ref())
                    || delete.where_clause.as_deref().is_some_and(|value| !simple_value(value))
                {
                    self.unsupported(location, "DELETE contains expressions or source clauses outside the supported subset");
                }
                Some(Operation::Delete(DataChange {
                    relation: relation(delete.relation.as_ref()?)?,
                    predicate: predicate(delete.where_clause.as_deref()),
                    additional_relations: !delete.using_clause.is_empty(),
                }))
            }
            Ast::InsertStmt(insert) => {
                let simple_source = insert.select_stmt.as_deref().is_none_or(|source| {
                    matches!(node(source), Some(Ast::SelectStmt(values)) if !values.values_lists.is_empty() && values.values_lists.iter().all(simple_value))
                });
                if !simple_source
                    || insert.on_conflict_clause.is_some()
                    || insert.with_clause.is_some()
                    || !returning_is_supported(insert.returning_clause.as_ref())
                {
                    self.unsupported(location, "INSERT source, conflict handling, or RETURNING is outside the supported subset");
                }
                Some(Operation::Insert(relation(insert.relation.as_ref()?)?))
            }
            Ast::SelectStmt(select) if select.into_clause.is_none()
                && select.with_clause.is_none()
                && select.from_clause.is_empty()
                && select.where_clause.is_none()
                && select.having_clause.is_none()
                && select.group_clause.is_empty()
                && select.locking_clause.is_empty()
                && select.window_clause.is_empty()
                && select.sort_clause.is_empty()
                && select.limit_count.is_none()
                && select.limit_offset.is_none()
                && select.larg.is_none()
                && select.rarg.is_none()
                && select.values_lists.is_empty()
                && select.target_list.iter().all(|target| {
                    matches!(node(target), Some(Ast::ResTarget(target)) if target.val.as_deref().is_some_and(|value| matches!(node(value), Some(Ast::AConst(_)))))
                }) => Some(Operation::Select),
            Ast::TransactionStmt(stmt) => normalize_transaction(stmt).map(Operation::Transaction),
            _ => None,
        }
    }

    fn create_table(&mut self, table: &CreateStmt, location: &SourceSpan) -> Option<CreateTable> {
        let partition_of = if table.partbound.is_some() {
            match table.inh_relations.as_slice() {
                [parent] => match node(parent) {
                    Some(Ast::RangeVar(parent)) => Some(relation(parent)?),
                    _ => return None,
                },
                _ => return None,
            }
        } else {
            None
        };
        let rel = table.relation.as_ref()?;
        if (partition_of.is_none() && !table.inh_relations.is_empty())
            || table.of_typename.is_some()
            || rel.relpersistence == "t"
        {
            self.unsupported(
                location,
                "typed, inherited, or temporary tables need additional schema context",
            );
        }
        let kind = if table.partspec.is_some() {
            RelationKind::PartitionedTable
        } else if partition_of.is_some() {
            RelationKind::Partition
        } else {
            RelationKind::Table
        };
        let mut columns = Vec::new();
        let mut constraints = Vec::new();
        for child in table
            .table_elts
            .iter()
            .chain(&table.constraints)
            .chain(&table.nnconstraints)
        {
            match node(child) {
                Some(Ast::ColumnDef(column)) => match self.column(column, location) {
                    Some(column) => columns.push(column),
                    None => self.unsupported(location, "could not normalize a CREATE TABLE column"),
                },
                Some(Ast::Constraint(value)) => {
                    constraints.push(self.constraint(value, None, location))
                }
                _ => self.unsupported(location, "CREATE TABLE contains unsupported table elements"),
            }
        }
        Some(CreateTable {
            relation: relation(rel)?,
            kind,
            partition_of,
            if_not_exists: table.if_not_exists,
            columns,
            constraints,
        })
    }

    fn column(&mut self, column: &ColumnDef, location: &SourceSpan) -> Option<ColumnDefinition> {
        let data_type = data_type(column.type_name.as_ref()?)?;
        let mut default = default_expr(column.raw_default.as_deref());
        let mut not_null = column.is_not_null;
        let mut constraints = Vec::new();
        for item in &column.constraints {
            match node(item) {
                Some(Ast::Constraint(item)) => match item.contype() {
                    ConstrType::ConstrDefault => default = default_expr(item.raw_expr.as_deref()),
                    ConstrType::ConstrNull => (),
                    ConstrType::ConstrNotnull => {
                        not_null = true;
                        constraints.push(self.constraint(item, Some(&column.colname), location));
                    }
                    ConstrType::ConstrPrimary => {
                        not_null = true;
                        constraints.push(self.constraint(item, Some(&column.colname), location));
                    }
                    ConstrType::ConstrIdentity => {
                        default = DefaultExpr::Volatile;
                        not_null = true;
                    }
                    _ => constraints.push(self.constraint(item, Some(&column.colname), location)),
                },
                _ => self.unsupported(location, "column contains unsupported attributes"),
            }
        }
        if matches!(data_type.name.as_slice(), [name] if matches!(name.as_str(), "serial" | "bigserial" | "smallserial" | "serial2" | "serial4" | "serial8"))
        {
            default = DefaultExpr::Volatile;
            not_null = true;
        }
        Some(ColumnDefinition {
            name: nonempty(&column.colname)?,
            data_type,
            default,
            not_null,
            constraints,
        })
    }

    fn constraint(
        &mut self,
        value: &pg_query::protobuf::Constraint,
        column: Option<&str>,
        location: &SourceSpan,
    ) -> Constraint {
        let kind = match value.contype() {
            ConstrType::ConstrCheck => ConstraintKind::Check {
                proves_not_null: value.raw_expr.as_deref().map_or_else(Vec::new, check_proof),
            },
            ConstrType::ConstrForeign => value
                .pktable
                .as_ref()
                .and_then(relation)
                .map(|referenced_table| ConstraintKind::ForeignKey {
                    referenced_table,
                    referenced_columns: names(&value.pk_attrs).unwrap_or_default(),
                })
                .unwrap_or(ConstraintKind::Unsupported),
            ConstrType::ConstrNotnull => ConstraintKind::NotNull,
            ConstrType::ConstrPrimary => ConstraintKind::PrimaryKey,
            ConstrType::ConstrUnique => ConstraintKind::Unique,
            _ => ConstraintKind::Unsupported,
        };
        if matches!(kind, ConstraintKind::Unsupported)
            || value.without_overlaps
            || value.fk_with_period
            || value.pk_with_period
            || !value.indexname.is_empty()
        {
            self.unsupported(
                location,
                "constraint uses a form outside the supported PostgreSQL analysis subset",
            );
        }
        let mut columns = names(if value.contype() == ConstrType::ConstrForeign {
            &value.fk_attrs
        } else {
            &value.keys
        })
        .unwrap_or_else(|| {
            self.unsupported(location, "could not normalize constraint columns");
            Vec::new()
        });
        if columns.is_empty()
            && let Some(column) = column
        {
            columns.push(column.to_owned());
        }
        Constraint {
            name: nonempty(&value.conname),
            columns,
            kind,
            not_valid: value.skip_validation,
            enforced: !matches!(
                value.contype(),
                ConstrType::ConstrCheck | ConstrType::ConstrForeign
            ) || value.is_enforced,
            deferrable: value.deferrable,
            initially_deferred: value.initdeferred,
        }
    }

    fn alter_table(
        &mut self,
        table: &AlterTableStmt,
        tokens: &[SqlToken<'_>],
        origin: usize,
        location: &SourceSpan,
    ) -> Option<AlterTable> {
        let rel = table.relation.as_ref()?;
        let ranges = alter_action_ranges(rel, tokens, origin, table.cmds.len());
        if ranges.is_none() {
            self.unsupported(
                location,
                "could not locate all ALTER TABLE actions precisely",
            );
        }
        let actions = table
            .cmds
            .iter()
            .enumerate()
            .map(|(index, action)| {
                let action_location = ranges
                    .as_ref()
                    .and_then(|ranges| span(self.map, self.base, ranges[index].clone()))
                    .unwrap_or_else(|| location.clone());
                let change = match node(action) {
                    Some(Ast::AlterTableCmd(cmd)) => self.alter_change(cmd, &action_location),
                    _ => None,
                }
                .unwrap_or_else(|| {
                    self.unsupported(
                        &action_location,
                        "ALTER TABLE action is outside the supported subset",
                    );
                    AlterChange::Unsupported
                });
                AlterAction {
                    span: action_location,
                    change,
                }
            })
            .collect();
        Some(AlterTable {
            relation: relation(rel)?,
            if_exists: table.missing_ok,
            only: !rel.inh,
            actions,
        })
    }

    fn alter_change(&mut self, cmd: &AlterTableCmd, location: &SourceSpan) -> Option<AlterChange> {
        let definition = cmd.def.as_deref().and_then(node);
        let name = || nonempty(&cmd.name);
        match cmd.subtype() {
            AlterTableType::AtAddColumn => {
                let Some(Ast::ColumnDef(raw_column)) = definition else {
                    return None;
                };
                let column = self.column(raw_column, location)?;
                if column.default == DefaultExpr::Unknown {
                    self.unsupported(
                        location,
                        "default expression volatility cannot be established from this migration",
                    );
                }
                Some(AlterChange::AddColumn {
                    column,
                    if_not_exists: cmd.missing_ok,
                })
            }
            AlterTableType::AtDropColumn => Some(AlterChange::DropColumn {
                name: name()?,
                if_exists: cmd.missing_ok,
                cascade: cmd.behavior() == DropBehavior::DropCascade,
            }),
            AlterTableType::AtSetNotNull => Some(AlterChange::SetNotNull { column: name()? }),
            AlterTableType::AtDropNotNull => Some(AlterChange::DropNotNull { column: name()? }),
            AlterTableType::AtColumnDefault => Some(AlterChange::SetDefault {
                column: name()?,
                default: default_expr(cmd.def.as_deref()),
            }),
            AlterTableType::AtAlterColumnType => {
                let Some(Ast::ColumnDef(column)) = definition else {
                    return None;
                };
                Some(AlterChange::ChangeType {
                    column: name()?,
                    data_type: data_type(column.type_name.as_ref()?)?,
                    using_expression: column.raw_default.is_some(),
                })
            }
            AlterTableType::AtAddConstraint | AlterTableType::AtAddIndexConstraint => {
                let Some(Ast::Constraint(constraint)) = definition else {
                    return None;
                };
                Some(AlterChange::AddConstraint(
                    self.constraint(constraint, None, location),
                ))
            }
            AlterTableType::AtDropConstraint => Some(AlterChange::DropConstraint {
                name: name()?,
                if_exists: cmd.missing_ok,
                cascade: cmd.behavior() == DropBehavior::DropCascade,
            }),
            AlterTableType::AtValidateConstraint => {
                Some(AlterChange::ValidateConstraint { name: name()? })
            }
            _ => None,
        }
    }
}

fn alter_action_ranges(
    relation: &RangeVar,
    tokens: &[SqlToken<'_>],
    origin: usize,
    count: usize,
) -> Option<Vec<std::ops::Range<usize>>> {
    let location = origin.checked_add(usize::try_from(relation.location).ok()?)?;
    let relation_start = tokens
        .iter()
        .position(|token| token.range.start == location)?;
    let parenthesized = relation_start > 0 && tokens[relation_start - 1].text == "(";
    let mut next = relation_start + 1;
    while tokens.get(next).is_some_and(|token| token.text == ".") {
        next += 2;
    }
    if tokens.get(next).is_some_and(|token| token.text == "*") {
        next += 1;
    }
    if parenthesized {
        if tokens.get(next)?.text != ")" {
            return None;
        }
        next += 1;
    }
    let actions = tokens.get(next..)?;
    let mut depth = 0;
    let mut start = 0;
    let mut groups = Vec::new();
    for (index, token) in actions.iter().enumerate() {
        if depth == 0 && token.text == "," {
            groups.push(token_range(&actions[start..index])?);
            start = index + 1;
        }
        depth = delimiter_depth(depth, token.text);
    }
    groups.push(token_range(&actions[start..])?);
    (groups.len() == count).then_some(groups)
}

fn normalize_rename(rename: &RenameStmt) -> Option<Rename> {
    let kind = match rename.rename_type() {
        ObjectType::ObjectTable => ObjectKind::Relation,
        ObjectType::ObjectColumn => ObjectKind::Column,
        ObjectType::ObjectIndex => ObjectKind::Index,
        ObjectType::ObjectTabconstraint => ObjectKind::Constraint,
        _ => return None,
    };
    Some(Rename {
        object: relation(rename.relation.as_ref()?)?,
        kind,
        member: nonempty(&rename.subname),
        new_name: nonempty(&rename.newname)?,
        if_exists: rename.missing_ok,
    })
}

fn normalize_drop(drop: &DropStmt) -> Option<DropObjects> {
    let kind = match drop.remove_type() {
        ObjectType::ObjectTable => ObjectKind::Relation,
        ObjectType::ObjectIndex => ObjectKind::Index,
        ObjectType::ObjectSchema => ObjectKind::Schema,
        ObjectType::ObjectType => ObjectKind::Type,
        _ => return None,
    };
    Some(DropObjects {
        kind,
        objects: drop
            .objects
            .iter()
            .map(named_object)
            .collect::<Option<_>>()?,
        if_exists: drop.missing_ok,
        cascade: drop.behavior() == DropBehavior::DropCascade,
        concurrently: drop.concurrent,
    })
}

fn normalize_truncate(truncate: &TruncateStmt) -> Option<Truncate> {
    let (relations, only) = truncate
        .relations
        .iter()
        .map(|table| match node(table) {
            Some(Ast::RangeVar(table)) => Some((relation(table)?, !table.inh)),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?
        .into_iter()
        .unzip();
    Some(Truncate {
        relations,
        only,
        restart_identity: truncate.restart_seqs,
        cascade: truncate.behavior() == DropBehavior::DropCascade,
    })
}

fn normalize_transaction(stmt: &TransactionStmt) -> Option<Transaction> {
    let savepoint = || nonempty(&stmt.savepoint_name);
    match stmt.kind() {
        TransactionStmtKind::TransStmtBegin | TransactionStmtKind::TransStmtStart => {
            Some(Transaction::Begin)
        }
        TransactionStmtKind::TransStmtCommit => Some(Transaction::Commit { chain: stmt.chain }),
        TransactionStmtKind::TransStmtRollback => Some(Transaction::Rollback { chain: stmt.chain }),
        TransactionStmtKind::TransStmtSavepoint => Some(Transaction::Savepoint(savepoint()?)),
        TransactionStmtKind::TransStmtRollbackTo => Some(Transaction::RollbackTo(savepoint()?)),
        TransactionStmtKind::TransStmtRelease => Some(Transaction::Release(savepoint()?)),
        _ => None,
    }
}
