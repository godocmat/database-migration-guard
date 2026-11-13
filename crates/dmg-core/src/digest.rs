//! Digests over original bytes.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// A SHA-256 digest serialized as 64 lowercase hexadecimal digits.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ContentDigest([u8; 32]);

impl ContentDigest {
    pub fn of(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }
}

impl FromStr for ContentDigest {
    type Err = InvalidContentDigest;

    fn from_str(hex: &str) -> Result<Self, Self::Err> {
        if hex.len() != 64
            || !hex
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        {
            return Err(InvalidContentDigest);
        }

        let mut bytes = [0; 32];
        for (index, byte) in bytes.iter_mut().enumerate() {
            let start = index * 2;
            *byte =
                u8::from_str_radix(&hex[start..start + 2], 16).map_err(|_| InvalidContentDigest)?;
        }

        Ok(Self(bytes))
    }
}

impl TryFrom<String> for ContentDigest {
    type Error = InvalidContentDigest;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<ContentDigest> for String {
    fn from(digest: ContentDigest) -> Self {
        digest.to_string()
    }
}

impl fmt::Display for ContentDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }

        Ok(())
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
#[error("expected a SHA-256 digest with 64 lowercase hexadecimal digits")]
pub struct InvalidContentDigest;
