//! Explicit PostgreSQL analysis targets.

use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(try_from = "u16", into = "u16")]
pub enum PostgresTarget {
    V17,
    V18,
}

impl PostgresTarget {
    pub fn major(self) -> u16 {
        match self {
            Self::V17 => 17,
            Self::V18 => 18,
        }
    }
}

impl TryFrom<u16> for PostgresTarget {
    type Error = UnsupportedPostgresTarget;

    fn try_from(major: u16) -> Result<Self, Self::Error> {
        match major {
            17 => Ok(Self::V17),
            18 => Ok(Self::V18),
            _ => Err(UnsupportedPostgresTarget { major }),
        }
    }
}

impl From<PostgresTarget> for u16 {
    fn from(target: PostgresTarget) -> Self {
        target.major()
    }
}

impl fmt::Display for PostgresTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.major().fmt(formatter)
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
#[error("PostgreSQL {major} is outside the supported target majors 17 and 18")]
pub struct UnsupportedPostgresTarget {
    pub major: u16,
}
