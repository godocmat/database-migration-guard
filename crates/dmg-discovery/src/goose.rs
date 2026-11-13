//! Goose v3.30.1 SQL file discovery and query grouping.
use dmg_core::{
    coverage::{CoverageGap, CoverageGapKind, DiagnosticScope},
    migration::{
        ExecutionMode, MigrationAdapter, MigrationDirection, MigrationIdentity, StatementDelivery,
        TransactionWrapper,
    },
    source::{ByteRange, RepoPath, SourceMap},
};

use crate::{Candidate, SourceLayout};

const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;
const MAX_QUERIES: usize = 1_024;
const MISSING_UP: &str = "Goose requires an Up annotation before migration SQL";
const UNFINISHED: &str = "Goose query has no terminating semicolon or StatementEnd";

pub(super) fn candidate(filename: &str) -> Result<Option<Candidate>, &'static str> {
    if filename.ends_with(".go") {
        if !filename.ends_with("_test.go") && version(filename).is_some() {
            return Err("Goose Go migrations require a custom runner and are unsupported");
        }
        return Ok(None);
    }
    if !filename.ends_with(".sql") {
        return Ok(None);
    }
    let version = version(filename)
        .ok_or("Goose SQL filenames require a positive i64 version before the first underscore")?;
    Ok(Some(Candidate {
        identity: MigrationIdentity::numeric(MigrationAdapter::Goose, version),
        direction: MigrationDirection::Forward,
    }))
}

fn version(filename: &str) -> Option<i64> {
    let (prefix, _) = filename.split_once('_')?;
    prefix.parse::<i64>().ok().filter(|version| *version > 0)
}

pub(super) fn source_layouts(
    path: &RepoPath,
    bytes: &[u8],
) -> (Vec<SourceLayout>, Option<CoverageGap>) {
    let Ok(source) = std::str::from_utf8(bytes) else {
        let gap = crate::coverage_gap(
            CoverageGapKind::UnsupportedInput,
            Some(path),
            "Goose SQL migration bytes must be UTF-8 for offline analysis",
        );
        return (Vec::new(), Some(gap));
    };
    match Script::read(source) {
        Ok(layouts) => (layouts, None),
        Err(issue) => {
            let scope = SourceMap::new(path.clone(), source)
                .span(issue.range)
                .map_or_else(
                    |_| DiagnosticScope::Path { path: path.clone() },
                    |span| DiagnosticScope::Span { span },
                );
            (
                Vec::new(),
                Some(CoverageGap {
                    kind: issue.kind,
                    scope,
                    message: issue.message.into(),
                    related_paths: Vec::new(),
                }),
            )
        }
    }
}

#[derive(Clone, Copy)]
struct Line<'a> {
    text: &'a str,
    start: usize,
}

impl Line<'_> {
    fn range(self) -> ByteRange {
        range(self.start, self.start + self.text.len())
    }
}

#[derive(Debug)]
struct Issue {
    range: ByteRange,
    kind: CoverageGapKind,
    message: &'static str,
}

impl Issue {
    fn unsupported(range: ByteRange, message: &'static str) -> Self {
        Self {
            range,
            kind: CoverageGapKind::UnsupportedInput,
            message,
        }
    }

    fn limit(range: ByteRange, message: &'static str) -> Self {
        Self {
            range,
            kind: CoverageGapKind::InputLimitExceeded,
            message,
        }
    }
}

#[derive(Clone, Copy)]
enum Annotation {
    Up,
    Down,
    StatementBegin,
    StatementEnd,
    NoTransaction,
}

fn annotation(line: Line<'_>) -> Result<Option<Annotation>, Issue> {
    if !line.text.trim_start().starts_with("--") || !line.text.contains("+goose") {
        return Ok(None);
    }
    let invalid = || Issue::unsupported(line.range(), "invalid or unsupported Goose annotation");
    // Use the documented marker order. Odd spellings accepted by the native string
    // replacement are out of scope so ambiguous source cannot pass the gate.
    let marker = line
        .text
        .strip_prefix("--")
        .ok_or_else(invalid)?
        .trim_start();
    let command = marker.strip_prefix("+goose").ok_or_else(invalid)?;
    if !command.starts_with(char::is_whitespace) {
        return Err(invalid());
    }
    let command = command.trim();
    for (name, annotation) in [
        ("Up", Annotation::Up),
        ("Down", Annotation::Down),
        ("StatementBegin", Annotation::StatementBegin),
        ("StatementEnd", Annotation::StatementEnd),
        ("NO TRANSACTION", Annotation::NoTransaction),
    ] {
        if command.eq_ignore_ascii_case(name) {
            return Ok(Some(annotation));
        }
    }
    if command.eq_ignore_ascii_case("ENVSUB ON") || command.eq_ignore_ascii_case("ENVSUB OFF") {
        return Err(Issue::unsupported(
            line.range(),
            "Goose ENVSUB requires runtime environment substitution",
        ));
    }
    Err(invalid())
}

#[derive(Default)]
struct Script {
    direction: Option<MigrationDirection>,
    up_offset: usize,
    forward: Vec<ByteRange>,
    down: Vec<ByteRange>,
    no_transaction: bool,
    group_start: Option<ByteRange>,
    query: Option<ByteRange>,
    buffering: bool,
}

impl Script {
    fn read(source: &str) -> Result<Vec<SourceLayout>, Issue> {
        let mut script = Self::default();
        if source.starts_with('\u{feff}') {
            return Err(Issue::unsupported(
                range(0, '\u{feff}'.len_utf8()),
                "Goose does not remove a UTF-8 BOM before the Up annotation",
            ));
        }

        let mut start = 0;
        for raw in source.split_inclusive('\n') {
            let text = raw.strip_suffix('\n').unwrap_or(raw);
            let text = text.strip_suffix('\r').unwrap_or(text);
            let line = Line { text, start };
            start += raw.len();
            if raw.len() > MAX_LINE_BYTES || (raw.len() == MAX_LINE_BYTES && !raw.ends_with('\n')) {
                return Err(Issue::limit(
                    line.range(),
                    "Goose migration line exceeds the native scanner limit",
                ));
            }
            if let Some(annotation) = annotation(line)? {
                script.apply_annotation(annotation, line.range())?;
            } else {
                script.push_line(line)?;
            }
        }
        if let Some(group_start) = script.group_start {
            return Err(Issue::unsupported(
                group_start,
                "Goose StatementBegin has no matching StatementEnd",
            ));
        }
        if let Some(query) = script.query {
            return Err(Issue::unsupported(query, UNFINISHED));
        }
        if script.direction.is_none() {
            return Err(Issue::unsupported(
                range(source.len(), source.len()),
                MISSING_UP,
            ));
        }

        let execution = ExecutionMode {
            transaction_wrapper: if script.no_transaction {
                TransactionWrapper::None
            } else {
                TransactionWrapper::File
            },
            statement_delivery: StatementDelivery::SingleQuery,
        };
        if script.forward.is_empty() {
            script
                .forward
                .push(range(script.up_offset, script.up_offset));
        }
        let mut layouts = vec![SourceLayout {
            direction: MigrationDirection::Forward,
            source_segments: script.forward,
            execution,
        }];
        if script.direction == Some(MigrationDirection::Down) {
            layouts.push(SourceLayout {
                direction: MigrationDirection::Down,
                source_segments: script.down,
                execution,
            });
        }
        Ok(layouts)
    }

    fn apply_annotation(&mut self, annotation: Annotation, at: ByteRange) -> Result<(), Issue> {
        match annotation {
            Annotation::Up if self.direction.is_none() => {
                self.direction = Some(MigrationDirection::Forward);
                self.up_offset = at.end();
            }
            Annotation::Up => {
                return Err(Issue::unsupported(
                    at,
                    "Goose migration repeats the Up annotation",
                ));
            }
            Annotation::Down
                if self.direction == Some(MigrationDirection::Forward)
                    && self.group_start.is_none() =>
            {
                if let Some(query) = self.query {
                    return Err(Issue::unsupported(query, UNFINISHED));
                }
                self.direction = Some(MigrationDirection::Down);
            }
            Annotation::Down => {
                return Err(Issue::unsupported(
                    at,
                    "Goose Down must follow Up outside a statement group",
                ));
            }
            Annotation::StatementBegin
                if self.direction.is_some() && self.group_start.is_none() =>
            {
                if self.buffering {
                    return Err(Issue::unsupported(
                        at,
                        "Goose StatementBegin inside a buffered query has no contiguous original source range",
                    ));
                }
                self.group_start = Some(at);
            }
            Annotation::StatementBegin => {
                return Err(Issue::unsupported(
                    at,
                    "Goose StatementBegin must follow Up or Down outside an open group",
                ));
            }
            Annotation::StatementEnd if self.group_start.is_some() => {
                if self.query.is_none() {
                    return Err(Issue::unsupported(
                        at,
                        "Goose empty statement group has ambiguous query boundaries",
                    ));
                }
                self.flush_query()?;
                self.group_start = None;
            }
            Annotation::StatementEnd => {
                return Err(Issue::unsupported(
                    at,
                    "Goose StatementEnd has no matching StatementBegin",
                ));
            }
            Annotation::NoTransaction => {
                if self.buffering {
                    return Err(Issue::unsupported(
                        at,
                        "Goose NO TRANSACTION inside a buffered query has no contiguous original source range",
                    ));
                }
                self.no_transaction = true;
            }
        }
        Ok(())
    }

    fn push_line(&mut self, line: Line<'_>) -> Result<(), Issue> {
        if !self.buffering && (line.text.trim_start().starts_with("--") || line.text.is_empty()) {
            return Ok(());
        }
        self.buffering = true;
        if self.direction.is_none() {
            return if line.text.trim().is_empty() {
                Ok(())
            } else {
                Err(Issue::unsupported(line.range(), MISSING_UP))
            };
        }

        // Leading and trailing Unicode whitespace is trimmed by the native parser.
        // Keep interior newlines in the original range for PostgreSQL source mapping.
        let start = self.query.map_or_else(
            || line.start + line.text.len() - line.text.trim_start().len(),
            ByteRange::start,
        );
        if !line.text.trim().is_empty() {
            self.query = Some(range(start, line.start + line.text.trim_end().len()));
        }
        if self.group_start.is_none() && ends_with_semicolon(line.text) {
            self.flush_query()?;
        }
        Ok(())
    }

    fn flush_query(&mut self) -> Result<(), Issue> {
        self.buffering = false;
        let Some(query) = self.query.take() else {
            return Ok(());
        };
        let ranges = match self
            .direction
            .expect("only direction SQL enters the buffer")
        {
            MigrationDirection::Forward => &mut self.forward,
            MigrationDirection::Down => &mut self.down,
        };
        if ranges.len() == MAX_QUERIES {
            return Err(Issue::limit(
                query,
                "Goose migration direction exceeds the Guard query limit",
            ));
        }
        ranges.push(query);
        Ok(())
    }
}

fn ends_with_semicolon(line: &str) -> bool {
    line.split_whitespace()
        .take_while(|word| !word.starts_with("--"))
        .last()
        .is_some_and(|word| word.ends_with(';'))
}

fn range(start: usize, end: usize) -> ByteRange {
    ByteRange::new(start, end).expect("Goose source ranges are in input order")
}
