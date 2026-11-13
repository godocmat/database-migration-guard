//! Migration keys preserve native ordering and the existing numeric wire shape.

use std::{cmp::Ordering, fmt};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MigrationVersion {
    Numeric(i64),
    Native(NativeVersion),
}

impl MigrationVersion {
    pub fn as_numeric(&self) -> Option<i64> {
        match self {
            Self::Numeric(value) => Some(*value),
            Self::Native(_) => None,
        }
    }
}

impl From<i64> for MigrationVersion {
    fn from(value: i64) -> Self {
        Self::Numeric(value)
    }
}

impl fmt::Display for MigrationVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Numeric(value) => value.fmt(formatter),
            Self::Native(NativeVersion::Flyway(value)) => formatter.write_str(value.as_str()),
            Self::Native(NativeVersion::Repeatable(value)) => {
                write!(formatter, "repeatable {:?}", value.as_str())
            }
            Self::Native(NativeVersion::Supabase(value)) => formatter.write_str(value.as_str()),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum NativeVersion {
    Flyway(FlywayVersion),
    Repeatable(FlywayRepeatable),
    Supabase(SupabaseVersion),
}

/// Canonical nonnegative Flyway version with dotted decimal components.
#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct FlywayVersion(String);

impl FlywayVersion {
    pub fn new(value: &str) -> Result<Self, InvalidMigrationVersion> {
        if value.len() > MAX_NATIVE_VERSION_BYTES {
            return Err(InvalidMigrationVersion);
        }
        let mut parts = Vec::new();
        for part in value.split(['.', '_']) {
            if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(InvalidMigrationVersion);
            }
            let normalized = part.trim_start_matches('0');
            parts.push(if normalized.is_empty() {
                "0"
            } else {
                normalized
            });
        }
        while parts.len() > 1 && parts.last() == Some(&"0") {
            parts.pop();
        }
        let version = parts.join(".");
        // This integer has special sentinel semantics in Flyway.
        if version == "9223372036854775807" {
            return Err(InvalidMigrationVersion);
        }
        Ok(Self(version))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for FlywayVersion {
    type Error = InvalidMigrationVersion;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(&value)
    }
}

impl Ord for FlywayVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        for (left, right) in self.0.split('.').zip(other.0.split('.')) {
            let order = left.len().cmp(&right.len()).then_with(|| left.cmp(right));
            if order != Ordering::Equal {
                return order;
            }
        }
        // A remaining component must eventually be positive after canonicalization.
        self.0.len().cmp(&other.0.len())
    }
}

impl PartialOrd for FlywayVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Flyway uses the normalized repeatable description as its identity.
#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct FlywayRepeatable(String);

impl FlywayRepeatable {
    pub fn new(description: &str) -> Result<Self, InvalidMigrationVersion> {
        if description.len() > MAX_NATIVE_VERSION_BYTES || description.chars().any(char::is_control)
        {
            return Err(InvalidMigrationVersion);
        }
        Ok(Self(description.replace('_', " ")))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for FlywayRepeatable {
    type Error = InvalidMigrationVersion;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(&value)
    }
}

impl Ord for FlywayRepeatable {
    fn cmp(&self, other: &Self) -> Ordering {
        // Java String.compareTo orders UTF-16 code units.
        self.0.encode_utf16().cmp(other.0.encode_utf16())
    }
}

impl PartialOrd for FlywayRepeatable {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Supabase orders the exact ASCII digits lexicographically, including leading zeroes.
#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct SupabaseVersion(String);

impl SupabaseVersion {
    pub fn new(value: &str) -> Result<Self, InvalidMigrationVersion> {
        if value.is_empty()
            || value.len() > MAX_NATIVE_VERSION_BYTES
            || !value.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(InvalidMigrationVersion);
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for SupabaseVersion {
    type Error = InvalidMigrationVersion;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(&value)
    }
}

const MAX_NATIVE_VERSION_BYTES: usize = 1024;

#[derive(Debug, Clone, Copy, Eq, PartialEq, Error)]
#[error("invalid migration version for the adapter")]
pub struct InvalidMigrationVersion;
