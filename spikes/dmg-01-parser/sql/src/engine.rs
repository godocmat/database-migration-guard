use crate::probe::{Failure, Parsed, Statement};
use serde_json::{Value, json};
use sqlparser::ast::{
    AlterTableOperation, AssignmentTarget, DropBehavior, Expr, Ident, ObjectName, ObjectType,
    Spanned, Statement as Ast, TableConstraint, TableFactor, Value as SqlValue,
};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::{Parser, ParserError};
use sqlparser::tokenizer::{Location, Span, Token, Tokenizer};
use std::ops::Range;

const RECURSION_LIMIT: usize = 64;

fn byte(sql: &str, loc: Location) -> Option<usize> {
    let mut start = 0;
    let column = usize::try_from(loc.column).ok()?.checked_sub(1)?;
    for (index, text) in sql.split('\n').enumerate() {
        if (index + 1) as u64 == loc.line {
            return text
                .char_indices()
                .map(|(byte, _)| start + byte)
                .chain([start + text.len()])
                .nth(column);
        }
        start += text.len() + 1;
    }
    None
}

fn range(sql: &str, span: Span) -> Option<Range<usize>> {
    Some(byte(sql, span.start)?..byte(sql, span.end)?)
}

pub fn scan(sql: &str) -> Result<Vec<Range<usize>>, Failure> {
    let tokens = Tokenizer::new(&PostgreSqlDialect {}, sql)
        .tokenize_with_location()
        .map_err(|err| Failure {
            message: err.message,
            byte: byte(sql, err.location),
            cursor_source: Some("TokenizerError.location"),
        })?;
    tokens
        .into_iter()
        .filter(|t| !matches!(t.token, Token::Whitespace(_) | Token::EOF))
        .map(|token| {
            range(sql, token.span).ok_or_else(|| Failure {
                message: "token span has no original byte range".into(),
                byte: None,
                cursor_source: None,
            })
        })
        .collect()
}

fn ident(value: &Ident) -> String {
    if value.quote_style.is_some() {
        value.value.clone()
    } else {
        value.value.to_ascii_lowercase()
    }
}

fn name(value: &ObjectName) -> Vec<String> {
    value
        .0
        .iter()
        .filter_map(|part| part.as_ident().map(ident))
        .collect()
}

fn expression(value: &Expr) -> Value {
    match value {
        Expr::Identifier(column) => json!({"column": [ident(column)]}),
        Expr::IsNotNull(arg) => json!({"is_not_null": expression(arg)}),
        Expr::BinaryOp { left, op, right } => json!({
            "operator": op.to_string(), "left": expression(left), "right": expression(right),
        }),
        Expr::Value(value) => match &value.value {
            SqlValue::Number(number, _) => match number.parse::<i32>() {
                Ok(integer) => json!({"integer": integer}),
                Err(_) => json!({"unsupported": "constant"}),
            },
            _ => json!({"unsupported": "constant"}),
        },
        _ => json!({"unsupported": "expression"}),
    }
}

fn key(value: &Expr) -> Option<String> {
    match value {
        Expr::Identifier(column) => Some(ident(column)),
        _ => None,
    }
}

fn action(operation: &AlterTableOperation) -> Value {
    match operation {
        AlterTableOperation::AddColumn {
            column_def,
            if_not_exists,
            ..
        } => json!({
            "kind": "add_column", "column": ident(&column_def.name), "if_not_exists": if_not_exists,
            "type": column_def.data_type.to_string().to_ascii_lowercase(),
        }),
        AlterTableOperation::DropColumn {
            column_names,
            if_exists,
            drop_behavior,
            ..
        } => json!({
            "kind": "drop_column", "columns": column_names.iter().map(ident).collect::<Vec<_>>(),
            "if_exists": if_exists,
            "behavior": if *drop_behavior == Some(DropBehavior::Cascade) { "cascade" } else { "restrict" },
        }),
        AlterTableOperation::ValidateConstraint { name } => json!({
            "kind": "validate_constraint", "name": ident(name),
        }),
        AlterTableOperation::AddConstraint {
            constraint: TableConstraint::ForeignKey(fk),
            not_valid,
        } => json!({
            "kind": "add_constraint", "constraint": "foreign_key", "name": fk.name.as_ref().map(ident),
            "columns": fk.columns.iter().map(ident).collect::<Vec<_>>(),
            "referenced_table": name(&fk.foreign_table),
            "referenced_columns": fk.referred_columns.iter().map(ident).collect::<Vec<_>>(),
            "not_valid": not_valid,
        }),
        AlterTableOperation::AddConstraint {
            constraint: TableConstraint::Unique(unique),
            not_valid,
        } => json!({
            "kind": "add_constraint", "constraint": "unique", "name": unique.name.as_ref().map(ident),
            "not_valid": not_valid,
        }),
        _ => json!({"kind": "unsupported"}),
    }
}

fn describe(stmt: &Ast) -> (Value, Option<&ObjectName>) {
    match stmt {
        Ast::AlterTable(stmt) => (
            json!({
                "kind": "alter_table", "relation": name(&stmt.name), "if_exists": stmt.if_exists,
                "actions": stmt.operations.iter().map(action).collect::<Vec<_>>(),
            }),
            Some(&stmt.name),
        ),
        Ast::CreateIndex(stmt) => (
            json!({
                "kind": "create_index", "name": stmt.name.as_ref().map(|n| name(n).join(".")),
                "relation": name(&stmt.table_name),
                "keys": stmt.columns.iter().map(|c| key(&c.column.expr)).collect::<Vec<_>>(),
                "predicate": stmt.predicate.as_ref().map(expression),
                "unique": stmt.unique, "concurrent": stmt.concurrently, "if_not_exists": stmt.if_not_exists,
            }),
            Some(&stmt.table_name),
        ),
        Ast::Update(stmt) => match &stmt.table.relation {
            TableFactor::Table { name: relation, .. } => (
                json!({
                    "kind": "update", "relation": name(relation), "predicate": stmt.selection.as_ref().map(expression),
                    "assignments": stmt.assignments.iter().map(|assignment| match &assignment.target {
                        AssignmentTarget::ColumnName(column) => json!({
                            "column": name(column).join("."), "value": expression(&assignment.value),
                        }),
                        _ => json!({"unsupported": "assignment"}),
                    }).collect::<Vec<_>>(),
                }),
                Some(relation),
            ),
            _ => (json!({"kind": "unsupported"}), None),
        },
        Ast::StartTransaction { .. } => (json!({"kind": "begin"}), None),
        Ast::Commit { .. } => (json!({"kind": "commit"}), None),
        Ast::Query(_) => (json!({"kind": "select"}), None),
        Ast::Drop {
            object_type: ObjectType::Table,
            names,
            ..
        } => (
            json!({
                "kind": "drop_table", "relations": names.iter().map(name).collect::<Vec<_>>(),
            }),
            None,
        ),
        _ => (json!({"kind": "unsupported"}), None),
    }
}

fn ast_span(sql: &str, value: &impl Spanned) -> Value {
    json!(range(sql, value.span()).map(|r| [r.start, r.end]))
}

fn parse_failure(sql: &str, message: String) -> Failure {
    let formatted = message.rsplit_once(" at Line: ").and_then(|(_, loc)| {
        let (line, column) = loc.split_once(", Column: ")?;
        byte(
            sql,
            Location {
                line: line.parse().ok()?,
                column: column.parse().ok()?,
            },
        )
    });
    Failure {
        message,
        byte: formatted,
        cursor_source: formatted.map(|_| "formatted ParserError"),
    }
}

fn parse_ast(sql: &str) -> Result<Vec<Ast>, ParserError> {
    Parser::new(&PostgreSqlDialect {})
        .with_recursion_limit(RECURSION_LIMIT)
        .try_with_sql(sql)
        .and_then(|mut parser| parser.parse_statements())
}

pub fn parse(sql: &str) -> Result<Parsed, Failure> {
    let ast = parse_ast(sql).map_err(|err| parse_failure(sql, err.to_string()))?;
    Ok(Parsed {
        version: None,
        statements: ast
            .iter()
            .map(|stmt| {
                let (facts, relation) = describe(stmt);
                let action_spans = match stmt {
                    Ast::AlterTable(stmt) => {
                        stmt.operations.iter().map(|op| ast_span(sql, op)).collect()
                    }
                    _ => Vec::new(),
                };
                Statement {
                    facts,
                    relation_start: relation.and_then(|n| byte(sql, n.span().start)),
                    backend_locations: json!({
                        "spanned_stmt": ast_span(sql, stmt), "spanned_actions": action_spans,
                        "spanned_relation": relation.map(|n| ast_span(sql, n)),
                    }),
                }
            })
            .collect(),
    })
}
