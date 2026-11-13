use crate::engine;
use serde_json::{Value, json};
use std::ops::Range;

const MAX_BYTES: usize = 64 * 1024;
const MAX_NESTING: usize = 64;

pub struct Failure {
    pub message: String,
    pub byte: Option<usize>,
    pub cursor_source: Option<&'static str>,
}

pub struct Parsed {
    pub version: Option<i32>,
    pub statements: Vec<Statement>,
}

#[derive(Default)]
pub struct Statement {
    pub facts: Value,
    pub relation_start: Option<usize>,
    pub backend_locations: Value,
}

fn position(sql: &str, byte: usize) -> Value {
    let before = &sql[..byte];
    let line = before.bytes().filter(|&b| b == b'\n').count() + 1;
    let column = before.rsplit('\n').next().unwrap().chars().count() + 1;
    json!({"byte": byte, "line": line, "column": column})
}

fn span(range: &Range<usize>) -> Value {
    json!([range.start, range.end])
}

fn bounds(tokens: &[Range<usize>]) -> Range<usize> {
    tokens[0].start..tokens.last().unwrap().end
}

fn nesting(text: &str, depth: usize) -> usize {
    match text {
        "(" | "[" => depth + 1,
        ")" | "]" => depth.saturating_sub(1),
        _ => depth,
    }
}

// The corpus uses semicolon-delimited SQL, including semicolons inside tokens.
fn split<'a>(
    sql: &str,
    tokens: &'a [Range<usize>],
    delimiter: &str,
) -> Vec<(&'a [Range<usize>], usize)> {
    let mut chunks = Vec::new();
    let mut first = 0;
    let mut depth = 0;
    for (i, token) in tokens.iter().enumerate() {
        let text = &sql[token.clone()];
        if depth == 0 && text == delimiter {
            if i > first {
                chunks.push((&tokens[first..i], token.end));
            }
            first = i + 1;
        }
        depth = nesting(text, depth);
    }
    if first < tokens.len() {
        chunks.push((&tokens[first..], sql.len()));
    }
    chunks
}

fn diagnostic(code: &str, file: &str, region: Range<usize>, precision: &str) -> Value {
    json!({
        "code": code, "span": span(&region), "precision": precision,
        "anchor": position(file, region.start),
    })
}

fn error(
    code: &str,
    file: &str,
    region: Range<usize>,
    precision: &str,
    failure: Failure,
    input: Range<usize>,
) -> Value {
    let mut detail = diagnostic(code, file, region, precision);
    detail["message"] = json!(failure.message);
    if let Some(byte) = failure.byte.map(|b| input.start + b)
        && byte <= input.end
        && file.is_char_boundary(byte)
    {
        detail["cursor"] = position(file, byte);
        detail["cursor_source"] = json!(failure.cursor_source);
        detail["cursor_precision"] = json!(if byte == input.end {
            "eof"
        } else {
            "character"
        });
    }
    detail
}

fn describe(
    file: &str,
    base: usize,
    tokens: &[Range<usize>],
    stmt: Statement,
    target: u8,
    relation_kind: Option<&str>,
    diagnostics: &mut Vec<Value>,
) -> Value {
    let absolute = |r: Range<usize>| (base + r.start)..(base + r.end);
    let region = absolute(bounds(tokens));
    let sql = &file[base..];
    let mut fact = stmt.facts;
    let mut locations = json!({});
    if let Some(index) = stmt
        .relation_start
        .and_then(|start| tokens.iter().position(|token| token.start == start))
    {
        let mut last = index;
        while last + 2 < tokens.len() && &sql[tokens[last + 1].clone()] == "." {
            last += 2;
        }
        locations["relation_token"] = span(&absolute(tokens[last].clone()));

        if let Some(actions) = fact.get_mut("actions").and_then(Value::as_array_mut) {
            let groups = split(sql, &tokens[last + 1..], ",");
            if groups.len() != actions.len() {
                diagnostics.push(diagnostic(
                    "action_alignment",
                    file,
                    region.clone(),
                    "statement",
                ));
            } else {
                for (action, (group, _)) in actions.iter_mut().zip(groups) {
                    let action_region = absolute(bounds(group));
                    let code = match (
                        action["kind"].as_str(),
                        action["constraint"].as_str(),
                        action["not_valid"].as_bool(),
                    ) {
                        (Some("unsupported"), _, _) => Some("unsupported_action"),
                        (_, Some("unique"), Some(true)) => Some("invalid_constraint"),
                        (_, Some("not_null"), _) if target == 17 => Some("target_incompatible"),
                        (_, Some("foreign_key"), Some(true)) if target == 17 => match relation_kind
                        {
                            Some("partitioned") => Some("target_incompatible"),
                            Some("ordinary") => None,
                            _ => Some("target_unresolved"),
                        },
                        _ => None,
                    };
                    if let Some(code) = code {
                        diagnostics.push(diagnostic(code, file, action_region.clone(), "action"));
                    }
                    action["span"] = span(&action_region);
                    action["anchor"] = position(file, action_region.start);
                }
            }
        }
    } else if fact["kind"] == "alter_table" {
        diagnostics.push(diagnostic(
            "action_alignment",
            file,
            region.clone(),
            "statement",
        ));
    }
    if matches!(fact["kind"].as_str(), Some("do" | "unsupported")) {
        diagnostics.push(diagnostic(
            "unsupported_statement",
            file,
            region.clone(),
            "statement",
        ));
    }
    json!({
        "span": span(&region), "anchor": position(file, region.start),
        "facts": fact, "locations": locations, "backend_locations": stmt.backend_locations,
        "parse_input_start": base,
    })
}

fn preflight(code: &str, start: usize, end: usize) -> Value {
    json!({"parse": "not_run", "diagnostics": [{
        "code": code, "span": [start, end], "precision": "input",
    }]})
}

fn observe(bytes: &[u8], base: usize, target: u8, relation_kind: Option<&str>) -> Value {
    if bytes.len() > MAX_BYTES {
        return preflight("size_limit", MAX_BYTES, bytes.len());
    }
    let file = match std::str::from_utf8(bytes) {
        Ok(file) => file,
        Err(err) => {
            let start = err.valid_up_to();
            let end = err.error_len().map_or(bytes.len(), |len| start + len);
            return preflight("invalid_utf8", start, end);
        }
    };
    if let Some(start) = bytes.iter().position(|&byte| byte == 0) {
        return preflight("nul", start, start + 1);
    }
    let sql = &file[base..];
    if sql.trim_start().starts_with('\\') {
        let start = base + sql.len() - sql.trim_start().len();
        let end = file[start..]
            .find(['\r', '\n'])
            .map_or(file.len(), |len| start + len);
        return preflight("client_directive", start, end);
    }
    let tokens = match engine::scan(sql) {
        Ok(tokens) => tokens,
        Err(err) => {
            let segment = base..file.len();
            return json!({"parse": "not_run", "diagnostics": [
                error("lex", file, segment.clone(), "segment", err, segment),
            ]});
        }
    };
    let mut depth = 0;
    for (index, token) in tokens.iter().enumerate() {
        depth = nesting(&sql[token.clone()], depth);
        if depth > MAX_NESTING {
            return preflight("nesting_limit", base + token.start, base + token.end);
        }
        if &sql[token.clone()] == "{"
            && tokens
                .get(index + 1)
                .is_some_and(|next| next.start == token.end && &sql[next.clone()] == "{")
            && let Some(end) = sql[token.start..].find("}}")
        {
            let start = base + token.start;
            return preflight("template", start, start + end + 2);
        }
    }

    let chunks = split(sql, &tokens, ";");
    let mut diagnostics = Vec::new();
    let mut statements = Vec::new();
    let mut batch_error = Value::Null;
    let accepted = match engine::parse(sql) {
        Ok(parsed) => {
            if parsed.statements.len() == chunks.len() {
                for (stmt, (tokens, _)) in parsed.statements.into_iter().zip(chunks) {
                    statements.push(describe(
                        file,
                        base,
                        tokens,
                        stmt,
                        target,
                        relation_kind,
                        &mut diagnostics,
                    ));
                }
            } else {
                diagnostics.push(diagnostic(
                    "statement_alignment",
                    file,
                    base..file.len(),
                    "segment",
                ));
            }
            true
        }
        Err(err) => {
            let segment = base..file.len();
            batch_error = error(
                "batch_parse",
                file,
                segment.clone(),
                "segment",
                err,
                segment,
            );
            for (tokens, end) in chunks {
                let range = bounds(tokens);
                let start = range.start;
                let shifted: Vec<_> = tokens
                    .iter()
                    .map(|t| (t.start - start)..(t.end - start))
                    .collect();
                let region = (base + start)..(base + range.end);
                match engine::parse(&sql[start..end]) {
                    Ok(mut parsed) if parsed.statements.len() == 1 => statements.push(describe(
                        file,
                        base + start,
                        &shifted,
                        parsed.statements.remove(0),
                        target,
                        relation_kind,
                        &mut diagnostics,
                    )),
                    Ok(_) => diagnostics.push(diagnostic(
                        "statement_alignment",
                        file,
                        region,
                        "statement",
                    )),
                    Err(err) => diagnostics.push(error(
                        "parse",
                        file,
                        region,
                        "statement",
                        err,
                        (base + start)..(base + end),
                    )),
                }
            }
            // Reparse successes never erase the original batch rejection.
            if diagnostics.is_empty() {
                diagnostics.push(diagnostic("batch_parse", file, base..file.len(), "segment"));
            }
            false
        }
    };
    json!({
        "parse": if accepted { "accepted" } else { "rejected" }, "statements": statements,
        "batch_error": batch_error, "diagnostics": diagnostics,
    })
}

pub fn run(pin: &str, version: Option<i32>) {
    let valid = b"SELECT 1;";
    assert_eq!(
        engine::parse("SELECT 1;").map(|result| result.version).ok(),
        Some(version)
    );
    let deep = format!(
        "SELECT {}1{};",
        "(".repeat(MAX_NESTING + 1),
        ")".repeat(MAX_NESTING + 1)
    );
    let oversized = vec![b' '; MAX_BYTES + 1];
    let quoted = include_bytes!("fixtures/quoted.sql");
    let partitioned = include_bytes!("fixtures/part-fk.sql");
    type Case<'a> = (&'static str, &'a [u8], usize, Option<&'static str>);
    let cases: &[Case<'_>] = &[
        ("quoted", quoted, 0, None),
        ("quoted-segment", quoted, 30, None),
        ("index", include_bytes!("fixtures/index.sql"), 0, None),
        ("updates", include_bytes!("fixtures/updates.sql"), 0, None),
        (
            "fk-add",
            include_bytes!("fixtures/fk-add.sql"),
            0,
            Some("ordinary"),
        ),
        (
            "fk-validate",
            include_bytes!("fixtures/fk-validate.sql"),
            0,
            Some("ordinary"),
        ),
        (
            "not-null",
            include_bytes!("fixtures/not-null.sql"),
            0,
            Some("ordinary"),
        ),
        ("part-fk", partitioned, 0, Some("partitioned")),
        ("part-fk-unknown", partitioned, 0, None),
        (
            "unique",
            include_bytes!("fixtures/unique.sql"),
            0,
            Some("ordinary"),
        ),
        ("do", include_bytes!("fixtures/do.sql"), 0, None),
        ("broken", include_bytes!("fixtures/broken.sql"), 0, None),
        ("dollar", include_bytes!("fixtures/dollar.sql"), 0, None),
        ("eof", include_bytes!("fixtures/eof.sql"), 0, None),
        (
            "directive",
            include_bytes!("fixtures/directive.sql"),
            0,
            None,
        ),
        ("template", include_bytes!("fixtures/template.sql"), 0, None),
        ("nul", b"SELECT '\0';", 0, None),
        ("invalid-utf8", b"SELECT '\xff';", 0, None),
        ("depth", deep.as_bytes(), 0, None),
        ("size", &oversized, 0, None),
        ("after-errors", valid, 0, None),
    ];
    for target in [17, 18] {
        for &(name, bytes, base, relation_kind) in cases {
            let mut result = observe(bytes, base, target, relation_kind);
            result["case"] = json!(name);
            result["parser"] = json!(pin);
            result["grammar"] = json!(version);
            result["target"] = json!(target);
            result["segment_start"] = json!(base);
            result["relation_kind"] = json!(relation_kind);
            result["coverage"] = json!(if result["diagnostics"].as_array().unwrap().is_empty() {
                "complete"
            } else {
                "incomplete"
            });
            println!("{result}");
        }
    }
}
