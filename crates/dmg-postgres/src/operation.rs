use dmg_core::{
    coverage::Coverage,
    migration::{ExecutionMode, MigrationIdentity, StatementDelivery},
    risk::ObjectKind,
    source::{RepoPath, SourceSpan},
};

/// Forward migrations in discovery order, including unchanged migrations used as context.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ParsedPlan {
    pub migrations: Vec<ParsedMigration>,
    /// Ordered paths selected by discovery for findings. None indicates unresolved selection.
    pub selected_forwards: Option<Vec<RepoPath>>,
    pub coverage: Coverage,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ParsedMigration {
    pub identity: MigrationIdentity,
    pub path: RepoPath,
    pub execution: ExecutionMode,
    pub statements: Vec<Statement>,
    pub coverage: Coverage,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Statement {
    pub span: SourceSpan,
    /// Effective runner segment, numbered from zero.
    pub segment: usize,
    /// Protocol delivery of this statement's effective runner segment.
    pub delivery: StatementDelivery,
    pub operation: Operation,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum Operation {
    CreateSchema {
        name: String,
        if_not_exists: bool,
    },
    CreateTable(CreateTable),
    AlterTable(AlterTable),
    Rename(Rename),
    Drop(DropObjects),
    Truncate(Truncate),
    CreateIndex(CreateIndex),
    Update(DataChange),
    Delete(DataChange),
    Insert(RelationName),
    Transaction(Transaction),
    /// A SELECT of supported literal expressions.
    Select,
    /// The grammar accepted the statement, and a coverage gap identifies the unsupported scope.
    Unsupported,
}

/// Identifier components decoded by PostgreSQL. An absent schema needs name resolution.
#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct RelationName {
    pub catalog: Option<String>,
    pub schema: Option<String>,
    pub name: String,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum RelationKind {
    Table,
    PartitionedTable,
    Partition,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CreateTable {
    pub relation: RelationName,
    pub kind: RelationKind,
    pub partition_of: Option<RelationName>,
    pub if_not_exists: bool,
    pub columns: Vec<ColumnDefinition>,
    pub constraints: Vec<Constraint>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ColumnDefinition {
    pub name: String,
    pub data_type: DataType,
    pub default: DefaultExpr,
    pub not_null: bool,
    pub constraints: Vec<Constraint>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct DataType {
    /// Unqualified or qualified name as represented by PostgreSQL's parser.
    pub name: Vec<String>,
    pub integer_modifiers: Vec<Option<i64>>,
    pub array_dimensions: usize,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum DefaultExpr {
    Absent,
    Constant,
    Stable,
    Volatile,
    Unknown,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct AlterTable {
    pub relation: RelationName,
    pub if_exists: bool,
    pub only: bool,
    pub actions: Vec<AlterAction>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct AlterAction {
    pub span: SourceSpan,
    pub change: AlterChange,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum AlterChange {
    AddColumn {
        column: ColumnDefinition,
        if_not_exists: bool,
    },
    DropColumn {
        name: String,
        if_exists: bool,
        cascade: bool,
    },
    SetNotNull {
        column: String,
    },
    DropNotNull {
        column: String,
    },
    SetDefault {
        column: String,
        default: DefaultExpr,
    },
    ChangeType {
        column: String,
        data_type: DataType,
        using_expression: bool,
    },
    AddConstraint(Constraint),
    DropConstraint {
        name: String,
        if_exists: bool,
        cascade: bool,
    },
    ValidateConstraint {
        name: String,
    },
    Unsupported,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Constraint {
    pub name: Option<String>,
    pub columns: Vec<String>,
    pub kind: ConstraintKind,
    pub not_valid: bool,
    pub enforced: bool,
    pub no_inherit: bool,
    pub deferrable: bool,
    pub initially_deferred: bool,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ConstraintKind {
    Check {
        proves_not_null: Vec<String>,
    },
    ForeignKey {
        referenced_table: RelationName,
        referenced_columns: Vec<String>,
    },
    NotNull,
    PrimaryKey,
    Unique,
    Unsupported,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Rename {
    pub action_span: SourceSpan,
    /// For a member, this identifies its parent relation.
    pub object: RelationName,
    pub kind: ObjectKind,
    pub member: Option<String>,
    pub new_name: String,
    pub if_exists: bool,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct DropObjects {
    pub kind: ObjectKind,
    pub objects: Vec<RelationName>,
    pub if_exists: bool,
    pub cascade: bool,
    pub concurrently: bool,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Truncate {
    pub relations: Vec<RelationName>,
    pub only: Vec<bool>,
    pub restart_identity: bool,
    pub cascade: bool,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CreateIndex {
    pub relation: RelationName,
    pub name: Option<String>,
    pub keys: Vec<IndexKey>,
    pub unique: bool,
    pub concurrently: bool,
    pub if_not_exists: bool,
    pub predicate: Predicate,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum IndexKey {
    Column(String),
    Expression,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct DataChange {
    pub relation: RelationName,
    pub predicate: Predicate,
    /// UPDATE FROM or DELETE USING can affect the number of changed rows.
    pub additional_relations: bool,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Predicate {
    Absent,
    AlwaysTrue,
    AlwaysFalse,
    Filtered,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum Transaction {
    Begin,
    Commit { chain: bool },
    Rollback { chain: bool },
    Savepoint(String),
    RollbackTo(String),
    Release(String),
}
