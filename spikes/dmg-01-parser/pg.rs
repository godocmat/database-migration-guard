use crate::pg_parse;
use crate::probe::{Failure, Parsed, Statement};
use pg_query::protobuf::{
    AlterTableCmd, AlterTableType, ConstrType, DropBehavior, Node, NullTestType, ObjectType,
    RangeVar, Token, TransactionStmtKind, a_const::Val, node::Node as Ast,
};
use serde_json::{Value, json};
use std::ffi::{CString, c_char, c_int};
use std::ops::Range;

// Both pinned pg_query.h files use these C layouts for the Protobuf parse and scan results.
// The public Rust wrapper runs first. This ABI probe only repeats a failed call for its cursor.
#[repr(C)]
struct PgError {
    message: *mut c_char,
    function: *mut c_char,
    filename: *mut c_char,
    lineno: c_int,
    cursorpos: c_int,
    context: *mut c_char,
}

#[repr(C)]
struct PgBuffer {
    len: usize,
    data: *mut c_char,
}

#[repr(C)]
struct PgResult {
    buffer: PgBuffer,
    stderr_buffer: *mut c_char,
    error: *mut PgError,
}

unsafe extern "C" {
    fn pg_query_parse_protobuf(input: *const c_char) -> PgResult;
    fn pg_query_free_protobuf_parse_result(result: PgResult);
    fn pg_query_scan(input: *const c_char) -> PgResult;
    fn pg_query_free_scan_result(result: PgResult);
}

fn failure(sql: &str, err: pg_query::Error) -> Failure {
    let mut byte = None;
    if matches!(err, pg_query::Error::Parse(_) | pg_query::Error::Scan(_)) {
        let input = CString::new(sql).expect("preflight checked NUL");
        // SAFETY: input is NUL terminated and both bundles match these layouts.
        // Read the cursor before calling the destructor paired with the native entry point.
        let cursor = unsafe {
            let scan = matches!(err, pg_query::Error::Scan(_));
            let result = if scan {
                pg_query_scan(input.as_ptr())
            } else {
                pg_query_parse_protobuf(input.as_ptr())
            };
            let cursor = result.error.as_ref().map(|error| error.cursorpos);
            if scan {
                pg_query_free_scan_result(result);
            } else {
                pg_query_free_protobuf_parse_result(result);
            }
            cursor
        };
        byte = cursor.filter(|&pos| pos > 0).and_then(|pos| {
            sql.char_indices()
                .map(|(i, _)| i)
                .chain([sql.len()])
                .nth(pos as usize - 1)
        });
    }
    Failure {
        message: err.to_string(),
        byte,
        cursor_source: byte.map(|_| "PgQueryError.cursorpos"),
    }
}

pub fn scan(sql: &str) -> Result<Vec<Range<usize>>, Failure> {
    let result = pg_query::scan(sql).map_err(|err| failure(sql, err))?;
    Ok(result
        .tokens
        .into_iter()
        .filter(|t| !matches!(t.token(), Token::SqlComment | Token::CComment))
        .map(|t| t.start as usize..t.end as usize)
        .collect())
}

fn ast(node: &Node) -> Option<&Ast> {
    node.node.as_ref()
}

fn names(nodes: &[Node]) -> Vec<&str> {
    nodes
        .iter()
        .map(|node| match ast(node) {
            Some(Ast::String(name)) => name.sval.as_str(),
            _ => "<unsupported name>",
        })
        .collect()
}

fn relation(name: Option<&RangeVar>) -> Vec<&str> {
    name.into_iter()
        .flat_map(|n| {
            [
                n.catalogname.as_str(),
                n.schemaname.as_str(),
                n.relname.as_str(),
            ]
        })
        .filter(|name| !name.is_empty())
        .collect()
}

fn expression(node: &Node) -> Value {
    match ast(node) {
        Some(Ast::ColumnRef(c)) => json!({"column": names(&c.fields)}),
        Some(Ast::AConst(c)) => match &c.val {
            Some(Val::Ival(i)) => json!({"integer": i.ival}),
            _ => json!({"unsupported": "constant"}),
        },
        Some(Ast::NullTest(p)) if p.nulltesttype() == NullTestType::IsNotNull => {
            json!({"is_not_null": p.arg.as_deref().map(expression)})
        }
        Some(Ast::AExpr(p)) => json!({
            "operator": names(&p.name).join("."),
            "left": p.lexpr.as_deref().map(expression), "right": p.rexpr.as_deref().map(expression),
        }),
        _ => json!({"unsupported": "expression"}),
    }
}

fn constraint(node: &Node) -> Value {
    let Some(Ast::Constraint(c)) = ast(node) else {
        return json!({"kind": "unsupported"});
    };
    let (kind, columns) = match c.contype() {
        ConstrType::ConstrForeign => ("foreign_key", names(&c.fk_attrs)),
        ConstrType::ConstrNotnull => ("not_null", names(&c.keys)),
        ConstrType::ConstrUnique => ("unique", names(&c.keys)),
        _ => return json!({"kind": "unsupported"}),
    };
    json!({
        "kind": "add_constraint", "constraint": kind, "name": c.conname, "columns": columns,
        "referenced_table": relation(c.pktable.as_ref()), "referenced_columns": names(&c.pk_attrs),
        "not_valid": c.skip_validation, "initially_valid": c.initially_valid,
    })
}

fn action(cmd: &AlterTableCmd) -> Value {
    match cmd.subtype() {
        AlterTableType::AtAddColumn => match cmd.def.as_deref().and_then(ast) {
            Some(Ast::ColumnDef(c)) => json!({
                "kind": "add_column", "column": c.colname, "if_not_exists": cmd.missing_ok,
                "type": c.type_name.as_ref().map(|t| names(&t.names).join(".")),
            }),
            _ => json!({"kind": "unsupported"}),
        },
        AlterTableType::AtDropColumn => json!({
            "kind": "drop_column", "columns": [cmd.name], "if_exists": cmd.missing_ok,
            "behavior": if cmd.behavior() == DropBehavior::DropCascade { "cascade" } else { "restrict" },
        }),
        AlterTableType::AtAddConstraint => cmd
            .def
            .as_deref()
            .map_or_else(|| json!({"kind": "unsupported"}), constraint),
        AlterTableType::AtValidateConstraint => {
            json!({"kind": "validate_constraint", "name": cmd.name})
        }
        _ => json!({"kind": "unsupported"}),
    }
}

fn describe(node: Option<&Ast>) -> (Value, Option<&RangeVar>) {
    match node {
        Some(Ast::AlterTableStmt(stmt)) => (
            json!({
                "kind": "alter_table", "relation": relation(stmt.relation.as_ref()), "if_exists": stmt.missing_ok,
                "actions": stmt.cmds.iter().map(|node| match ast(node) {
                    Some(Ast::AlterTableCmd(cmd)) => action(cmd),
                    _ => json!({"kind": "unsupported"}),
                }).collect::<Vec<_>>(),
            }),
            stmt.relation.as_ref(),
        ),
        Some(Ast::IndexStmt(stmt)) => (
            json!({
                "kind": "create_index", "name": stmt.idxname, "relation": relation(stmt.relation.as_ref()),
                "keys": stmt.index_params.iter().map(|node| match ast(node) {
                    Some(Ast::IndexElem(elem)) => elem.name.as_str(),
                    _ => "<unsupported key>",
                }).collect::<Vec<_>>(),
                "predicate": stmt.where_clause.as_deref().map(expression),
                "unique": stmt.unique, "concurrent": stmt.concurrent, "if_not_exists": stmt.if_not_exists,
            }),
            stmt.relation.as_ref(),
        ),
        Some(Ast::UpdateStmt(stmt)) => (
            json!({
                "kind": "update", "relation": relation(stmt.relation.as_ref()),
                "assignments": stmt.target_list.iter().map(|node| match ast(node) {
                    Some(Ast::ResTarget(target)) => json!({
                        "column": target.name, "value": target.val.as_deref().map(expression),
                    }),
                    _ => json!({"unsupported": "assignment"}),
                }).collect::<Vec<_>>(),
                "predicate": stmt.where_clause.as_deref().map(expression),
            }),
            stmt.relation.as_ref(),
        ),
        Some(Ast::TransactionStmt(stmt)) => {
            let kind = match stmt.kind() {
                TransactionStmtKind::TransStmtBegin => "begin",
                TransactionStmtKind::TransStmtCommit => "commit",
                _ => "unsupported",
            };
            (json!({"kind": kind}), None)
        }
        Some(Ast::SelectStmt(_)) => (json!({"kind": "select"}), None),
        Some(Ast::DoStmt(_)) => (json!({"kind": "do"}), None),
        Some(Ast::DropStmt(stmt)) if stmt.remove_type() == ObjectType::ObjectTable => (
            json!({
                "kind": "drop_table",
                "relations": stmt.objects.iter().map(|node| match ast(node) {
                    Some(Ast::List(list)) => names(&list.items),
                    _ => Vec::new(),
                }).collect::<Vec<_>>(),
            }),
            None,
        ),
        _ => (json!({"kind": "unsupported"}), None),
    }
}

pub fn parse(sql: &str) -> Result<Parsed, Failure> {
    let parsed = pg_parse(sql).map_err(|err| failure(sql, err))?.protobuf;
    Ok(Parsed {
        version: Some(parsed.version),
        statements: parsed
            .stmts
            .iter()
            .map(|stmt| {
                let (facts, relation) = describe(stmt.stmt.as_deref().and_then(ast));
                let relation_start = relation.and_then(|r| r.location.try_into().ok());
                Statement {
                    facts,
                    relation_start,
                    backend_locations: json!({
                        "raw_stmt_location": stmt.stmt_location, "raw_stmt_len": stmt.stmt_len,
                        "range_var_start": relation_start,
                    }),
                }
            })
            .collect(),
    })
}
