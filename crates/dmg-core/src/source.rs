//! Original-byte source coordinates for UTF-8 migration files.

use std::{fmt, num::NonZeroUsize};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A normalized, slash-delimited path relative to the repository root.
#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct RepoPath(String);

impl RepoPath {
    /// Validates the portable path representation. Filesystem containment is checked at I/O.
    pub fn new(path: impl Into<String>) -> Result<Self, SourceError> {
        Self::try_from(path.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for RepoPath {
    type Error = SourceError;

    fn try_from(path: String) -> Result<Self, Self::Error> {
        if path.contains(['\\', ':', '\0'])
            || path
                .split('/')
                .any(|component| matches!(component, "" | "." | ".."))
        {
            return Err(SourceError::InvalidRepoPath);
        }

        Ok(Self(path))
    }
}

impl AsRef<str> for RepoPath {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for RepoPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A zero-based, half-open range into the original file bytes.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(try_from = "ByteRangeFields")]
pub struct ByteRange {
    start: usize,
    end: usize,
}

#[derive(Deserialize)]
struct ByteRangeFields {
    start: usize,
    end: usize,
}

impl TryFrom<ByteRangeFields> for ByteRange {
    type Error = SourceError;

    fn try_from(fields: ByteRangeFields) -> Result<Self, Self::Error> {
        Self::new(fields.start, fields.end)
    }
}

impl ByteRange {
    pub fn new(start: usize, end: usize) -> Result<Self, SourceError> {
        if start > end {
            return Err(SourceError::ReversedRange { start, end });
        }

        Ok(Self { start, end })
    }

    pub fn start(self) -> usize {
        self.start
    }

    pub fn end(self) -> usize {
        self.end
    }

    pub fn len(self) -> usize {
        self.end - self.start
    }

    pub fn is_empty(self) -> bool {
        self.start == self.end
    }

    pub fn as_range(self) -> std::ops::Range<usize> {
        self.start..self.end
    }
}

/// One-based line and Unicode scalar column.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct SourcePosition {
    pub line: NonZeroUsize,
    pub column: NonZeroUsize,
}

/// A source range and its display coordinates in the original file.
#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct SourceSpan {
    pub path: RepoPath,
    pub bytes: ByteRange,
    pub start: SourcePosition,
    pub end: SourcePosition,
}

/// Maps original UTF-8 file offsets to positions without altering the file.
pub struct SourceMap<'a> {
    path: RepoPath,
    source: &'a str,
    line_starts: Vec<usize>,
}

impl<'a> SourceMap<'a> {
    pub fn new(path: RepoPath, source: &'a str) -> Self {
        let line_starts = std::iter::once(0)
            .chain(source.match_indices('\n').map(|(offset, _)| offset + 1))
            .collect();
        Self {
            path,
            source,
            line_starts,
        }
    }

    pub fn position(&self, offset: usize) -> Result<SourcePosition, SourceError> {
        if offset > self.source.len() {
            return Err(SourceError::OffsetOutOfBounds {
                offset,
                length: self.source.len(),
            });
        }
        if !self.source.is_char_boundary(offset) {
            return Err(SourceError::InvalidUtf8Boundary { offset });
        }

        let line = self.line_starts.partition_point(|start| *start <= offset);
        let line_start = self.line_starts[line - 1];
        let prefix = &self.source[line_start..offset];
        // Both bytes of a CRLF delimiter map to the end of the preceding line.
        let display_prefix = if self.source.as_bytes().get(offset) == Some(&b'\n') {
            prefix.strip_suffix('\r').unwrap_or(prefix)
        } else {
            prefix
        };
        Ok(SourcePosition {
            line: NonZeroUsize::new(line).expect("the first line starts at byte zero"),
            column: NonZeroUsize::new(display_prefix.chars().count() + 1)
                .expect("columns start at one"),
        })
    }

    pub fn span(&self, bytes: ByteRange) -> Result<SourceSpan, SourceError> {
        Ok(SourceSpan {
            path: self.path.clone(),
            bytes,
            start: self.position(bytes.start)?,
            end: self.position(bytes.end)?,
        })
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum SourceError {
    #[error("expected a normalized repository-relative path")]
    InvalidRepoPath,
    #[error("source byte range starts at {start} after its end at {end}")]
    ReversedRange { start: usize, end: usize },
    #[error("source byte offset {offset} exceeds the file length of {length} bytes")]
    OffsetOutOfBounds { offset: usize, length: usize },
    #[error("source byte offset {offset} is inside a UTF-8 code point")]
    InvalidUtf8Boundary { offset: usize },
}
