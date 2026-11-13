//! Reconcile authorized runner history against a bound local forward inventory.

use std::collections::{BTreeMap, BTreeSet};

use dmg_core::{
    coverage::{Coverage, CoverageGap, CoverageGapKind, DiagnosticScope},
    digest::ContentDigest,
    history::{
        HistoryEvidenceError, NativeChecksum, NativeHistoryCoverage, NativeHistoryEvidenceV1,
        NativeHistoryMethod, NativeHistoryRow, NativeHistoryState, Sha384Digest,
        forward_inventory_digest,
    },
    inspection::{InspectionField, InspectionGapReason, InspectionGapScope, TargetId},
    migration::{
        AppliedHistory, AppliedHistoryBasis, AppliedMigration, AppliedStatus, MigrationAdapter,
        MigrationDirection, MigrationFile, MigrationIdentity,
    },
};
use thiserror::Error;

use crate::{Discovery, coverage_gap};

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct NativeHistoryReconciliation {
    pub applied_history: AppliedHistory,
    /// Gaps in this reconciliation. The caller may retain this alongside discovery coverage.
    pub coverage: Coverage,
}

#[derive(Debug, Error)]
pub enum NativeHistoryError {
    #[error("the selected adapter uses a trusted applied manifest for history")]
    UnsupportedAdapter,
    #[error("native migration history belongs to a different adapter")]
    WrongAdapter,
    #[error("native migration history belongs to a different target")]
    WrongTarget,
    #[error("native migration history is bound to a different forward inventory")]
    WrongInventory,
    #[error("could not establish a unique forward inventory for native history")]
    Inventory(#[source] HistoryEvidenceError),
    #[error("could not digest native migration history")]
    EvidenceDigest(#[source] serde_json::Error),
}

/// Classify local forward migrations using a trusted, target-bound history observation.
///
/// The caller supplies evidence from the designated history source under the effective runner
/// profile. An absent active row in a complete history is pending in this comparison. The status
/// describes the history record only. Schema state and execution eligibility require their own
/// evidence.
pub fn reconcile_native_history(
    discovery: &Discovery,
    expected_target: &TargetId,
    evidence: &NativeHistoryEvidenceV1,
) -> Result<NativeHistoryReconciliation, NativeHistoryError> {
    let adapter = discovery.options.adapter.kind();
    let method = NativeHistoryMethod::for_adapter(adapter);
    if adapter == MigrationAdapter::Plain {
        return Err(NativeHistoryError::UnsupportedAdapter);
    }
    if evidence.adapter() != adapter {
        return Err(NativeHistoryError::WrongAdapter);
    }
    if &evidence.observation().target_id != expected_target {
        return Err(NativeHistoryError::WrongTarget);
    }
    let inventory_digest = forward_inventory_digest(adapter, &discovery.plan.inventory)
        .map_err(NativeHistoryError::Inventory)?;
    if evidence.inventory_digest() != inventory_digest {
        return Err(NativeHistoryError::WrongInventory);
    }

    let local = discovery
        .plan
        .inventory
        .iter()
        .filter(|file| file.direction == MigrationDirection::Forward)
        .map(|file| (&file.identity, file))
        .collect::<BTreeMap<_, _>>();
    let latest = latest_rows(evidence.rows());
    let identities = local
        .keys()
        .chain(latest.keys())
        .copied()
        .collect::<BTreeSet<_>>();
    let history_complete = evidence.coverage() == NativeHistoryCoverage::Complete;
    let mut checksum_gap = false;
    let mut migration_checksum_gaps = BTreeSet::new();
    for gap in evidence.gaps() {
        match (gap.field, &gap.scope) {
            (InspectionField::NativeChecksum, InspectionGapScope::Migration { identity }) => {
                migration_checksum_gaps.insert(identity);
            }
            (InspectionField::NativeChecksum, _) => checksum_gap = true,
            _ => {}
        }
    }
    let mut gaps = history_gaps(discovery, evidence);
    let mut migrations = Vec::with_capacity(identities.len());
    for identity in identities {
        let file = local.get(identity).copied();
        let row = latest.get(identity).copied();

        // SQLx's version is unique in its history table. A verified dirty row establishes a
        // failure even if other versions were omitted. A partial ranked history may omit a
        // later event that changes the state for the same identity.
        let status = if discovery.head_complete && history_complete {
            let checksum_available = !checksum_gap && !migration_checksum_gaps.contains(identity);
            classify_complete(discovery, method, file, row, checksum_available, &mut gaps)
        } else if row.is_some_and(|row| {
            row.state == NativeHistoryState::Failed
                && (history_complete || adapter == MigrationAdapter::Sqlx)
        }) {
            Some(AppliedStatus::Failed)
        } else {
            Some(AppliedStatus::Unknown)
        };
        if let Some(status) = status {
            migrations.push(AppliedMigration {
                identity: identity.clone(),
                status,
            });
        }
    }

    let digest = evidence
        .digest()
        .map_err(NativeHistoryError::EvidenceDigest)?;
    Ok(NativeHistoryReconciliation {
        applied_history: AppliedHistory {
            basis: AppliedHistoryBasis::Native {
                digest,
                inventory_digest,
                adapter,
                method,
            },
            migrations,
        },
        coverage: Coverage::from_gaps(gaps),
    })
}

fn latest_rows(rows: &[NativeHistoryRow]) -> BTreeMap<&MigrationIdentity, &NativeHistoryRow> {
    let mut latest = BTreeMap::<_, &NativeHistoryRow>::new();
    for row in rows {
        latest
            .entry(&row.identity)
            .and_modify(|previous| {
                if row.rank > previous.rank {
                    *previous = row;
                }
            })
            .or_insert(row);
    }
    latest
}

fn classify_complete(
    discovery: &Discovery,
    method: NativeHistoryMethod,
    file: Option<&MigrationFile>,
    row: Option<&NativeHistoryRow>,
    checksum_available: bool,
    gaps: &mut Vec<CoverageGap>,
) -> Option<AppliedStatus> {
    let Some(row) = row else {
        return Some(AppliedStatus::Pending);
    };
    match (row.direction, row.state) {
        (MigrationDirection::Down, NativeHistoryState::Applied)
        | (_, NativeHistoryState::Reverted) => file.map(|_| AppliedStatus::Pending),
        (_, NativeHistoryState::Failed) => Some(AppliedStatus::Failed),
        (_, NativeHistoryState::Superseded | NativeHistoryState::Unknown) => {
            gaps.push(migration_gap(
                &row.identity,
                file,
                "the latest native history event does not establish an active migration",
            ));
            Some(AppliedStatus::Unknown)
        }
        (MigrationDirection::Forward, NativeHistoryState::Applied) => {
            let Some(file) = file else {
                return Some(AppliedStatus::Missing);
            };
            if method == NativeHistoryMethod::PresenceOnly {
                return Some(AppliedStatus::Applied);
            }
            if !checksum_available {
                return Some(AppliedStatus::Unknown);
            }
            let local_checksum = discovery
                .source_bytes(&file.path)
                .filter(|bytes| ContentDigest::of(bytes) == file.digest)
                .and_then(|bytes| runner_checksum(row.identity.adapter, bytes));
            match (local_checksum.as_ref(), row.checksum.as_ref()) {
                (Some(local), Some(recorded)) if local == recorded => Some(AppliedStatus::Applied),
                (Some(_), Some(_)) => Some(AppliedStatus::Modified),
                _ => {
                    gaps.push(migration_gap(
                        &row.identity,
                        Some(file),
                        "the runner checksum cannot be verified for the original migration bytes",
                    ));
                    Some(AppliedStatus::Unknown)
                }
            }
        }
    }
}

fn history_gaps(discovery: &Discovery, evidence: &NativeHistoryEvidenceV1) -> Vec<CoverageGap> {
    let mut gaps = Vec::new();
    if !discovery.head_complete {
        gaps.push(coverage_gap(
            CoverageGapKind::MissingAppliedHistory,
            None,
            "complete head inventory is required for native migration history reconciliation",
        ));
    }
    if evidence.coverage() != NativeHistoryCoverage::Complete && evidence.gaps().is_empty() {
        gaps.push(coverage_gap(
            CoverageGapKind::MissingAppliedHistory,
            None,
            "native migration history is incomplete",
        ));
    }
    for gap in evidence.gaps() {
        let message = match gap.reason {
            InspectionGapReason::PermissionDenied => {
                "permission denied reading native migration history"
            }
            InspectionGapReason::Missing => "the designated native migration history is missing",
            InspectionGapReason::Truncated => {
                "native migration history exceeded the observation limit"
            }
            InspectionGapReason::Contention => {
                "native migration history read reached its lock limit"
            }
            InspectionGapReason::Cancelled => "native migration history read was cancelled",
            InspectionGapReason::Deadline => "native migration history read reached its deadline",
            InspectionGapReason::InvalidValue => {
                "native migration history contains an invalid value"
            }
            InspectionGapReason::Unsupported => {
                "native migration history contains an unsupported record"
            }
            InspectionGapReason::OutsideScope => {
                "native migration history is outside the authorized scope"
            }
            InspectionGapReason::QueryFailed => {
                "could not read the designated native migration history"
            }
        };
        gaps.push(match &gap.scope {
            InspectionGapScope::Migration { identity } => migration_gap(identity, None, message),
            _ => coverage_gap(CoverageGapKind::MissingAppliedHistory, None, message),
        });
    }
    gaps
}

fn migration_gap(
    identity: &MigrationIdentity,
    file: Option<&MigrationFile>,
    message: &'static str,
) -> CoverageGap {
    CoverageGap {
        kind: CoverageGapKind::MissingAppliedHistory,
        scope: DiagnosticScope::Migration {
            identity: identity.clone(),
        },
        message: message.into(),
        related_paths: file.map(|file| file.path.clone()).into_iter().collect(),
    }
}

fn runner_checksum(adapter: MigrationAdapter, bytes: &[u8]) -> Option<NativeChecksum> {
    let source = std::str::from_utf8(bytes).ok()?;
    match adapter {
        MigrationAdapter::Sqlx => Some(NativeChecksum::SqlxSha384 {
            digest: Sha384Digest::of(bytes),
        }),
        MigrationAdapter::Flyway if !source.contains("${") => Some(NativeChecksum::FlywayCrc32 {
            value: flyway_crc32(source),
        }),
        _ => None,
    }
}

fn flyway_crc32(source: &str) -> i32 {
    // Flyway hashes the UTF-8 bytes of each Java readLine result, removing an initial BOM.
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let mut crc = u32::MAX;
    for byte in source.bytes().filter(|byte| !matches!(byte, b'\n' | b'\r')) {
        crc = CRC32_TABLE[((crc ^ u32::from(byte)) & 0xff) as usize] ^ (crc >> 8);
    }
    !crc as i32
}

const CRC32_TABLE: [u32; 256] = {
    const REFLECTED_IEEE_POLYNOMIAL: u32 = 0xedb8_8320;
    let mut table = [0; 256];
    let mut byte = 0;
    while byte < table.len() {
        let mut crc = byte as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 == 0 {
                crc >> 1
            } else {
                (crc >> 1) ^ REFLECTED_IEEE_POLYNOMIAL
            };
            bit += 1;
        }
        table[byte] = crc;
        byte += 1;
    }
    table
};
