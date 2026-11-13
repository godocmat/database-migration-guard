use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    application::ApplicationAssessmentV1, coverage::CoverageStatus, diff::SchemaComparisonV1,
};

/// The complete saved result of a schema comparison and its application checks.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct DiffReportV1 {
    #[serde(flatten)]
    comparison: SchemaComparisonV1,
    coverage: CoverageStatus,
    application_contracts: Vec<ApplicationAssessmentV1>,
}

impl DiffReportV1 {
    pub fn new(
        comparison: SchemaComparisonV1,
        application_contracts: Vec<ApplicationAssessmentV1>,
    ) -> Self {
        let coverage = Self::combined_coverage(&comparison, &application_contracts);
        Self {
            comparison,
            coverage,
            application_contracts,
        }
    }

    pub fn from_json(input: &str) -> Result<Self, DiffReportFormatError> {
        let report: Self = serde_json::from_str(input)?;
        let current = &report.comparison.expected_to_current.after;
        let target = &report.comparison.current_to_target.after;
        if report.coverage
            != Self::combined_coverage(&report.comparison, &report.application_contracts)
            || current != &report.comparison.current_to_target.before
            || report.application_contracts.iter().any(|assessment| {
                &assessment.current.source != current || &assessment.target.source != target
            })
        {
            return Err(DiffReportFormatError::Inconsistent);
        }
        Ok(report)
    }

    pub fn coverage(&self) -> CoverageStatus {
        self.coverage
    }

    pub fn comparison(&self) -> &SchemaComparisonV1 {
        &self.comparison
    }

    pub fn application_contracts(&self) -> &[ApplicationAssessmentV1] {
        &self.application_contracts
    }

    fn combined_coverage(
        comparison: &SchemaComparisonV1,
        application_contracts: &[ApplicationAssessmentV1],
    ) -> CoverageStatus {
        let complete = comparison.expected_to_current.coverage == CoverageStatus::Complete
            && comparison.current_to_target.coverage == CoverageStatus::Complete
            && application_contracts.iter().all(|assessment| {
                assessment.current.coverage == CoverageStatus::Complete
                    && assessment.target.coverage == CoverageStatus::Complete
            });
        if complete {
            CoverageStatus::Complete
        } else {
            CoverageStatus::Incomplete
        }
    }
}

#[derive(Debug, Error)]
pub enum DiffReportFormatError {
    #[error("invalid versioned diff report JSON")]
    Json(#[from] serde_json::Error),
    #[error("the diff report has inconsistent sources or coverage")]
    Inconsistent,
}
