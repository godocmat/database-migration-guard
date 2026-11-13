//! Authenticated offline import of scoped catalog observations.

use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use clap::{Args, value_parser};
use dmg_core::{
    coverage::{CoverageGap, CoverageGapKind},
    digest::ContentDigest,
    inspection::{InspectionEvidenceV1, SchemaState, TargetId},
    target::PostgresTarget,
};

use crate::{config::read_input, evidence_gap, inspection::parse_target_id, parse_digest};

const MAX_CATALOG_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Args)]
pub(crate) struct CatalogEvidenceArgs {
    /// Trusted catalog JSON from dmg inspect.
    #[arg(long, value_name = "FILE", requires_all = ["catalog_evidence_sha256", "catalog_target_id", "catalog_max_age_seconds"])]
    catalog_evidence: Option<PathBuf>,

    /// SHA-256 of the exact saved catalog file bytes.
    #[arg(long, value_name = "SHA256", requires = "catalog_evidence", value_parser = parse_digest)]
    catalog_evidence_sha256: Option<ContentDigest>,

    /// Expected opaque target ID for the saved catalog observation.
    #[arg(long, value_name = "ID", requires = "catalog_evidence", value_parser = parse_target_id)]
    catalog_target_id: Option<TargetId>,

    /// Maximum age of the catalog observation in seconds.
    #[arg(long, value_name = "SECONDS", requires = "catalog_evidence", value_parser = value_parser!(u64).range(1..))]
    catalog_max_age_seconds: Option<u64>,
}

pub(crate) struct PinnedCatalog {
    pub(crate) evidence: InspectionEvidenceV1,
    pub(crate) digest: ContentDigest,
}

impl PinnedCatalog {
    pub(crate) fn unavailable_reason(&self) -> Option<String> {
        if self.evidence.observation().server_version_num.is_none() {
            Some("the observed server major is unavailable".into())
        } else if self.evidence.snapshot().is_none()
            && self
                .evidence
                .scope()
                .iter()
                .any(|schema| schema.state != SchemaState::Missing)
        {
            Some("the observation has no usable catalog snapshot".into())
        } else {
            None
        }
    }
}

impl CatalogEvidenceArgs {
    pub(crate) fn load(
        &self,
        target: PostgresTarget,
        policy_target: Option<&TargetId>,
        history_target: Option<&TargetId>,
    ) -> Result<Option<PinnedCatalog>, String> {
        let (Some(path), Some(digest), Some(target_id), Some(max_age)) = (
            self.catalog_evidence.as_deref(),
            self.catalog_evidence_sha256,
            self.catalog_target_id.as_ref(),
            self.catalog_max_age_seconds,
        ) else {
            return Ok(None);
        };
        for (expected, name) in [
            (policy_target, "the protected policy"),
            (history_target, "native history"),
        ] {
            if expected.is_some_and(|expected| expected != target_id) {
                return Err(format!("the opaque target ID differs from {name}"));
            }
        }
        load_catalog(path, digest, target_id, target, max_age)
            .map(|evidence| Some(PinnedCatalog { evidence, digest }))
    }
}

fn load_catalog(
    path: &Path,
    digest: ContentDigest,
    target_id: &TargetId,
    target: PostgresTarget,
    max_age: u64,
) -> Result<InspectionEvidenceV1, String> {
    let bytes = read_input(path, MAX_CATALOG_BYTES).map_err(|error| error.to_string())?;
    if ContentDigest::of(&bytes) != digest {
        return Err("the saved file SHA-256 does not match its trusted pin".into());
    }
    let evidence: InspectionEvidenceV1 =
        serde_json::from_slice(&bytes).map_err(|_| "invalid versioned catalog JSON")?;
    let observation = evidence.observation();
    if observation.target_id != *target_id {
        return Err("the observation belongs to a different target".into());
    }
    if observation
        .server_version_num
        .is_some_and(|version| version / 10_000 != u32::from(target.major()))
    {
        return Err("the observed server major differs from the project target".into());
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "the system clock cannot verify catalog freshness")?
        .as_secs();
    let age = now
        .checked_sub(observation.observed_at_unix_seconds)
        .ok_or("the observation timestamp is in the future")?;
    if age > max_age {
        return Err("the observation is older than the configured maximum age".into());
    }
    Ok(evidence)
}

pub(crate) fn catalog_gap(message: String) -> CoverageGap {
    evidence_gap(
        CoverageGapKind::MissingCatalogEvidence,
        "Catalog evidence",
        message,
    )
}
