use serde::{Deserialize, Serialize};

use crate::{
    coverage::CoverageStatus,
    diff::{
        DiffSource, SchemaInput, ScopeState,
        input::{Inventory, Presence},
    },
    digest::ContentDigest,
    schema::SchemaVersionV1,
    snapshot::{
        ColumnIdentity, ColumnSnapshot, ConstraintKind, QualifiedName, TableKind, TableSnapshot,
    },
};

use super::{ApplicationContractV1, ColumnAccess, ColumnType, NullabilityRequirement};

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct ApplicationAssessmentV1 {
    schema_version: SchemaVersionV1,
    pub application: String,
    pub version: String,
    pub contract_digest: ContentDigest,
    pub critical: bool,
    pub current: ContractObservation,
    pub target: ContractObservation,
    pub critical_target_violation: bool,
    pub checks: Vec<ContractCheck>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContractObservation {
    pub source: DiffSource,
    pub status: ContractStatus,
    pub coverage: CoverageStatus,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContractCheck {
    pub table: QualifiedName,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<String>,
    pub fact: ContractFact,
    pub current: FactAssessment,
    pub target: FactAssessment,
    pub transition: ContractTransition,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContractFact {
    Table {
        expected_kind: Option<TableKind>,
    },
    ColumnType {
        access: ColumnAccess,
        expected: ColumnType,
    },
    ExplicitWrite,
    Nullability {
        expected: NullabilityRequirement,
    },
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContractStatus {
    Satisfied,
    Violated,
    Unknown,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContractTransition {
    Preserved,
    Regression,
    PreexistingViolation,
    Resolved,
    Indeterminate,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum FactAssessment {
    Satisfied,
    Violated { reason: ContractViolation },
    Unknown { reason: ContractUnknown },
}

impl FactAssessment {
    fn status(&self) -> ContractStatus {
        match self {
            Self::Satisfied => ContractStatus::Satisfied,
            Self::Violated { .. } => ContractStatus::Violated,
            Self::Unknown { .. } => ContractStatus::Unknown,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContractViolation {
    MissingTable,
    MissingColumn,
    TableKindMismatch { observed: TableKind },
    ColumnTypeMismatch { observed: ColumnType },
    MissingNotNull,
    DeclaredNotNull,
    GeneratedColumn,
    AlwaysIdentity,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContractUnknown {
    TableUnobserved { scope: ScopeState },
    ColumnUnobserved,
    NotNullConstraintUnobserved,
    NotNullUnvalidated,
    NotNullUnenforced,
}

/// Assess one application version's exact dependencies on both supplied schemas.
pub fn evaluate_contract<'a>(
    contract: &ApplicationContractV1,
    current: impl Into<SchemaInput<'a>>,
    target: impl Into<SchemaInput<'a>>,
) -> Result<ApplicationAssessmentV1, serde_json::Error> {
    let current = ContractInput::new(current.into())?;
    let target = ContractInput::new(target.into())?;
    let mut checks = Vec::new();

    for requirement in contract.tables() {
        let name = &requirement.table;
        let current_table = current.require_table(name);
        let target_table = target.require_table(name);
        checks.push(ContractCheck::new(
            name,
            None,
            ContractFact::Table {
                expected_kind: requirement.kind,
            },
            assess(&current_table, |table| table_kind(table, requirement.kind)),
            assess(&target_table, |table| table_kind(table, requirement.kind)),
        ));

        for dependency in &requirement.columns {
            let column = Some(dependency.name.as_str());
            let current_column = current.require_column(&current_table, &dependency.name);
            let target_column = target.require_column(&target_table, &dependency.name);
            checks.push(ContractCheck::new(
                name,
                column,
                ContractFact::ColumnType {
                    access: dependency.access,
                    expected: dependency.data_type.clone(),
                },
                assess(&current_column, |column| {
                    column_type(column, &dependency.data_type)
                }),
                assess(&target_column, |column| {
                    column_type(column, &dependency.data_type)
                }),
            ));
            if dependency.access.requires_explicit_write() {
                checks.push(ContractCheck::new(
                    name,
                    column,
                    ContractFact::ExplicitWrite,
                    assess(&current_column, explicit_write),
                    assess(&target_column, explicit_write),
                ));
            }
            if dependency.nullability != NullabilityRequirement::Any {
                checks.push(ContractCheck::new(
                    name,
                    column,
                    ContractFact::Nullability {
                        expected: dependency.nullability,
                    },
                    assess(&current_column, |column| {
                        current.nullability(name, column, dependency.nullability)
                    }),
                    assess(&target_column, |column| {
                        target.nullability(name, column, dependency.nullability)
                    }),
                ));
            }
        }
    }

    let current = summarize(current.source, checks.iter().map(|check| &check.current));
    let target = summarize(target.source, checks.iter().map(|check| &check.target));
    Ok(ApplicationAssessmentV1 {
        schema_version: SchemaVersionV1,
        application: contract.application().into(),
        version: contract.version().into(),
        contract_digest: contract.digest()?,
        critical: contract.is_critical(),
        critical_target_violation: contract.is_critical()
            && target.status == ContractStatus::Violated,
        current,
        target,
        checks,
    })
}

struct ContractInput<'a> {
    inventory: Inventory<'a>,
    source: DiffSource,
}

impl<'a> ContractInput<'a> {
    fn new(input: SchemaInput<'a>) -> Result<Self, serde_json::Error> {
        let inventory = Inventory::new(input);
        let source = inventory.source()?;
        Ok(Self { inventory, source })
    }

    fn require_table(&self, name: &QualifiedName) -> Result<&'a TableSnapshot, FactAssessment> {
        if let Some(table) = self.inventory.table(name) {
            return Ok(table);
        }
        Err(match self.inventory.table_presence(name) {
            Presence::Absent => violated(ContractViolation::MissingTable),
            _ => unknown(ContractUnknown::TableUnobserved {
                scope: self.inventory.scope_state(&name.schema),
            }),
        })
    }

    fn require_column(
        &self,
        table: &Result<&'a TableSnapshot, FactAssessment>,
        name: &str,
    ) -> Result<&'a ColumnSnapshot, FactAssessment> {
        let table = table.as_ref().map_err(Clone::clone)?;
        if let Some(column) = self.inventory.column(&table.name, name) {
            return Ok(column);
        }
        Err(match self.inventory.column_presence(&table.name, name) {
            Presence::Absent => violated(ContractViolation::MissingColumn),
            _ => unknown(ContractUnknown::ColumnUnobserved),
        })
    }

    fn nullability(
        &self,
        table: &QualifiedName,
        column: &ColumnSnapshot,
        expected: NullabilityRequirement,
    ) -> FactAssessment {
        match expected {
            NullabilityRequirement::Any => FactAssessment::Satisfied,
            NullabilityRequirement::Nullable if column.not_null => {
                violated(ContractViolation::DeclaredNotNull)
            }
            NullabilityRequirement::Nullable => FactAssessment::Satisfied,
            NullabilityRequirement::NotNull if !column.not_null => {
                violated(ContractViolation::MissingNotNull)
            }
            NullabilityRequirement::NotNull => self.validated_not_null(table, column),
        }
    }

    fn validated_not_null(&self, table: &QualifiedName, column: &ColumnSnapshot) -> FactAssessment {
        let major = self
            .source
            .observation
            .as_ref()
            .and_then(|observation| observation.server_version_num)
            .map(|version| version / 10_000);
        if matches!(major, Some(15..=17)) {
            return FactAssessment::Satisfied;
        }

        let mut unvalidated = false;
        let mut unenforced = false;
        let constraints = self
            .inventory
            .table(table)
            .map(|table| table.constraints.as_slice())
            .unwrap_or_default();
        for constraint in constraints.iter().filter(|constraint| {
            constraint.kind == ConstraintKind::NotNull
                && constraint.columns.as_slice() == [column.name.as_str()]
                && self
                    .inventory
                    .constraint_columns_observed(table, &constraint.name)
        }) {
            if constraint.enforced && constraint.validated {
                return FactAssessment::Satisfied;
            }
            unvalidated |= constraint.enforced && !constraint.validated;
            unenforced |= !constraint.enforced;
        }
        let reason = if unvalidated {
            ContractUnknown::NotNullUnvalidated
        } else if unenforced {
            ContractUnknown::NotNullUnenforced
        } else {
            ContractUnknown::NotNullConstraintUnobserved
        };
        unknown(reason)
    }
}

impl ContractCheck {
    fn new(
        table: &QualifiedName,
        column: Option<&str>,
        fact: ContractFact,
        current: FactAssessment,
        target: FactAssessment,
    ) -> Self {
        use ContractStatus::{Satisfied, Violated};
        let transition = match (current.status(), target.status()) {
            (Satisfied, Satisfied) => ContractTransition::Preserved,
            (Satisfied, Violated) => ContractTransition::Regression,
            (Violated, Violated) => ContractTransition::PreexistingViolation,
            (Violated, Satisfied) => ContractTransition::Resolved,
            _ => ContractTransition::Indeterminate,
        };
        Self {
            table: table.clone(),
            column: column.map(str::to_owned),
            fact,
            current,
            target,
            transition,
        }
    }
}

fn summarize<'a>(
    source: DiffSource,
    outcomes: impl IntoIterator<Item = &'a FactAssessment>,
) -> ContractObservation {
    let mut coverage = CoverageStatus::Complete;
    let mut violation = false;
    for outcome in outcomes {
        match outcome.status() {
            ContractStatus::Unknown => coverage = CoverageStatus::Incomplete,
            ContractStatus::Violated => violation = true,
            ContractStatus::Satisfied => {}
        }
    }
    let status = if violation {
        ContractStatus::Violated
    } else if coverage == CoverageStatus::Incomplete {
        ContractStatus::Unknown
    } else {
        ContractStatus::Satisfied
    };
    ContractObservation {
        source,
        status,
        coverage,
    }
}

fn assess<T>(
    fact: &Result<&T, FactAssessment>,
    evaluate: impl FnOnce(&T) -> FactAssessment,
) -> FactAssessment {
    match fact {
        Ok(value) => evaluate(value),
        Err(error) => error.clone(),
    }
}

fn table_kind(table: &TableSnapshot, expected: Option<TableKind>) -> FactAssessment {
    if expected.is_some_and(|expected| expected != table.kind) {
        violated(ContractViolation::TableKindMismatch {
            observed: table.kind,
        })
    } else {
        FactAssessment::Satisfied
    }
}

fn column_type(column: &ColumnSnapshot, expected: &ColumnType) -> FactAssessment {
    if column.data_type == expected.name
        && column.type_modifier == expected.type_modifier
        && column.array_dimensions == expected.array_dimensions
    {
        FactAssessment::Satisfied
    } else {
        violated(ContractViolation::ColumnTypeMismatch {
            observed: ColumnType {
                name: column.data_type.clone(),
                type_modifier: column.type_modifier,
                array_dimensions: column.array_dimensions,
            },
        })
    }
}

fn explicit_write(column: &ColumnSnapshot) -> FactAssessment {
    if column.generation.is_some() {
        violated(ContractViolation::GeneratedColumn)
    } else if column.identity == Some(ColumnIdentity::Always) {
        violated(ContractViolation::AlwaysIdentity)
    } else {
        FactAssessment::Satisfied
    }
}

fn violated(reason: ContractViolation) -> FactAssessment {
    FactAssessment::Violated { reason }
}

fn unknown(reason: ContractUnknown) -> FactAssessment {
    FactAssessment::Unknown { reason }
}
