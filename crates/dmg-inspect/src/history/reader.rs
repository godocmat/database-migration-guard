use tokio_postgres::{Row, Transaction};

use super::{
    HistoryOptions, HistoryProfile, Reason, TransportError, TransportStage, profile::value,
};

const SOURCE: &str = "
SELECT n.oid, pg_catalog.has_schema_privilege(n.oid, 'USAGE'), c.oid,
       COALESCE(c.relkind = 'r' AND c.relpersistence <> 't' AND NOT c.relispartition
                AND NOT c.relhasrules AND c.relam = am.oid, false),
       COALESCE(c.relrowsecurity, false)
FROM pg_catalog.pg_namespace n
LEFT JOIN pg_catalog.pg_class c
       ON c.relnamespace = n.oid AND c.relname::pg_catalog.text COLLATE pg_catalog.\"C\" = $2
LEFT JOIN pg_catalog.pg_am am ON am.amname = 'heap'
WHERE n.nspname::pg_catalog.text COLLATE pg_catalog.\"C\" = $1";

const LOCK_SCHEMA: &str = "
SELECT objid FROM pg_catalog.pg_get_object_address(
    'schema', ARRAY[$1::pg_catalog.text], ARRAY[]::pg_catalog.text[]
)";

const COLUMNS: &str = "
SELECT a.attname::pg_catalog.text, a.atttypid, a.attnotnull,
       a.attgenerated = '',
       pg_catalog.has_column_privilege(a.attrelid, a.attnum, 'SELECT')
FROM pg_catalog.pg_attribute a
WHERE a.attrelid = $1 AND a.attnum > 0 AND NOT a.attisdropped
  AND a.attname::pg_catalog.text COLLATE pg_catalog.\"C\" = ANY($2::pg_catalog.text[])";

const PRIMARY_KEY: &str = "
SELECT EXISTS (
    SELECT 1 FROM pg_catalog.pg_index i
    JOIN pg_catalog.pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = i.indkey[0]
    WHERE i.indrelid = $1 AND a.attname::pg_catalog.text COLLATE pg_catalog.\"C\" = $2
      AND i.indisprimary AND i.indisunique AND NOT i.indisexclusion AND i.indisvalid AND i.indislive AND i.indisready
      AND i.indnkeyatts = 1 AND i.indpred IS NULL AND i.indexprs IS NULL
)";

pub(super) enum ReadError {
    Gap(Reason),
    Postgres(tokio_postgres::Error),
}

impl ReadError {
    pub(super) fn reason(&self) -> Reason {
        match self {
            Self::Gap(reason) => *reason,
            Self::Postgres(error) => {
                match error.code().map(tokio_postgres::error::SqlState::code) {
                    Some("42P01" | "3F000") => Reason::Missing,
                    Some("42703" | "42804") => Reason::InvalidValue,
                    Some("42809") => Reason::Unsupported,
                    _ => TransportError::postgres(TransportStage::Query, error).gap_reason(),
                }
            }
        }
    }
}

impl From<Reason> for ReadError {
    fn from(reason: Reason) -> Self {
        Self::Gap(reason)
    }
}

impl From<tokio_postgres::Error> for ReadError {
    fn from(error: tokio_postgres::Error) -> Self {
        Self::Postgres(error)
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct Source {
    namespace: u32,
    relation: u32,
}

pub(super) async fn read(
    tx: &Transaction<'_>,
    options: &HistoryOptions,
    profile: HistoryProfile,
) -> Result<Vec<Row>, ReadError> {
    let expected = source(tx, options).await?;
    let namespace: u32 = value(
        &tx.query_one(LOCK_SCHEMA, &[&options.source.schema]).await?,
        0,
    )?;
    if namespace != expected.namespace {
        return Err(Reason::InvalidValue.into());
    }

    // Fail closed if RLS appears between catalog lookup and relation lock acquisition.
    tx.batch_execute("SET LOCAL row_security = off").await?;
    let name = qualified_name(&options.source);
    let marker = tx
        .prepare(&format!("SELECT {} FROM ONLY {name}", profile.key))
        .await?;
    // Parse takes AccessShareLock through the transaction. Its column origin pins the
    // qualified name to the expected OID even when the history table is empty.
    if marker
        .columns()
        .first()
        .and_then(|column| column.table_oid())
        != Some(expected.relation)
        || source(tx, options).await? != expected
    {
        return Err(Reason::InvalidValue.into());
    }

    verify_columns(tx, expected, profile).await?;
    let key_is_primary: bool = value(
        &tx.query_one(PRIMARY_KEY, &[&expected.relation, &profile.key])
            .await?,
        0,
    )?;
    if !key_is_primary {
        return Err(Reason::InvalidValue.into());
    }
    let limit = i64::from(options.row_limit) + 1;
    Ok(tx.query(&profile.query(&name), &[&limit]).await?)
}

async fn source(tx: &Transaction<'_>, options: &HistoryOptions) -> Result<Source, ReadError> {
    let row = tx
        .query_opt(SOURCE, &[&options.source.schema, &options.source.name])
        .await?
        .ok_or(Reason::Missing)?;
    if !value::<bool>(&row, 1)? {
        return Err(Reason::PermissionDenied.into());
    }
    let relation = value::<Option<u32>>(&row, 2)?.ok_or(Reason::Missing)?;
    if !value::<bool>(&row, 3)? {
        return Err(Reason::Unsupported.into());
    }
    if value::<bool>(&row, 4)? {
        return Err(Reason::PermissionDenied.into());
    }
    Ok(Source {
        namespace: value(&row, 0)?,
        relation,
    })
}

async fn verify_columns(
    tx: &Transaction<'_>,
    source: Source,
    profile: HistoryProfile,
) -> Result<(), ReadError> {
    let names: Vec<_> = profile.columns.iter().map(|(name, _, _)| *name).collect();
    let attributes = tx.query(COLUMNS, &[&source.relation, &names]).await?;
    if attributes.len() != profile.columns.len() {
        return Err(Reason::InvalidValue.into());
    }
    for row in &attributes {
        let name: &str = row.try_get(0).map_err(|_| Reason::InvalidValue)?;
        let (_, data_type, require_not_null) = profile
            .columns
            .iter()
            .find(|(expected, _, _)| *expected == name)
            .ok_or(Reason::InvalidValue)?;
        if value::<u32>(row, 1)? != data_type.oid()
            || (*require_not_null && !value::<bool>(row, 2)?)
            || !value::<bool>(row, 3)?
        {
            return Err(Reason::InvalidValue.into());
        }
        if !value::<bool>(row, 4)? {
            return Err(Reason::PermissionDenied.into());
        }
    }
    Ok(())
}

pub(super) fn qualified_name(name: &dmg_core::snapshot::QualifiedName) -> String {
    format!(
        "\"{}\".\"{}\"",
        name.schema.replace('"', "\"\""),
        name.name.replace('"', "\"\""),
    )
}
