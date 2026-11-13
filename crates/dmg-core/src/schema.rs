use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Default, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "u32", into = "u32")]
pub(crate) struct SchemaVersionV1;

impl SchemaVersionV1 {
    pub const NUMBER: u32 = 1;
}

impl TryFrom<u32> for SchemaVersionV1 {
    type Error = UnsupportedSchemaVersion;

    fn try_from(version: u32) -> Result<Self, Self::Error> {
        if version != Self::NUMBER {
            return Err(UnsupportedSchemaVersion(version));
        }

        Ok(Self)
    }
}

impl From<SchemaVersionV1> for u32 {
    fn from(_: SchemaVersionV1) -> Self {
        SchemaVersionV1::NUMBER
    }
}

#[derive(Debug, Error)]
#[error("unsupported schema version {0} (expected 1)")]
pub(crate) struct UnsupportedSchemaVersion(u32);
