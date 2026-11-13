//! Native applied-history observations and binding to a local forward inventory.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha384};
use thiserror::Error;

use crate::{
    digest::ContentDigest,
    inspection::{InspectionGap, Observation},
    migration::{MigrationAdapter, MigrationDirection, MigrationFile, MigrationIdentity},
    schema::SchemaVersionV1,
    snapshot::QualifiedName,
    source::RepoPath,
};

pub const NATIVE_HISTORY_SCHEMA_VERSION: u32 = SchemaVersionV1::NUMBER;

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeHistoryMethod {
    RunnerChecksum,
    PresenceOnly,
    Unsupported,
}

impl NativeHistoryMethod {
    pub const fn for_adapter(adapter: MigrationAdapter) -> Self {
        match adapter {
            MigrationAdapter::Sqlx | MigrationAdapter::Flyway => Self::RunnerChecksum,
            MigrationAdapter::Goose | MigrationAdapter::Supabase => Self::PresenceOnly,
            MigrationAdapter::Plain => Self::Unsupported,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeHistoryCoverage {
    Complete,
    Incomplete,
    Unavailable,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct NativeHistoryRow {
    pub identity: MigrationIdentity,
    pub direction: MigrationDirection,
    /// Runner order, when present. Flyway uses installed rank and Goose can use row ID.
    pub rank: Option<u64>,
    pub state: NativeHistoryState,
    pub checksum: Option<NativeChecksum>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeHistoryState {
    Applied,
    Failed,
    Reverted,
    Superseded,
    Unknown,
}

/// An algorithm and digest from a native history row.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NativeChecksum {
    SqlxSha384 { digest: Sha384Digest },
    FlywayCrc32 { value: i32 },
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Sha384Digest([u8; 48]);

impl Sha384Digest {
    pub fn of(bytes: &[u8]) -> Self {
        Self(Sha384::digest(bytes).into())
    }

    pub const fn from_bytes(bytes: [u8; 48]) -> Self {
        Self(bytes)
    }
}

impl FromStr for Sha384Digest {
    type Err = InvalidSha384Digest;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != 96
            || !value
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        {
            return Err(InvalidSha384Digest);
        }
        let mut bytes = [0; 48];
        for (index, byte) in bytes.iter_mut().enumerate() {
            let start = index * 2;
            *byte = u8::from_str_radix(&value[start..start + 2], 16)
                .map_err(|_| InvalidSha384Digest)?;
        }
        Ok(Self(bytes))
    }
}

impl TryFrom<String> for Sha384Digest {
    type Error = InvalidSha384Digest;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<Sha384Digest> for String {
    fn from(digest: Sha384Digest) -> Self {
        digest.to_string()
    }
}

impl fmt::Display for Sha384Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
#[error("expected a SHA-384 digest of 96 lowercase hexadecimal digits")]
pub struct InvalidSha384Digest;

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "NativeHistoryFields")]
pub struct NativeHistoryEvidenceV1 {
    schema_version: SchemaVersionV1,
    observation: Observation,
    adapter: MigrationAdapter,
    source: Option<QualifiedName>,
    inventory_digest: ContentDigest,
    coverage: NativeHistoryCoverage,
    rows: Vec<NativeHistoryRow>,
    gaps: Vec<InspectionGap>,
}

impl NativeHistoryEvidenceV1 {
    pub fn new(
        observation: Observation,
        adapter: MigrationAdapter,
        source: Option<QualifiedName>,
        inventory_digest: ContentDigest,
        coverage: NativeHistoryCoverage,
        mut rows: Vec<NativeHistoryRow>,
        mut gaps: Vec<InspectionGap>,
    ) -> Result<Self, HistoryEvidenceError> {
        if source.as_ref().is_some_and(|name| !name.is_valid())
            || (coverage != NativeHistoryCoverage::Unavailable
                && (source.is_none()
                    || observation.server_version_num.is_none()
                    || adapter == MigrationAdapter::Plain))
            || (adapter == MigrationAdapter::Plain && source.is_some())
        {
            return Err(HistoryEvidenceError::InvalidSource);
        }
        if coverage == NativeHistoryCoverage::Unavailable && !rows.is_empty() {
            return Err(HistoryEvidenceError::InvalidCoverage);
        }
        if rows.iter().any(|row| !is_valid_row(row, adapter)) {
            return Err(HistoryEvidenceError::InvalidRow);
        }
        let mut identities = std::collections::BTreeMap::new();
        for row in &rows {
            let key = (&row.identity, row.direction);
            let ranked = row.rank.is_some();
            if let Some(previous_ranked) = identities.insert(key, ranked)
                && !(previous_ranked && ranked)
            {
                return Err(HistoryEvidenceError::AmbiguousRows);
            }
        }
        rows.sort_unstable_by(|left, right| {
            (left.rank, &left.identity, left.direction).cmp(&(
                right.rank,
                &right.identity,
                right.direction,
            ))
        });
        if rows
            .windows(2)
            .any(|pair| pair[0].rank.is_some() && pair[0].rank == pair[1].rank)
        {
            return Err(HistoryEvidenceError::AmbiguousRows);
        }
        gaps.sort_unstable();
        gaps.dedup();
        Ok(Self {
            schema_version: SchemaVersionV1,
            observation,
            adapter,
            source,
            inventory_digest,
            coverage,
            rows,
            gaps,
        })
    }

    pub fn observation(&self) -> &Observation {
        &self.observation
    }

    pub fn adapter(&self) -> MigrationAdapter {
        self.adapter
    }

    pub fn source(&self) -> Option<&QualifiedName> {
        self.source.as_ref()
    }

    pub fn inventory_digest(&self) -> ContentDigest {
        self.inventory_digest
    }

    pub fn coverage(&self) -> NativeHistoryCoverage {
        self.coverage
    }

    pub fn rows(&self) -> &[NativeHistoryRow] {
        &self.rows
    }

    pub fn gaps(&self) -> &[InspectionGap] {
        &self.gaps
    }

    /// Binds the canonical history evidence, including its own target, time, and coverage.
    pub fn digest(&self) -> Result<ContentDigest, serde_json::Error> {
        serde_json::to_vec(self).map(|bytes| ContentDigest::of(&bytes))
    }
}

fn is_valid_row(row: &NativeHistoryRow, adapter: MigrationAdapter) -> bool {
    row.identity.adapter == adapter
        && MigrationIdentity::from_version(adapter, row.identity.version.clone()).is_ok()
        && (row.direction == MigrationDirection::Forward || adapter == MigrationAdapter::Flyway)
        && (adapter != MigrationAdapter::Flyway || row.rank.is_some_and(|rank| rank > 0))
        && (adapter != MigrationAdapter::Goose
            || row
                .identity
                .version
                .as_numeric()
                .is_some_and(|version| version > 0))
        && !(row.direction == MigrationDirection::Down && row.identity.is_repeatable())
        && matches!(
            (adapter, row.checksum),
            (_, None)
                | (
                    MigrationAdapter::Sqlx,
                    Some(NativeChecksum::SqlxSha384 { .. })
                )
                | (
                    MigrationAdapter::Flyway,
                    Some(NativeChecksum::FlywayCrc32 { .. })
                )
        )
}

/// Binds adapter, forward identities, repository-relative paths, and original-byte digests.
pub fn forward_inventory_digest(
    adapter: MigrationAdapter,
    inventory: &[MigrationFile],
) -> Result<ContentDigest, HistoryEvidenceError> {
    #[derive(Serialize)]
    struct Binding<'a> {
        schema_version: SchemaVersionV1,
        adapter: MigrationAdapter,
        forwards: Vec<Forward<'a>>,
    }
    #[derive(Serialize)]
    struct Forward<'a> {
        identity: &'a MigrationIdentity,
        path: &'a RepoPath,
        digest: ContentDigest,
    }

    if inventory
        .iter()
        .any(|file| file.identity.adapter != adapter)
    {
        return Err(HistoryEvidenceError::InvalidInventory);
    }
    let mut forwards = inventory
        .iter()
        .filter(|file| file.direction == MigrationDirection::Forward)
        .map(|file| Forward {
            identity: &file.identity,
            path: &file.path,
            digest: file.digest,
        })
        .collect::<Vec<_>>();
    forwards.sort_unstable_by(|left, right| left.identity.cmp(right.identity));
    let mut paths = std::collections::BTreeSet::new();
    if forwards.iter().any(|file| {
        !paths.insert(file.path)
            || MigrationIdentity::from_version(adapter, file.identity.version.clone()).is_err()
    }) || forwards
        .windows(2)
        .any(|pair| pair[0].identity == pair[1].identity)
    {
        return Err(HistoryEvidenceError::InvalidInventory);
    }
    let bytes = serde_json::to_vec(&Binding {
        schema_version: SchemaVersionV1,
        adapter,
        forwards,
    })?;
    Ok(ContentDigest::of(&bytes))
}

#[derive(Deserialize)]
struct NativeHistoryFields {
    #[serde(rename = "schema_version")]
    _schema_version: SchemaVersionV1,
    observation: Observation,
    adapter: MigrationAdapter,
    source: Option<QualifiedName>,
    inventory_digest: ContentDigest,
    coverage: NativeHistoryCoverage,
    rows: Vec<NativeHistoryRow>,
    gaps: Vec<InspectionGap>,
}

impl TryFrom<NativeHistoryFields> for NativeHistoryEvidenceV1 {
    type Error = HistoryEvidenceError;

    fn try_from(fields: NativeHistoryFields) -> Result<Self, Self::Error> {
        Self::new(
            fields.observation,
            fields.adapter,
            fields.source,
            fields.inventory_digest,
            fields.coverage,
            fields.rows,
            fields.gaps,
        )
    }
}

#[derive(Debug, Error)]
pub enum HistoryEvidenceError {
    #[error("native history evidence has an invalid or missing source")]
    InvalidSource,
    #[error("unavailable native history cannot contain observed rows")]
    InvalidCoverage,
    #[error("native history row disagrees with the declared runner or identity")]
    InvalidRow,
    #[error("native history rows have duplicate ranks or ambiguous identities")]
    AmbiguousRows,
    #[error("cannot bind native history to an ambiguous or different forward inventory")]
    InvalidInventory,
    #[error("could not serialize the native history inventory binding")]
    Serialize(#[from] serde_json::Error),
}
