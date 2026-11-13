use std::collections::BTreeSet;

use dmg_core::{
    coverage::{Coverage, CoverageGap, CoverageGapKind, CoverageStatus, DiagnosticScope},
    digest::ContentDigest,
    history::forward_inventory_digest,
    inspection::{InspectionScope, TargetId},
    migration::{
        AppliedStatus, IntegrityBasis, IntegrityStatus, MigrationAdapter, MigrationDirection,
        MigrationFile, MigrationPlan, StatementDelivery, TransactionWrapper,
    },
    rehearsal::{
        REHEARSAL_PROFILE_VERSION, RehearsalBindingV1, RehearsalGroupV1, RehearsalMigrationState,
        RehearsalMigrationV1, RehearsalMode, RehearsalScriptV1, RehearsalStatementState,
        RehearsalStatementV1, SeedKind, valid_rehearsal_search_path,
    },
    risk::ObjectKind,
    source::{ByteRange, RepoPath},
    target::PostgresTarget,
};
use dmg_discovery::Discovery;
use dmg_postgres::{
    AlterChange, ConstraintKind, Operation, ParsedMigration, RelationName, Statement, Transaction,
    parse_migration, parse_plan,
};
use thiserror::Error;

use crate::PostgresImage;

pub const MAX_REHEARSAL_SETUP_BYTES: usize = 256 * 1024;
const MAX_HEAD_BYTES: usize = 32 * 1024 * 1024;
const MAX_MIGRATIONS: usize = 1024;
const MAX_EXECUTED_STATEMENTS: usize = 8192;
const MAX_POTENTIAL_STATEMENTS: usize = 32_768;

pub struct RehearsalContext {
    pub repository_id: TargetId,
    pub target: PostgresTarget,
    pub target_id: TargetId,
    /// Exact schema names provisioned by the disposable worker.
    pub scope: InspectionScope,
    /// Search order for unqualified names, matching the reviewed project.
    pub search_path: Vec<String>,
    pub project_config_digest: ContentDigest,
    pub policy_digest: ContentDigest,
}

pub enum RehearsalInput<'a> {
    EmptyDatabase,
    /// The caller supplies a trusted schema-only script representing the exact Git base.
    SchemaOnly {
        baseline_sql: &'a [u8],
    },
    /// Load the exact base schema and synthetic or sanitized literal INSERTs before rehearsal.
    Seeded {
        baseline_sql: &'a [u8],
        seed_sql: &'a [u8],
        seed_kind: SeedKind,
    },
}

pub struct PreparedRehearsal<'a> {
    pub(super) binding: RehearsalBindingV1,
    #[cfg_attr(
        not(all(target_os = "linux", target_arch = "x86_64")),
        allow(dead_code)
    )]
    pub(super) baseline: Option<PreparedScript<'a>>,
    #[cfg_attr(
        not(all(target_os = "linux", target_arch = "x86_64")),
        allow(dead_code)
    )]
    pub(super) seed: Option<PreparedScript<'a>>,
    pub(super) migrations: Vec<PlannedMigration<'a>>,
}

impl PreparedRehearsal<'_> {
    pub fn binding(&self) -> &RehearsalBindingV1 {
        &self.binding
    }

    /// Build the ordered migration and statement records expected from this plan.
    pub fn expected_migrations(&self) -> Vec<RehearsalMigrationV1> {
        self.migrations
            .iter()
            .map(|migration| migration.observation.clone())
            .collect()
    }
}

#[derive(Debug, Error)]
pub enum RehearsalPlanError {
    #[error("the worker image does not match the declared PostgreSQL major")]
    TargetMismatch,
    #[error("the rehearsal requires an explicit search path within its provisioned schema scope")]
    InvalidSearchPath,
    #[error("the rehearsal plan has incomplete coverage")]
    Incomplete(Coverage),
    #[error("could not bind the rehearsal plan to the forward inventory")]
    Binding,
}

impl RehearsalPlanError {
    pub fn coverage(&self) -> Option<&Coverage> {
        match self {
            Self::Incomplete(coverage) => Some(coverage),
            _ => None,
        }
    }
}

#[derive(Clone, Copy)]
#[cfg_attr(
    not(all(target_os = "linux", target_arch = "x86_64")),
    allow(dead_code)
)]
pub(super) struct PreparedScript<'a> {
    pub(super) source: &'a str,
    pub(super) statement_count: usize,
}

type PreparedInputs<'a> = (
    RehearsalMode,
    Option<PreparedScript<'a>>,
    Option<PreparedScript<'a>>,
);

pub(super) struct PlannedMigration<'a> {
    pub(super) observation: RehearsalMigrationV1,
    #[cfg_attr(
        not(all(target_os = "linux", target_arch = "x86_64")),
        allow(dead_code)
    )]
    pub(super) groups: Vec<PlannedGroup<'a>>,
}

#[cfg_attr(
    not(all(target_os = "linux", target_arch = "x86_64")),
    allow(dead_code)
)]
pub(super) struct PlannedGroup<'a> {
    pub(super) query: PlannedQuery<'a>,
    pub(super) effects: Vec<StatementEffect>,
}

#[cfg_attr(
    not(all(target_os = "linux", target_arch = "x86_64")),
    allow(dead_code)
)]
pub(super) enum PlannedQuery<'a> {
    Simple(&'a str),
    Extended(Vec<&'a str>),
}

#[cfg_attr(
    not(all(target_os = "linux", target_arch = "x86_64")),
    allow(dead_code)
)]
pub(super) enum StatementEffect {
    Transaction(Transaction),
    ConcurrentIndex,
    Other,
}

/// Prepare all inputs without opening the worker. The resulting plan borrows the exact
/// discovered migration and setup bytes until execution completes.
pub fn prepare_rehearsal<'a>(
    discovery: &'a Discovery,
    context: RehearsalContext,
    input: RehearsalInput<'a>,
    image: PostgresImage,
) -> Result<PreparedRehearsal<'a>, RehearsalPlanError> {
    if image.server_version_num() / 10_000 != u32::from(context.target.major()) {
        return Err(RehearsalPlanError::TargetMismatch);
    }
    if !valid_rehearsal_search_path(&context.scope, &context.search_path) {
        return Err(RehearsalPlanError::InvalidSearchPath);
    }

    let (mode, baseline, seed) = prepare_inputs(input, context.target, &context.scope)?;
    let migrations = plan_execution(discovery, context.target, &context.scope, &mode)?;
    let comparison = discovery
        .plan
        .integrity
        .as_ref()
        .ok_or_else(revision_required)?;
    let IntegrityBasis::Git {
        base_commit,
        head_commit,
    } = &comparison.basis
    else {
        return Err(revision_required());
    };
    let adapter = discovery.adapter();
    let forward_inventory_digest = forward_inventory_digest(adapter, &discovery.plan.inventory)
        .map_err(|_| RehearsalPlanError::Binding)?;
    let execution_plan_digest = digest_execution(&migrations)?;
    let binding = RehearsalBindingV1 {
        repository_id: context.repository_id,
        base_commit: base_commit.clone(),
        head_commit: head_commit.clone(),
        project_config_digest: context.project_config_digest,
        policy_digest: context.policy_digest,
        forward_inventory_digest,
        execution_plan_digest,
        adapter,
        profile_version: REHEARSAL_PROFILE_VERSION,
        target: context.target,
        target_id: context.target_id,
        server_version_num: image.server_version_num(),
        image_manifest_digest: image_digest(image.manifest_digest()),
        image_config_digest: image_digest(image.config_id()),
        scope: context.scope,
        search_path: context.search_path,
        mode,
    };

    Ok(PreparedRehearsal {
        binding,
        baseline,
        seed,
        migrations,
    })
}

/// Recompute ordered execution metadata from trusted Git discovery and mode.
/// Baseline and seed content digests belong to the separately verified binding.
pub fn expected_rehearsal_plan(
    discovery: &Discovery,
    target: PostgresTarget,
    scope: &InspectionScope,
    mode: &RehearsalMode,
) -> Result<(ContentDigest, Vec<RehearsalMigrationV1>), RehearsalPlanError> {
    let migrations = plan_execution(discovery, target, scope, mode)?;
    let digest = digest_execution(&migrations)?;
    Ok((
        digest,
        migrations
            .into_iter()
            .map(|migration| migration.observation)
            .collect(),
    ))
}

fn plan_execution<'a>(
    discovery: &'a Discovery,
    target: PostgresTarget,
    scope: &InspectionScope,
    mode: &RehearsalMode,
) -> Result<Vec<PlannedMigration<'a>>, RehearsalPlanError> {
    let plan = &discovery.plan;
    check_coverage(&plan.coverage)?;
    let Some((comparison, selected)) = plan.integrity.as_ref().zip(plan.selected_forwards.as_ref())
    else {
        return Err(revision_required());
    };
    let IntegrityBasis::Git {
        base_commit,
        head_commit,
    } = &comparison.basis
    else {
        return Err(revision_required());
    };
    if !discovery.head_complete() || !valid_commit_ids(base_commit, head_commit) {
        return Err(revision_required());
    }

    let forwards = plan
        .inventory
        .iter()
        .filter(|file| file.direction == MigrationDirection::Forward)
        .collect::<Vec<_>>();
    check_input_limits(discovery, &forwards)?;

    let pending = match mode {
        RehearsalMode::EmptyDatabase => forwards.clone(),
        _ => select_pending(plan, &forwards, selected)?,
    };
    let pending = pending
        .into_iter()
        .map(|file| &file.path)
        .collect::<BTreeSet<_>>();
    let parsed = parse_plan(plan, target, |path| discovery.source_bytes(path));
    check_coverage(&parsed.coverage)?;
    let migrations = forwards
        .into_iter()
        .zip(parsed.migrations)
        .filter(|(file, _)| pending.contains(&file.path))
        .map(|(file, parsed)| {
            let source = discovery
                .source(&file.path)
                .ok_or(RehearsalPlanError::Binding)?;
            plan_migration(file, parsed, source, scope)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let statement_count = migrations
        .iter()
        .flat_map(|migration| &migration.observation.groups)
        .map(|group| group.statements.len())
        .sum::<usize>();
    if statement_count > MAX_EXECUTED_STATEMENTS {
        return Err(repository_gap(
            CoverageGapKind::InputLimitExceeded,
            "rehearsal statement count exceeds its limit",
        ));
    }
    Ok(migrations)
}

fn check_input_limits(
    discovery: &Discovery,
    forwards: &[&MigrationFile],
) -> Result<(), RehearsalPlanError> {
    let too_large = || {
        repository_gap(
            CoverageGapKind::InputLimitExceeded,
            "rehearsal inventory exceeds its parser budget",
        )
    };
    if forwards.len() > MAX_MIGRATIONS {
        return Err(too_large());
    }
    let mut bytes = 0usize;
    let mut potential_statements = 0usize;
    for file in forwards {
        let source = discovery.source_bytes(&file.path).ok_or_else(|| {
            repository_gap(
                CoverageGapKind::UnreadableInput,
                "forward source bytes are unavailable",
            )
        })?;
        bytes = bytes.saturating_add(source.len());
        // Preflight conservatively bounds native parsing and retained ASTs. A segment
        // can contain one statement without a trailing delimiter.
        potential_statements = potential_statements.saturating_add(
            file.source_segments
                .len()
                .saturating_add(source.iter().filter(|&&byte| byte == b';').count()),
        );
        if bytes > MAX_HEAD_BYTES || potential_statements > MAX_POTENTIAL_STATEMENTS {
            return Err(too_large());
        }
    }
    Ok(())
}

fn digest_execution(
    migrations: &[PlannedMigration<'_>],
) -> Result<ContentDigest, RehearsalPlanError> {
    let execution_plan = migrations
        .iter()
        .map(|migration| &migration.observation)
        .collect::<Vec<_>>();
    let mut binding_bytes = b"dmg-rehearsal-execution-v1\0".to_vec();
    serde_json::to_writer(&mut binding_bytes, &execution_plan)
        .map_err(|_| RehearsalPlanError::Binding)?;
    Ok(ContentDigest::of(&binding_bytes))
}

pub(super) fn image_digest(pin: &str) -> ContentDigest {
    pin.strip_prefix("sha256:")
        .expect("pinned image digest algorithm")
        .parse()
        .expect("pinned image digest")
}

fn valid_commit_ids(base: &str, head: &str) -> bool {
    matches!(base.len(), 40 | 64)
        && base.len() == head.len()
        && [base, head].iter().all(|commit| {
            commit
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        })
}

fn select_pending<'a>(
    plan: &MigrationPlan,
    forwards: &[&'a MigrationFile],
    selected: &[RepoPath],
) -> Result<Vec<&'a MigrationFile>, RehearsalPlanError> {
    let comparison = plan.integrity.as_ref().ok_or_else(revision_required)?;
    let mut pending = Vec::new();
    let mut prior_version = None;
    for change in comparison
        .changes
        .iter()
        .filter(|change| change.direction == MigrationDirection::Forward)
    {
        match change.status {
            IntegrityStatus::New => {
                let after = change.after.as_ref().ok_or(RehearsalPlanError::Binding)?;
                pending.push((&change.identity, after));
            }
            IntegrityStatus::Unchanged if change.before == change.after => {
                if !change.identity.is_repeatable() {
                    prior_version = prior_version.max(Some(&change.identity));
                }
            }
            _ => {
                return Err(repository_gap(
                    CoverageGapKind::MissingAppliedHistory,
                    "a Git-base rehearsal requires unchanged base forwards and unambiguous new forwards",
                ));
            }
        }
    }
    let mut paths = pending.iter();
    let mut files = Vec::with_capacity(pending.len());
    for file in forwards {
        if let Some((identity, after)) = paths.as_slice().first()
            && *identity == &file.identity
        {
            if after.path != file.path
                || after.digest != file.digest
                || (!file.identity.is_repeatable()
                    && prior_version.is_some_and(|prior| &file.identity <= prior))
            {
                return Err(repository_gap(
                    CoverageGapKind::MissingAppliedHistory,
                    "a new forward cannot be placed before an established base version",
                ));
            }
            files.push(*file);
            paths.next();
        }
    }
    if paths.len() != 0 || !files.iter().map(|file| &file.path).eq(selected.iter()) {
        return Err(repository_gap(
            CoverageGapKind::MissingAppliedHistory,
            "Git selection does not match the ordered pending forwards",
        ));
    }
    if let Some(history) = &plan.applied_history {
        let mut seen = BTreeSet::new();
        for applied in &history.migrations {
            let expected = if files.iter().any(|file| file.identity == applied.identity) {
                AppliedStatus::Pending
            } else {
                AppliedStatus::Applied
            };
            if !seen.insert(&applied.identity)
                || applied.status != expected
                || !forwards
                    .iter()
                    .any(|file| file.identity == applied.identity)
            {
                return Err(repository_gap(
                    CoverageGapKind::MissingAppliedHistory,
                    "applied history does not match the trusted Git base",
                ));
            }
        }
        if history.migrations.len() != forwards.len() {
            return Err(repository_gap(
                CoverageGapKind::MissingAppliedHistory,
                "applied history does not cover all forward identities",
            ));
        }
    }
    Ok(files)
}

fn prepare_inputs<'a>(
    input: RehearsalInput<'a>,
    target: PostgresTarget,
    scope: &InspectionScope,
) -> Result<PreparedInputs<'a>, RehearsalPlanError> {
    let prepared = match input {
        RehearsalInput::EmptyDatabase => (RehearsalMode::EmptyDatabase, None, None),
        RehearsalInput::SchemaOnly { baseline_sql } => {
            let (baseline, source) = prepare_script(baseline_sql, target, scope, false)?;
            (RehearsalMode::SchemaOnly { baseline }, Some(source), None)
        }
        RehearsalInput::Seeded {
            baseline_sql,
            seed_sql,
            seed_kind,
        } => {
            let (baseline, baseline_source) = prepare_script(baseline_sql, target, scope, false)?;
            let (seed, seed_source) = prepare_script(seed_sql, target, scope, true)?;
            (
                RehearsalMode::Seeded {
                    baseline,
                    seed,
                    seed_kind,
                },
                Some(baseline_source),
                Some(seed_source),
            )
        }
    };
    Ok(prepared)
}

fn prepare_script<'a>(
    bytes: &'a [u8],
    target: PostgresTarget,
    scope: &InspectionScope,
    seed: bool,
) -> Result<(RehearsalScriptV1, PreparedScript<'a>), RehearsalPlanError> {
    let error = |kind| {
        repository_gap(
            kind,
            if seed {
                "seed must be bounded UTF-8 SQL containing only supported INSERT statements"
            } else {
                "baseline must be bounded UTF-8 SQL containing only supported schema statements"
            },
        )
    };
    if bytes.is_empty() || bytes.len() > MAX_REHEARSAL_SETUP_BYTES {
        return Err(error(CoverageGapKind::InputLimitExceeded));
    }
    let source =
        std::str::from_utf8(bytes).map_err(|_| error(CoverageGapKind::UnsupportedInput))?;
    let digest = ContentDigest::of(bytes);
    let file = MigrationFile {
        identity: dmg_core::migration::MigrationIdentity::numeric(MigrationAdapter::Plain, 1),
        path: RepoPath::new(if seed { "seed.sql" } else { "baseline.sql" })
            .expect("fixed relative path"),
        direction: MigrationDirection::Forward,
        digest,
        source_segments: vec![ByteRange::new(0, bytes.len()).expect("bounded source")],
        execution: dmg_core::migration::ExecutionMode::PSQL_FILE_TRANSACTION,
    };
    let parsed = parse_migration(&file, bytes, target);
    check_coverage(&parsed.coverage)?;
    if parsed.statements.is_empty() {
        return Err(error(CoverageGapKind::UnsupportedInput));
    }
    for statement in &parsed.statements {
        if !seed && is_concurrent_index(&statement.operation) {
            return Err(statement_gap(
                statement,
                CoverageGapKind::UnresolvedTransactionMode,
                "baseline schema setup runs atomically and cannot build or drop an index concurrently",
            ));
        }
        match &statement.operation {
            Operation::Insert(_) if seed => check_worker_scope(statement, scope)?,
            Operation::CreateSchema { .. }
            | Operation::CreateTable(_)
            | Operation::AlterTable(_)
            | Operation::Rename(_)
            | Operation::Drop(_)
            | Operation::CreateIndex(_)
                if !seed =>
            {
                check_worker_scope(statement, scope)?;
            }
            _ => return Err(error(CoverageGapKind::UnsupportedSql)),
        }
    }
    Ok((
        RehearsalScriptV1 {
            digest,
            bytes: bytes.len() as u64,
        },
        PreparedScript {
            source,
            statement_count: parsed.statements.len(),
        },
    ))
}

fn plan_migration<'a>(
    file: &MigrationFile,
    parsed: ParsedMigration,
    source: &'a str,
    scope: &InspectionScope,
) -> Result<PlannedMigration<'a>, RehearsalPlanError> {
    for statement in &parsed.statements {
        check_worker_scope(statement, scope)?;
        if matches!(
            &statement.operation,
            Operation::Transaction(
                Transaction::Begin | Transaction::Commit { .. } | Transaction::Rollback { .. }
            )
        ) && (file.identity.adapter == MigrationAdapter::Sqlx
            || parsed.execution.transaction_wrapper == TransactionWrapper::File)
        {
            return Err(statement_gap(
                statement,
                CoverageGapKind::UnresolvedTransactionMode,
                "authored transaction commands would change the runner-owned transaction boundary",
            ));
        }
    }
    if matches!(
        parsed.execution.transaction_wrapper,
        TransactionWrapper::Flyway(_)
    ) {
        return Err(RehearsalPlanError::Binding);
    }
    let mut groups = Vec::new();
    let mut observations = Vec::new();
    let mut remaining = parsed.statements.as_slice();
    while let Some(first) = remaining.first() {
        let count = if first.delivery == StatementDelivery::PerStatement {
            1
        } else {
            remaining
                .iter()
                .take_while(|statement| {
                    statement.segment == first.segment && statement.delivery == first.delivery
                })
                .count()
        };
        let (statements, tail) = remaining.split_at(count);
        let get_source = |range: ByteRange| {
            source
                .get(range.as_range())
                .ok_or(RehearsalPlanError::Binding)
        };
        let query = match first.delivery {
            StatementDelivery::PerStatement => PlannedQuery::Simple(get_source(first.span.bytes)?),
            StatementDelivery::SingleQuery => {
                let range = file
                    .source_segments
                    .get(first.segment)
                    .ok_or(RehearsalPlanError::Binding)?;
                PlannedQuery::Simple(get_source(*range)?)
            }
            StatementDelivery::ExtendedBatch => PlannedQuery::Extended(
                statements
                    .iter()
                    .map(|stmt| get_source(stmt.span.bytes))
                    .collect::<Result<_, _>>()?,
            ),
        };
        let effects = statements
            .iter()
            .map(|statement| match &statement.operation {
                Operation::Transaction(effect) => StatementEffect::Transaction(effect.clone()),
                operation if is_concurrent_index(operation) => StatementEffect::ConcurrentIndex,
                _ => StatementEffect::Other,
            })
            .collect();
        observations.push(RehearsalGroupV1 {
            delivery: first.delivery,
            elapsed_millis: None,
            statements: statements
                .iter()
                .map(|statement| RehearsalStatementV1 {
                    span: statement.span.clone(),
                    state: RehearsalStatementState::Skipped,
                    durability: None,
                    elapsed_millis: None,
                })
                .collect(),
        });
        groups.push(PlannedGroup { query, effects });
        remaining = tail;
    }

    let observation = RehearsalMigrationV1 {
        identity: file.identity.clone(),
        path: file.path.clone(),
        digest: file.digest,
        execution: parsed.execution,
        state: RehearsalMigrationState::Skipped,
        elapsed_millis: None,
        groups: observations,
    };
    Ok(PlannedMigration {
        observation,
        groups,
    })
}

fn is_concurrent_index(operation: &Operation) -> bool {
    match operation {
        Operation::CreateIndex(index) => index.concurrently,
        Operation::Drop(drop) => drop.kind == ObjectKind::Index && drop.concurrently,
        _ => false,
    }
}

fn check_worker_scope(
    statement: &Statement,
    scope: &InspectionScope,
) -> Result<(), RehearsalPlanError> {
    let contains = |schema: &str| {
        scope
            .schemas()
            .binary_search_by(|name| name.as_str().cmp(schema))
            .is_ok()
    };
    let allowed =
        |name: &RelationName| name.catalog.is_none() && name.schema.as_deref().is_none_or(contains);
    let constraint_in_scope = |constraint: &dmg_postgres::Constraint| match &constraint.kind {
        ConstraintKind::ForeignKey {
            referenced_table, ..
        } => allowed(referenced_table),
        _ => true,
    };
    let in_scope = match &statement.operation {
        Operation::CreateSchema { .. } => {
            return Err(statement_gap(
                statement,
                CoverageGapKind::UnsupportedSql,
                "CREATE SCHEMA requires database privileges unavailable to the worker",
            ));
        }
        Operation::CreateTable(table) => {
            allowed(&table.relation)
                && table.partition_of.as_ref().is_none_or(allowed)
                && table.constraints.iter().all(constraint_in_scope)
                && table
                    .columns
                    .iter()
                    .flat_map(|column| &column.constraints)
                    .all(constraint_in_scope)
        }
        Operation::AlterTable(table) => {
            allowed(&table.relation)
                && table.actions.iter().all(|action| match &action.change {
                    AlterChange::AddConstraint(constraint) => constraint_in_scope(constraint),
                    AlterChange::AddColumn { column, .. } => {
                        column.constraints.iter().all(constraint_in_scope)
                    }
                    _ => true,
                })
        }
        Operation::Rename(rename) => allowed(&rename.object),
        Operation::Drop(drop) if drop.kind == ObjectKind::Schema => {
            drop.objects.iter().all(|name| contains(&name.name))
        }
        Operation::Drop(drop) => drop.objects.iter().all(allowed),
        Operation::Truncate(truncate) => truncate.relations.iter().all(allowed),
        Operation::CreateIndex(index) => allowed(&index.relation),
        Operation::Update(change) | Operation::Delete(change) => allowed(&change.relation),
        Operation::Insert(relation) => allowed(relation),
        _ => true,
    };
    if !in_scope {
        return Err(statement_gap(
            statement,
            CoverageGapKind::UnresolvedDependency,
            "statement references an explicitly qualified object outside the provisioned worker scope",
        ));
    }
    Ok(())
}

fn check_coverage(coverage: &Coverage) -> Result<(), RehearsalPlanError> {
    if coverage.status() == CoverageStatus::Complete {
        Ok(())
    } else {
        Err(RehearsalPlanError::Incomplete(coverage.clone()))
    }
}

fn revision_required() -> RehearsalPlanError {
    repository_gap(
        CoverageGapKind::MissingIntegrityEvidence,
        "rehearsal requires complete discovery from explicit Git base and head commits",
    )
}

fn repository_gap(kind: CoverageGapKind, message: &str) -> RehearsalPlanError {
    gap(DiagnosticScope::Repository, kind, message)
}

fn statement_gap(
    statement: &Statement,
    kind: CoverageGapKind,
    message: &str,
) -> RehearsalPlanError {
    gap(
        DiagnosticScope::Span {
            span: statement.span.clone(),
        },
        kind,
        message,
    )
}

fn gap(scope: DiagnosticScope, kind: CoverageGapKind, message: &str) -> RehearsalPlanError {
    RehearsalPlanError::Incomplete(Coverage::from_gaps(vec![CoverageGap {
        scope,
        kind,
        message: message.into(),
        related_paths: Vec::new(),
    }]))
}
