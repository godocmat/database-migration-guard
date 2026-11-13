//! Resolve the Supabase CLI 3.0.0 execution plan from original statement text.

use dmg_core::{
    coverage::{CoverageGap, CoverageGapKind},
    migration::{ExecutionMode, StatementDelivery},
    risk::ObjectKind,
};

use crate::{Operation, ParsedMigration, Transaction, parse::span_gap};

pub(crate) fn resolve_execution(
    migration: &mut ParsedMigration,
    text: &str,
    gaps: &mut Vec<CoverageGap>,
) {
    // Parse recovery can omit a statement that changes the rest of the
    // file's execution mode. The existing gap already makes that uncertain.
    if gaps
        .iter()
        .any(|gap| gap.kind == CoverageGapKind::ParseError)
    {
        return;
    }

    if migration.execution != ExecutionMode::SUPABASE_BATCH {
        return;
    }

    let mut sequential = false;
    let mut ambiguous = Vec::new();
    for statement in &migration.statements {
        // Token spans omit leading trivia, as the CLI does. Include trailing
        // trivia before the separator so an adjacent comment still affects
        // matching of a bare transaction keyword.
        let range = statement.span.bytes.as_range();
        let tail = &text[range.end..];
        let tail = tail.split_once(';').map_or(tail, |(part, _)| part);
        let sql = text[range.start..range.end + tail.len()].trim_end_matches(js_whitespace);
        let native_boundary = owns_transaction_boundary(sql);
        sequential |= native_boundary;

        if let Operation::Transaction(transaction) = &statement.operation {
            let postgres_boundary = matches!(
                transaction,
                Transaction::Begin | Transaction::Commit { .. } | Transaction::Rollback { .. }
            );
            if native_boundary != postgres_boundary {
                ambiguous.push(&statement.span);
            }
        }
    }

    if sequential {
        migration.execution = ExecutionMode::PSQL_AUTOCOMMIT;
        for statement in &mut migration.statements {
            statement.delivery = StatementDelivery::PerStatement;
        }
        return;
    }
    for span in ambiguous {
        gaps.push(span_gap(
            span.clone(),
            CoverageGapKind::UnresolvedTransactionMode,
            "Supabase and PostgreSQL classify this transaction boundary differently",
        ));
    }

    let mut group = 0;
    let mut pending = false;
    for statement in &mut migration.statements {
        let sql = &text[statement.span.bytes.as_range()];
        let concurrent = match &statement.operation {
            Operation::CreateIndex(index) => index.concurrently,
            Operation::Drop(drop) => drop.kind == ObjectKind::Index && drop.concurrently,
            _ => false,
        };
        if standalone(sql) {
            // A standalone statement ends any preceding batch and starts a
            // new one for the next ordinary statement.
            group += usize::from(pending);
            statement.segment = group;
            statement.delivery = StatementDelivery::PerStatement;
            group += 1;
            pending = false;
        } else {
            statement.segment = group;
            if concurrent && !pending {
                // PostgreSQL has not set its pipelining flag before the
                // first nonempty Execute. A transaction-restricted command
                // can then commit immediately, even though the CLI put it
                // in the batch. This differs from the ordinary batch model.
                gaps.push(span_gap(
                    statement.span.clone(),
                    CoverageGapKind::UnresolvedTransactionMode,
                    "Supabase did not split this concurrent index statement at the start of the batch",
                ));
            }
            pending = true;
        }
    }
}

fn js_whitespace(ch: char) -> bool {
    // ECMAScript WhiteSpace and LineTerminator characters.
    matches!(
        ch,
        ' ' | '\t' | '\n' | '\r' | '\u{000b}' | '\u{000c}' | '\u{00a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

fn owns_transaction_boundary(sql: &str) -> bool {
    let mut words = sql.split(js_whitespace).filter(|word| !word.is_empty());
    let first = words.next().unwrap_or_default();
    if ["BEGIN", "COMMIT", "END", "ABORT"]
        .iter()
        .any(|keyword| first.eq_ignore_ascii_case(keyword))
    {
        return true;
    }
    let second = words.next().unwrap_or_default();
    if first.eq_ignore_ascii_case("ROLLBACK") {
        let to =
            if second.eq_ignore_ascii_case("WORK") || second.eq_ignore_ascii_case("TRANSACTION") {
                words.next().unwrap_or_default()
            } else {
                second
            };
        return !to.eq_ignore_ascii_case("TO");
    }
    (first.eq_ignore_ascii_case("START") || first.eq_ignore_ascii_case("PREPARE"))
        && second.eq_ignore_ascii_case("TRANSACTION")
}

fn standalone(sql: &str) -> bool {
    let mut words = sql.split(js_whitespace).filter(|word| !word.is_empty());
    let first = words.next().unwrap_or_default();
    if first.eq_ignore_ascii_case("CREATE") {
        let mut second = words.next().unwrap_or_default();
        if second.eq_ignore_ascii_case("UNIQUE") {
            second = words.next().unwrap_or_default();
        }
        return second.eq_ignore_ascii_case("INDEX")
            && words
                .next()
                .is_some_and(|word| word.eq_ignore_ascii_case("CONCURRENTLY"));
    }
    if first.eq_ignore_ascii_case("DROP") {
        return words
            .next()
            .is_some_and(|word| word.eq_ignore_ascii_case("INDEX"))
            && words
                .next()
                .is_some_and(|word| word.eq_ignore_ascii_case("CONCURRENTLY"));
    }

    // These operations are outside the rule engine's normalized SQL subset.
    // Preserve the execution boundary when the pinned runner recognizes one.
    if first.eq_ignore_ascii_case("ALTER") {
        return words
            .next()
            .is_some_and(|word| word.eq_ignore_ascii_case("SYSTEM"));
    }
    for command in ["VACUUM", "CLUSTER", "REINDEX"] {
        let Some((prefix, rest)) = sql.get(..command.len()).zip(sql.get(command.len()..)) else {
            continue;
        };
        if !prefix.eq_ignore_ascii_case(command)
            || !rest
                .chars()
                .next()
                .is_none_or(|next| next == '(' || js_whitespace(next))
        {
            continue;
        }
        if command != "REINDEX" {
            return true;
        }

        // JavaScript's dot in the pinned REINDEX pattern does not match line terminators.
        let Some(first_delimiter) = rest.chars().next() else {
            return false;
        };
        let rest = &rest[first_delimiter.len_utf8()..];
        return rest
            .split_inclusive(['\r', '\n', '\u{2028}', '\u{2029}'])
            .next()
            .is_some_and(|line| {
                line.char_indices().any(|(index, ch)| {
                    js_whitespace(ch)
                        && line[index + ch.len_utf8()..]
                            .split(js_whitespace)
                            .next()
                            .is_some_and(|word| word.eq_ignore_ascii_case("CONCURRENTLY"))
                })
            });
    }
    false
}
