use std::collections::BTreeSet;

use dmg_core::{
    digest::ContentDigest,
    migration::MigrationAdapter,
    risk::{
        Confidence, DatabaseObject, EvidenceKind, Finding, FindingEvidence, ObjectKind, RuleId,
        Severity,
    },
    source::{RepoPath, SourceSpan},
};
use dmg_postgres::{ParsedMigration, ParsedPlan};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Rule {
    Pg001,
    Pg002,
    Pg003,
    Pg004,
    Pg005,
    Pg006,
    Pg007,
}

impl Rule {
    pub const ALL: [Self; 7] = [
        Self::Pg001,
        Self::Pg002,
        Self::Pg003,
        Self::Pg004,
        Self::Pg005,
        Self::Pg006,
        Self::Pg007,
    ];

    pub fn id(self) -> RuleId {
        self.code().parse().expect("built-in rule IDs are valid")
    }

    fn code(self) -> &'static str {
        match self {
            Self::Pg001 => "PG001",
            Self::Pg002 => "PG002",
            Self::Pg003 => "PG003",
            Self::Pg004 => "PG004",
            Self::Pg005 => "PG005",
            Self::Pg006 => "PG006",
            Self::Pg007 => "PG007",
        }
    }

    fn documentation(self) -> &'static str {
        match self {
            Self::Pg001 | Self::Pg002 | Self::Pg003 | Self::Pg004 => {
                "https://www.postgresql.org/docs/18/sql-altertable.html"
            }
            Self::Pg005 => "https://www.postgresql.org/docs/18/sql-createindex.html",
            Self::Pg006 => "https://www.postgresql.org/docs/18/dml.html",
            Self::Pg007 => "https://www.postgresql.org/docs/18/explicit-locking.html",
        }
    }
}

pub(crate) struct Selection<'a>(Option<BTreeSet<&'a RepoPath>>);

impl<'a> Selection<'a> {
    pub(crate) fn new(plan: &'a ParsedPlan) -> Self {
        Self(
            plan.selected_forwards
                .as_ref()
                .map(|paths| paths.iter().collect()),
        )
    }

    pub(crate) fn contains(&self, path: &RepoPath) -> bool {
        self.0.as_ref().is_none_or(|paths| paths.contains(path))
    }
}

pub(crate) struct FindingSpec<'a> {
    pub rule: Rule,
    pub object: Option<DatabaseObject>,
    pub severity: Severity,
    pub confidence: Confidence,
    pub explanation: &'static str,
    pub impact: &'static str,
    pub mitigation: &'static str,
    pub evidence: &'a str,
}

pub(crate) fn record<'findings>(
    findings: &'findings mut Vec<Finding>,
    migration: &ParsedMigration,
    span: &SourceSpan,
    spec: FindingSpec<'_>,
) -> &'findings mut Finding {
    findings.push(Finding {
        span: span.clone(),
        rule_id: spec.rule.id(),
        migration: migration.identity.clone(),
        fingerprint: fingerprint(spec.rule, migration, span, spec.object.as_ref()),
        object: spec.object,
        severity: spec.severity,
        confidence: spec.confidence,
        explanation: spec.explanation.into(),
        impact: spec.impact.into(),
        mitigation: spec.mitigation.into(),
        documentation: spec.rule.documentation().into(),
        evidence: vec![FindingEvidence {
            kind: EvidenceKind::StaticPrediction,
            detail: spec.evidence.into(),
            artifact_digest: None,
        }],
    });
    findings.last_mut().expect("a finding was just added")
}

fn fingerprint(
    rule: Rule,
    migration: &ParsedMigration,
    span: &SourceSpan,
    object: Option<&DatabaseObject>,
) -> ContentDigest {
    // Encode fields with their lengths so decoded names cannot create ambiguous tuples.
    let mut bytes = b"dmg-finding-v1\0".to_vec();
    let mut field = |value: &str| {
        bytes.extend_from_slice(&(value.len() as u64).to_le_bytes());
        bytes.extend_from_slice(value.as_bytes());
    };
    field(rule.code());
    field(match migration.identity.adapter {
        MigrationAdapter::Plain => "plain",
        MigrationAdapter::Sqlx => "sqlx",
    });
    field(&migration.identity.version.to_string());
    field(span.path.as_str());
    field(&span.bytes.start().to_string());
    field(&span.bytes.end().to_string());
    if let Some(object) = object {
        field(match object.kind {
            ObjectKind::Schema => "schema",
            ObjectKind::Relation => "relation",
            ObjectKind::Column => "column",
            ObjectKind::Index => "index",
            ObjectKind::Constraint => "constraint",
            ObjectKind::Type => "type",
        });
        field(object.schema.as_deref().unwrap_or(""));
        field(&object.name);
        field(object.member.as_deref().unwrap_or(""));
    }
    ContentDigest::of(&bytes)
}
