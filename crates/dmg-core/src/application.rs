//! Versioned contracts for the structural dependencies of a deployed application version.

mod evaluate;

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    digest::ContentDigest,
    inspection::{InspectionScope, is_catalog_identifier},
    schema::SchemaVersionV1,
    snapshot::{QualifiedName, TableKind},
};

pub use evaluate::{
    ApplicationAssessmentV1, ContractCheck, ContractFact, ContractObservation, ContractStatus,
    ContractTransition, ContractUnknown, ContractViolation, FactAssessment, evaluate_contract,
};

pub const APPLICATION_CONTRACT_VERSION: u32 = SchemaVersionV1::NUMBER;
pub const MAX_APPLICATION_CONTRACT_BYTES: usize = 4 * 1024 * 1024;
const MAX_TABLES: usize = 256;
const MAX_TABLE_COLUMNS: usize = 256;
const MAX_COLUMNS: usize = 4096;

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "ContractFields")]
pub struct ApplicationContractV1 {
    schema_version: SchemaVersionV1,
    application: String,
    version: String,
    critical: bool,
    tables: Vec<TableRequirement>,
}

impl ApplicationContractV1 {
    pub fn new(
        application: impl Into<String>,
        version: impl Into<String>,
        critical: bool,
        mut tables: Vec<TableRequirement>,
    ) -> Result<Self, ApplicationContractError> {
        let application = application.into();
        let version = version.into();
        if !valid_label(&application) || !valid_label(&version) {
            return Err(ApplicationContractError::InvalidLabel);
        }
        if tables.is_empty() || tables.len() > MAX_TABLES {
            return Err(ApplicationContractError::TableLimit);
        }
        tables.sort_unstable_by(|left, right| left.table.cmp(&right.table));
        if tables.windows(2).any(|pair| pair[0].table == pair[1].table) {
            return Err(ApplicationContractError::DuplicateTable);
        }
        if tables.iter().any(|table| !table.table.is_valid()) {
            return Err(ApplicationContractError::InvalidIdentifier);
        }
        let schemas: BTreeSet<_> = tables.iter().map(|table| &table.table.schema).collect();
        InspectionScope::new(schemas.into_iter().cloned().collect())
            .map_err(|_| ApplicationContractError::InvalidScope)?;

        let mut column_count = 0;
        for table in &mut tables {
            if table.columns.len() > MAX_TABLE_COLUMNS {
                return Err(ApplicationContractError::ColumnLimit);
            }
            column_count += table.columns.len();
            if column_count > MAX_COLUMNS {
                return Err(ApplicationContractError::ColumnLimit);
            }
            table
                .columns
                .sort_unstable_by(|left, right| left.name.cmp(&right.name));
            if table
                .columns
                .windows(2)
                .any(|pair| pair[0].name == pair[1].name)
            {
                return Err(ApplicationContractError::DuplicateColumn);
            }
            if table.columns.iter().any(|column| {
                !is_catalog_identifier(&column.name) || !column.data_type.name.is_valid()
            }) {
                return Err(ApplicationContractError::InvalidIdentifier);
            }
        }

        Ok(Self {
            schema_version: SchemaVersionV1,
            application,
            version,
            critical,
            tables,
        })
    }

    pub fn from_json(input: impl AsRef<[u8]>) -> Result<Self, ApplicationContractError> {
        let input = input.as_ref();
        if input.len() > MAX_APPLICATION_CONTRACT_BYTES {
            return Err(ApplicationContractError::ByteLimit);
        }
        serde_json::from_slice(input).map_err(ApplicationContractError::Json)
    }

    /// SHA-256 of the canonical contract, including its application version and criticality.
    pub fn digest(&self) -> Result<ContentDigest, serde_json::Error> {
        serde_json::to_vec(self).map(|bytes| ContentDigest::of(&bytes))
    }

    pub fn application(&self) -> &str {
        &self.application
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn is_critical(&self) -> bool {
        self.critical
    }

    pub fn tables(&self) -> &[TableRequirement] {
        &self.tables
    }
}

fn valid_label(label: &str) -> bool {
    label.len() <= 128
        && label
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && label
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'+'))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ContractFields {
    #[serde(rename = "schema_version")]
    _schema_version: SchemaVersionV1,
    application: String,
    version: String,
    #[serde(default)]
    critical: bool,
    tables: Vec<TableRequirement>,
}

impl TryFrom<ContractFields> for ApplicationContractV1 {
    type Error = ApplicationContractError;

    fn try_from(fields: ContractFields) -> Result<Self, Self::Error> {
        Self::new(
            fields.application,
            fields.version,
            fields.critical,
            fields.tables,
        )
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TableRequirement {
    #[serde(with = "ExactName")]
    pub table: QualifiedName,
    /// An omitted kind accepts either an ordinary or partitioned table.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<TableKind>,
    #[serde(default)]
    pub columns: Vec<ColumnRequirement>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColumnRequirement {
    pub name: String,
    pub access: ColumnAccess,
    pub data_type: ColumnType,
    #[serde(default)]
    pub nullability: NullabilityRequirement,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColumnAccess {
    Read,
    /// The application supplies a direct column value without an identity override.
    Write,
    ReadWrite,
}

impl ColumnAccess {
    fn requires_explicit_write(self) -> bool {
        matches!(self, Self::Write | Self::ReadWrite)
    }
}

#[derive(Debug, Default, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NullabilityRequirement {
    #[default]
    Any,
    /// Requires the absence of the direct column NOT NULL flag.
    Nullable,
    /// Requires an enforced and validated direct column NOT NULL declaration.
    NotNull,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColumnType {
    #[serde(with = "ExactName")]
    pub name: QualifiedName,
    /// PostgreSQL's raw pg_attribute.atttypmod value.
    pub type_modifier: i32,
    /// PostgreSQL's declared pg_attribute.attndims value.
    pub array_dimensions: u16,
}

#[derive(Deserialize, Serialize)]
#[serde(remote = "QualifiedName", deny_unknown_fields)]
struct ExactName {
    schema: String,
    name: String,
}

#[derive(Debug, Error)]
pub enum ApplicationContractError {
    #[error("application contract exceeds the 4 MiB JSON limit")]
    ByteLimit,
    #[error("invalid application contract JSON")]
    Json(#[source] serde_json::Error),
    #[error("application and version labels must use 1 to 128 supported ASCII characters")]
    InvalidLabel,
    #[error("application contract requires 1 to 256 tables")]
    TableLimit,
    #[error("application contract exceeds 256 columns per table or 4096 columns total")]
    ColumnLimit,
    #[error("application contract requires valid exact catalog identifiers")]
    InvalidIdentifier,
    #[error("application contract requires at most 32 non-system table schemas")]
    InvalidScope,
    #[error("application contract repeats a qualified table")]
    DuplicateTable,
    #[error("application contract repeats a column in one table")]
    DuplicateColumn,
}
