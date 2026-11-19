use pg_query::protobuf::{
    AExpr, AExprKind, BoolExprType, Node, NullTestType, ReturningClause, TypeName, a_const::Val,
    node::Node as Ast,
};

use crate::{
    DefaultExpr, Predicate,
    normalize::{names, node},
};

const MAX_EXPRESSION_DEPTH: usize = 32;

pub(crate) fn default_expr(value: Option<&Node>) -> DefaultExpr {
    value.map_or(DefaultExpr::Absent, |value| {
        classify_default(value, MAX_EXPRESSION_DEPTH)
    })
}

fn classify_default(value: &Node, depth: usize) -> DefaultExpr {
    if depth == 0 {
        return DefaultExpr::Unknown;
    }
    match node(value) {
        Some(Ast::AConst(value)) if value.isnull => DefaultExpr::Null,
        Some(Ast::AConst(_)) => DefaultExpr::Constant,
        Some(Ast::SqlvalueFunction(_)) => DefaultExpr::Stable,
        Some(Ast::TypeCast(cast)) if cast.type_name.as_ref().is_some_and(builtin_cast) => cast
            .arg
            .as_deref()
            .map_or(DefaultExpr::Unknown, |arg| classify_default(arg, depth - 1)),
        Some(Ast::FuncCall(call)) => {
            let Some(name) = names(&call.funcname) else {
                return DefaultExpr::Unknown;
            };
            match (builtin_name(&name), call.args.len()) {
                (Some("clock_timestamp" | "random" | "gen_random_uuid" | "timeofday"), 0)
                | (Some("nextval"), 1) => DefaultExpr::Volatile,
                (Some("now" | "statement_timestamp" | "transaction_timestamp"), 0) => {
                    DefaultExpr::Stable
                }
                _ => DefaultExpr::Unknown,
            }
        }
        _ => DefaultExpr::Unknown,
    }
}

fn builtin_name(name: &[String]) -> Option<&str> {
    match name {
        [name] => Some(name),
        [schema, name] if schema == "pg_catalog" => Some(name),
        _ => None,
    }
}

fn builtin_cast(value: &TypeName) -> bool {
    value.array_bounds.is_empty()
        && names(&value.names)
            .and_then(|parts| builtin_name(&parts).map(str::to_owned))
            .is_some_and(|name| {
                matches!(
                    name.as_str(),
                    "bool"
                        | "boolean"
                        | "bpchar"
                        | "bytea"
                        | "char"
                        | "date"
                        | "float4"
                        | "float8"
                        | "int2"
                        | "int4"
                        | "int8"
                        | "integer"
                        | "json"
                        | "jsonb"
                        | "numeric"
                        | "real"
                        | "smallint"
                        | "bigint"
                        | "text"
                        | "time"
                        | "timestamp"
                        | "timestamptz"
                        | "timetz"
                        | "uuid"
                        | "varchar"
                )
            })
}

pub(crate) fn check_proof(value: &Node) -> Vec<String> {
    let mut columns = Vec::new();
    collect_check_proof(value, MAX_EXPRESSION_DEPTH, &mut columns);
    columns.sort();
    columns.dedup();
    columns
}

fn collect_check_proof(value: &Node, depth: usize, columns: &mut Vec<String>) {
    if depth == 0 {
        return;
    }
    match node(value) {
        Some(Ast::BoolExpr(test)) if test.boolop() == BoolExprType::AndExpr => {
            for arg in &test.args {
                collect_check_proof(arg, depth - 1, columns);
            }
        }
        Some(Ast::NullTest(test))
            if !test.argisrow && test.nulltesttype() == NullTestType::IsNotNull =>
        {
            if let Some(Ast::ColumnRef(column)) = test.arg.as_deref().and_then(node)
                && let Some([column]) = names(&column.fields).as_deref()
            {
                columns.push(column.clone());
            }
        }
        _ => (),
    }
}

pub(crate) fn predicate(value: Option<&Node>) -> Predicate {
    match value {
        None => Predicate::Absent,
        Some(value) => match truth(value, MAX_EXPRESSION_DEPTH) {
            Some(true) => Predicate::AlwaysTrue,
            Some(false) => Predicate::AlwaysFalse,
            None => Predicate::Filtered,
        },
    }
}

fn truth(value: &Node, depth: usize) -> Option<bool> {
    if depth == 0 {
        return None;
    }
    match node(value)? {
        Ast::AConst(value) => match &value.val {
            Some(Val::Boolval(boolean)) => Some(boolean.boolval),
            _ => None,
        },
        Ast::BoolExpr(expr) => match expr.boolop() {
            BoolExprType::NotExpr => {
                let [arg] = expr.args.as_slice() else {
                    return None;
                };
                truth(arg, depth - 1).map(|value| !value)
            }
            BoolExprType::AndExpr => connective(&expr.args, depth, false),
            BoolExprType::OrExpr => connective(&expr.args, depth, true),
            _ => None,
        },
        Ast::AExpr(expr) if expr.kind() == AExprKind::AexprOp => compare_integers(expr),
        _ => None,
    }
}

fn connective(args: &[Node], depth: usize, decisive: bool) -> Option<bool> {
    let mut all_known = true;
    for arg in args {
        match truth(arg, depth - 1) {
            Some(value) if value == decisive => return Some(decisive),
            None => all_known = false,
            _ => (),
        }
    }
    all_known.then_some(!decisive)
}

fn compare_integers(expr: &AExpr) -> Option<bool> {
    fn integer(value: Option<&Node>) -> Option<i32> {
        let Ast::AConst(value) = node(value?)? else {
            return None;
        };
        let Val::Ival(value) = value.val.as_ref()? else {
            return None;
        };
        Some(value.ival)
    }
    let left = integer(expr.lexpr.as_deref())?;
    let right = integer(expr.rexpr.as_deref())?;
    Some(match builtin_name(&names(&expr.name)?)? {
        "=" => left == right,
        "<>" | "!=" => left != right,
        ">" => left > right,
        "<" => left < right,
        ">=" => left >= right,
        "<=" => left <= right,
        _ => return None,
    })
}

pub(crate) fn simple_value(value: &Node) -> bool {
    supported_value(value, MAX_EXPRESSION_DEPTH)
}

fn supported_value(value: &Node, depth: usize) -> bool {
    if depth == 0 {
        return false;
    }
    let child = |value: &Node| supported_value(value, depth - 1);
    match node(value) {
        Some(
            Ast::AConst(_) | Ast::ColumnRef(_) | Ast::SetToDefault(_) | Ast::SqlvalueFunction(_),
        ) => true,
        Some(Ast::AExpr(expr)) => {
            expr.lexpr.as_deref().is_none_or(child) && expr.rexpr.as_deref().is_none_or(child)
        }
        Some(Ast::BoolExpr(expr)) => expr.args.iter().all(child),
        Some(Ast::NullTest(expr)) => expr.arg.as_deref().is_some_and(child),
        Some(Ast::TypeCast(expr)) => {
            expr.type_name.as_ref().is_some_and(builtin_cast)
                && expr.arg.as_deref().is_some_and(child)
        }
        Some(Ast::List(values)) => values.items.iter().all(child),
        _ => false,
    }
}

pub(crate) fn returning_is_supported(returning: Option<&ReturningClause>) -> bool {
    returning.is_none_or(|returning| {
        returning.options.is_empty() && returning.exprs.iter().all(|target| {
            matches!(node(target), Some(Ast::ResTarget(target)) if target.val.as_deref().is_some_and(simple_value))
        })
    })
}
