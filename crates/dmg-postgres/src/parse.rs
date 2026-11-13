use std::{ops::Range, sync::Mutex};

use dmg_core::{
    coverage::{Coverage, CoverageGap, CoverageGapKind, DiagnosticScope},
    digest::ContentDigest,
    migration::{
        MigrationAdapter, MigrationDirection, MigrationFile, MigrationPlan, StatementDelivery,
    },
    source::{ByteRange, RepoPath, SourceMap, SourceSpan},
    target::PostgresTarget,
};
use pg_query::protobuf::{RawStmt, Token};

use crate::{
    ParsedMigration, ParsedPlan, Statement,
    normalize::{Normalizer, node},
    version::VersionContext,
};

const MAX_FILE_BYTES: usize = 8 * 1024 * 1024;
const MAX_SEGMENT_BYTES: usize = 256 * 1024;
const MAX_TOKENS: usize = 32_768;
const MAX_DELIMITER_DEPTH: usize = 64;
const MAX_STATEMENTS: usize = 2_048;
const MAX_SEGMENTS: usize = 1_024;

// The pinned native parser temporarily redirects process-wide stderr.
static NATIVE_PARSE: Mutex<()> = Mutex::new(());

fn native_parse(sql: &str) -> pg_query::Result<pg_query::ParseResult> {
    let _guard = NATIVE_PARSE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    pg_query::parse(sql, pg_query::ParserOptions::default())
}

fn scan(sql: &str) -> pg_query::Result<Vec<pg_query::protobuf::ScanToken>> {
    let _guard = NATIVE_PARSE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    pg_query::scan(sql).map(|scan| scan.tokens)
}

pub fn parse_migration(
    file: &MigrationFile,
    bytes: &[u8],
    target: PostgresTarget,
) -> ParsedMigration {
    parse_file(file, Some(bytes), target, &mut VersionContext::default())
}

/// Analyze every head forward in discovery order. Keep the changed-forward selection
/// independent from the source used to establish earlier schema facts.
pub fn parse_plan<'source>(
    plan: &MigrationPlan,
    target: PostgresTarget,
    mut source: impl FnMut(&RepoPath) -> Option<&'source [u8]>,
) -> ParsedPlan {
    let mut versions = VersionContext::default();
    let mut gaps = plan.coverage.gaps().to_vec();
    let mut migrations = Vec::new();
    for file in &plan.inventory {
        if file.direction == MigrationDirection::Forward {
            let parsed = parse_file(file, source(&file.path), target, &mut versions);
            gaps.extend_from_slice(parsed.coverage.gaps());
            migrations.push(parsed);
        }
    }
    ParsedPlan {
        migrations,
        selected_forwards: plan.selected_forwards.clone(),
        coverage: Coverage::from_gaps(gaps),
    }
}

fn parse_file(
    file: &MigrationFile,
    bytes: Option<&[u8]>,
    target: PostgresTarget,
    versions: &mut VersionContext,
) -> ParsedMigration {
    let mut parsed = ParsedMigration {
        identity: file.identity.clone(),
        path: file.path.clone(),
        execution: file.execution,
        statements: Vec::new(),
        coverage: Coverage::Complete,
    };
    if file.direction != MigrationDirection::Forward {
        return parsed;
    }
    let text = match source_text(file, bytes) {
        Ok(text) => text,
        Err((kind, message)) => {
            parsed.coverage = Coverage::from_gaps(vec![path_gap(&file.path, kind, message)]);
            versions.clear();
            return parsed;
        }
    };
    let map = SourceMap::new(file.path.clone(), text);
    let mut gaps = Vec::new();
    if file.source_segments.is_empty() {
        gaps.push(path_gap(
            &file.path,
            CoverageGapKind::UnsupportedInput,
            "migration has no declared forward source segments",
        ));
    }
    if file.source_segments.len() > MAX_SEGMENTS {
        gaps.push(path_gap(
            &file.path,
            CoverageGapKind::InputLimitExceeded,
            "migration exceeds the parser segment count limit",
        ));
    }
    let mut previous_end = 0;
    for (segment, range) in file
        .source_segments
        .iter()
        .copied()
        .take(MAX_SEGMENTS)
        .enumerate()
    {
        if range.start() < previous_end || text.get(range.as_range()).is_none() {
            gaps.push(path_gap(
                &file.path,
                CoverageGapKind::UnsupportedInput,
                "migration source segments must be ordered, disjoint UTF-8 ranges",
            ));
            break;
        }
        let mut segment_parser = SegmentParser {
            path: &file.path,
            map: &map,
            base: range.start(),
            number: segment,
            delivery: file.execution.statement_delivery,
            sql: &text[range.as_range()],
            gaps: &mut gaps,
            statements: &mut parsed.statements,
        };
        segment_parser.parse();
        previous_end = range.end();
    }
    if file.identity.adapter == MigrationAdapter::Supabase {
        crate::supabase::resolve_execution(&mut parsed, text, &mut gaps);
    }
    let syntax_complete = gaps.is_empty();
    versions.check(target, &parsed, &mut gaps, syntax_complete);
    parsed.coverage = Coverage::from_gaps(gaps);
    parsed
}

fn source_text<'a>(
    file: &MigrationFile,
    bytes: Option<&'a [u8]>,
) -> Result<&'a str, (CoverageGapKind, &'static str)> {
    let Some(bytes) = bytes else {
        return Err((
            CoverageGapKind::UnreadableInput,
            "original migration bytes are unavailable",
        ));
    };
    if bytes.len() > MAX_FILE_BYTES {
        return Err((
            CoverageGapKind::InputLimitExceeded,
            "migration exceeds the parser file size limit",
        ));
    }
    if ContentDigest::of(bytes) != file.digest {
        return Err((
            CoverageGapKind::UnreadableInput,
            "migration bytes do not match the discovered digest",
        ));
    }
    std::str::from_utf8(bytes).map_err(|_| {
        (
            CoverageGapKind::UnsupportedInput,
            "migration is not valid UTF-8",
        )
    })
}

pub(crate) fn path_gap(path: &RepoPath, kind: CoverageGapKind, message: &str) -> CoverageGap {
    CoverageGap {
        scope: DiagnosticScope::Path { path: path.clone() },
        kind,
        message: message.into(),
        related_paths: Vec::new(),
    }
}

pub(crate) fn span_gap(span: SourceSpan, kind: CoverageGapKind, message: &str) -> CoverageGap {
    CoverageGap {
        scope: DiagnosticScope::Span { span },
        kind,
        message: message.into(),
        related_paths: Vec::new(),
    }
}

pub(crate) fn span(map: &SourceMap<'_>, base: usize, range: Range<usize>) -> Option<SourceSpan> {
    let range =
        ByteRange::new(base.checked_add(range.start)?, base.checked_add(range.end)?).ok()?;
    map.span(range).ok()
}

pub(crate) struct SqlToken<'sql> {
    pub text: &'sql str,
    pub range: Range<usize>,
}

pub(crate) fn token_range(tokens: &[SqlToken<'_>]) -> Option<Range<usize>> {
    Some(tokens.first()?.range.start..tokens.last()?.range.end)
}

pub(crate) fn delimiter_depth(depth: usize, token: &str) -> usize {
    match token {
        "(" | "[" => depth + 1,
        ")" | "]" => depth.saturating_sub(1),
        _ => depth,
    }
}

struct SegmentParser<'a, 'source> {
    path: &'a RepoPath,
    map: &'a SourceMap<'source>,
    base: usize,
    number: usize,
    delivery: StatementDelivery,
    sql: &'source str,
    gaps: &'a mut Vec<CoverageGap>,
    statements: &'a mut Vec<Statement>,
}

impl<'source> SegmentParser<'_, 'source> {
    fn gap(&mut self, range: Range<usize>, kind: CoverageGapKind, message: &str) {
        self.gaps.push(span(self.map, self.base, range).map_or_else(
            || path_gap(self.path, kind, message),
            |location| span_gap(location, kind, message),
        ));
    }

    fn segment_gap(&mut self, kind: CoverageGapKind, message: &str) {
        self.gap(0..self.sql.len(), kind, message);
    }

    fn parse(&mut self) {
        if self.sql.len() > MAX_SEGMENT_BYTES {
            self.segment_gap(
                CoverageGapKind::InputLimitExceeded,
                "migration segment exceeds the parser byte limit",
            );
            return;
        }
        if let Some(offset) = self.sql.find('\0') {
            self.gap(
                offset..offset + 1,
                CoverageGapKind::UnsupportedInput,
                "migration contains an embedded NUL",
            );
            return;
        }
        if self.sql.trim_start().starts_with('\\') {
            let start = self.sql.len() - self.sql.trim_start().len();
            let end = self.sql[start..]
                .find(['\r', '\n'])
                .map_or(self.sql.len(), |len| start + len);
            self.gap(
                start..end,
                CoverageGapKind::UnsupportedSql,
                "client directives require runner interpretation",
            );
            return;
        }
        let Some(tokens) = self.scan() else {
            return;
        };
        if tokens.is_empty() {
            return;
        }

        match native_parse(self.sql) {
            Ok(result) => self.normalize_batch(&result.protobuf.stmts, &tokens, self.sql.len()),
            Err(_) => self.recover(&tokens),
        }
    }

    fn scan(&mut self) -> Option<Vec<SqlToken<'source>>> {
        let scanned = match scan(self.sql) {
            Ok(scanned) => scanned,
            Err(_) => {
                self.segment_gap(
                    CoverageGapKind::ParseError,
                    "PostgreSQL could not tokenize the migration segment",
                );
                return None;
            }
        };
        if scanned.len() > MAX_TOKENS {
            self.segment_gap(
                CoverageGapKind::InputLimitExceeded,
                "migration segment exceeds the parser token limit",
            );
            return None;
        }
        let mut tokens: Vec<SqlToken<'source>> = Vec::with_capacity(scanned.len());
        let mut previous_end = 0;
        let mut depth = 0;
        let mut separators = 0;
        let mut previous_separator = None;
        for raw in scanned {
            let Some(range) = usize::try_from(raw.start)
                .ok()
                .zip(usize::try_from(raw.end).ok())
                .map(|(start, end)| start..end)
                .filter(|range| range.start >= previous_end && range.start < range.end)
            else {
                self.segment_gap(
                    CoverageGapKind::ParseError,
                    "PostgreSQL returned inconsistent token offsets",
                );
                return None;
            };
            let Some(text) = self.sql.get(range.clone()) else {
                self.segment_gap(
                    CoverageGapKind::ParseError,
                    "PostgreSQL token offsets fall outside UTF-8 boundaries",
                );
                return None;
            };
            previous_end = range.end;
            if matches!(raw.token(), Token::SqlComment | Token::CComment) {
                continue;
            }
            let control = if text == "\\" {
                Some((
                    range.clone(),
                    "client directives require runner interpretation",
                ))
            } else if text == "{"
                && tokens.last().is_some_and(|prev| {
                    matches!(prev.text, "{" | "$" | "%") && prev.range.end == range.start
                })
            {
                let start = tokens.last().expect("checked previous token").range.start;
                Some((
                    start..range.end,
                    "unresolved template requires runner interpretation",
                ))
            } else {
                None
            };
            if let Some((location, message)) = control {
                self.gap(location, CoverageGapKind::UnsupportedSql, message);
                self.recover_before_control(&tokens, previous_separator);
                return None;
            }
            if depth == 0 && text == ";" {
                separators += 1;
                previous_separator = Some(tokens.len());
            }
            depth = delimiter_depth(depth, text);
            if depth > MAX_DELIMITER_DEPTH || separators > MAX_STATEMENTS {
                self.gap(
                    range,
                    CoverageGapKind::InputLimitExceeded,
                    "migration segment exceeds the parser depth or statement limit",
                );
                return None;
            }
            tokens.push(SqlToken { text, range });
        }
        Some(tokens)
    }

    fn recover_before_control(&mut self, tokens: &[SqlToken<'_>], end: Option<usize>) {
        let Some(end) = end else { return };
        let prefix = &tokens[..end];
        let Some(range) = token_range(prefix) else {
            return;
        };
        // Parse the completed prefix as a whole so no fragment of a control block
        // becomes an independent operation.
        if let Ok(result) = native_parse(&self.sql[..range.end]) {
            self.normalize_batch(&result.protobuf.stmts, prefix, range.end);
        } else {
            self.recover(prefix);
        }
    }

    fn normalize_batch(&mut self, raw: &[RawStmt], tokens: &[SqlToken<'_>], end: usize) {
        let mut next = 0;
        for statement in raw {
            let Some((start, end)) = self.native_range(statement, end) else {
                self.segment_gap(
                    CoverageGapKind::ParseError,
                    "could not locate PostgreSQL statements in the source segment",
                );
                return;
            };
            while tokens
                .get(next)
                .is_some_and(|token| token.range.end <= start && token.text == ";")
            {
                next += 1;
            }
            let first = next;
            while tokens.get(next).is_some_and(|token| token.range.end <= end) {
                next += 1;
            }
            if first == next || tokens[first].range.start < start {
                self.segment_gap(
                    CoverageGapKind::ParseError,
                    "could not align a PostgreSQL statement with its source tokens",
                );
                return;
            }
            self.statement(statement, &tokens[first..next], 0);
        }
        if tokens[next..].iter().any(|token| token.text != ";") {
            self.segment_gap(
                CoverageGapKind::ParseError,
                "PostgreSQL statement ranges leave unaligned source tokens",
            );
        }
    }

    fn native_range(&self, statement: &RawStmt, segment_end: usize) -> Option<(usize, usize)> {
        let start = usize::try_from(statement.stmt_location).ok()?;
        let len = usize::try_from(statement.stmt_len).ok()?;
        let end = if len == 0 {
            segment_end
        } else {
            start.checked_add(len)?
        };
        self.sql.get(start..end).map(|_| (start, end))
    }

    fn recover(&mut self, tokens: &[SqlToken<'_>]) {
        let Some(groups) = recoverable_groups(tokens) else {
            self.segment_gap(
                CoverageGapKind::ParseError,
                "PostgreSQL rejected the segment and statement boundaries are uncertain",
            );
            return;
        };
        let before = self.gaps.len();
        for group in groups {
            let Some(range) = token_range(group) else {
                continue;
            };
            let start = range.start;
            match native_parse(&self.sql[range.clone()]) {
                Ok(result) if result.protobuf.stmts.len() == 1 => {
                    self.statement(&result.protobuf.stmts[0], group, start);
                }
                _ => self.gap(
                    range,
                    CoverageGapKind::ParseError,
                    "PostgreSQL rejected this statement",
                ),
            }
        }
        if !self.gaps[before..]
            .iter()
            .any(|gap| gap.kind == CoverageGapKind::ParseError)
        {
            self.segment_gap(
                CoverageGapKind::ParseError,
                "PostgreSQL rejected the original migration segment",
            );
        }
    }

    fn statement(&mut self, raw: &RawStmt, tokens: &[SqlToken<'_>], origin: usize) {
        let Some(location) = token_range(tokens).and_then(|range| span(self.map, self.base, range))
        else {
            self.segment_gap(
                CoverageGapKind::ParseError,
                "could not map PostgreSQL source offsets",
            );
            return;
        };
        let mut normalizer = Normalizer {
            map: self.map,
            base: self.base,
            gaps: self.gaps,
        };
        let operation = normalizer.statement(
            raw.stmt.as_deref().and_then(node),
            tokens,
            origin,
            &location,
        );
        self.statements.push(Statement {
            span: location,
            segment: self.number,
            delivery: self.delivery,
            operation,
        });
    }
}

fn recoverable_groups<'tokens, 'sql>(
    tokens: &'tokens [SqlToken<'sql>],
) -> Option<Vec<&'tokens [SqlToken<'sql>]>> {
    if tokens.windows(2).any(|pair| {
        pair[0].text.eq_ignore_ascii_case("begin") && pair[1].text.eq_ignore_ascii_case("atomic")
    }) {
        return None;
    }
    let mut groups = Vec::new();
    let mut delimiters = Vec::new();
    let mut first = 0;
    for (index, token) in tokens.iter().enumerate() {
        match token.text {
            "(" => delimiters.push(")"),
            "[" => delimiters.push("]"),
            ")" | "]" if delimiters.pop() != Some(token.text) => return None,
            ";" if delimiters.is_empty() => {
                if first < index {
                    groups.push(&tokens[first..index]);
                }
                first = index + 1;
            }
            _ => {}
        }
    }
    if !delimiters.is_empty() {
        return None;
    }
    if first < tokens.len() {
        groups.push(&tokens[first..]);
    }
    Some(groups)
}
