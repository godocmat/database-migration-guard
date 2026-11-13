use dmg_core::{
    history::{NativeChecksum, NativeHistoryRow, NativeHistoryState, Sha384Digest},
    migration::{MigrationAdapter, MigrationDirection, MigrationIdentity},
};
use tokio_postgres::{
    Row,
    types::{FromSqlOwned, Type},
};

use super::Reason;

const MAX_VERSION_BYTES: i32 = 1024;
const MAX_FLYWAY_TYPE_BYTES: i32 = 80;
const SHA384_BYTES: usize = 48;

#[derive(Clone, Copy)]
pub(super) struct HistoryProfile {
    adapter: MigrationAdapter,
    pub(super) key: &'static str,
    pub(super) columns: &'static [(&'static str, Type, bool)],
}

impl HistoryProfile {
    pub(super) fn for_adapter(adapter: MigrationAdapter) -> Self {
        let (key, columns): (_, &[_]) = match adapter {
            MigrationAdapter::Sqlx => (
                "version",
                &[
                    ("version", Type::INT8, true),
                    ("success", Type::BOOL, true),
                    ("checksum", Type::BYTEA, true),
                ][..],
            ),
            MigrationAdapter::Flyway => (
                "installed_rank",
                &[
                    ("installed_rank", Type::INT4, true),
                    ("version", Type::VARCHAR, false),
                    ("description", Type::VARCHAR, true),
                    ("type", Type::VARCHAR, true),
                    ("success", Type::BOOL, true),
                    ("checksum", Type::INT4, false),
                ][..],
            ),
            MigrationAdapter::Goose => (
                "id",
                &[
                    ("id", Type::INT4, true),
                    ("version_id", Type::INT8, true),
                    ("is_applied", Type::BOOL, true),
                ][..],
            ),
            MigrationAdapter::Supabase => ("version", &[("version", Type::TEXT, true)][..]),
            MigrationAdapter::Plain => unreachable!("history options reject plain SQL"),
        };
        Self {
            adapter,
            key,
            columns,
        }
    }

    pub(super) fn query(self, source: &str) -> String {
        let (projection, order) = match self.adapter {
            MigrationAdapter::Sqlx => (
                format!(
                    "version, success, CASE WHEN pg_catalog.octet_length(checksum) = {SHA384_BYTES}
                     THEN checksum END"
                ),
                "version",
            ),
            MigrationAdapter::Flyway => (
                format!(
                    "installed_rank, success, checksum,
                     pg_catalog.octet_length(version),
                     CASE WHEN pg_catalog.octet_length(version) <= {MAX_VERSION_BYTES}
                          THEN version END,
                     CASE WHEN version IS NULL AND pg_catalog.octet_length(description) <= {MAX_VERSION_BYTES}
                          THEN description END,
                     CASE WHEN pg_catalog.octet_length(type) <= {MAX_FLYWAY_TYPE_BYTES}
                          THEN type END"
                ),
                "installed_rank",
            ),
            MigrationAdapter::Goose => ("id, version_id, is_applied".into(), "id"),
            MigrationAdapter::Supabase => (
                format!(
                    "CASE WHEN pg_catalog.octet_length(version) <= {MAX_VERSION_BYTES} THEN version END"
                ),
                "version COLLATE pg_catalog.\"C\"",
            ),
            MigrationAdapter::Plain => unreachable!("history options reject plain SQL"),
        };
        format!("SELECT {projection} FROM ONLY {source} ORDER BY {order} LIMIT $1::pg_catalog.int8")
    }

    pub(super) fn event(self, row: &Row) -> Result<RowEvent, Reason> {
        let event = match self.adapter {
            MigrationAdapter::Sqlx => {
                let mut event = migration(MigrationIdentity::numeric(
                    MigrationAdapter::Sqlx,
                    value(row, 0)?,
                ));
                event.state = success(value(row, 1)?);
                event.checksum = value::<Option<Vec<u8>>>(row, 2)?
                    .and_then(|bytes| <[u8; SHA384_BYTES]>::try_from(bytes).ok())
                    .map(|bytes| NativeChecksum::SqlxSha384 {
                        digest: Sha384Digest::from_bytes(bytes),
                    });
                event
            }
            MigrationAdapter::Flyway => flyway_event(row)?,
            MigrationAdapter::Goose => {
                let rank = positive_rank(value(row, 0)?)?;
                let version = value::<i64>(row, 1)?;
                let applied = value::<bool>(row, 2)?;
                if version == 0 && applied {
                    return Ok(RowEvent::Bootstrap);
                }
                if version <= 0 {
                    return Err(Reason::InvalidValue);
                }
                let mut event =
                    migration(MigrationIdentity::numeric(MigrationAdapter::Goose, version));
                event.rank = Some(rank);
                event.state = if applied {
                    NativeHistoryState::Applied
                } else {
                    NativeHistoryState::Unknown
                };
                event
            }
            MigrationAdapter::Supabase => {
                let version: String = value::<Option<_>>(row, 0)?.ok_or(Reason::Truncated)?;
                migration(
                    MigrationIdentity::supabase_version(&version)
                        .map_err(|_| Reason::InvalidValue)?,
                )
            }
            MigrationAdapter::Plain => unreachable!("history options reject plain SQL"),
        };
        Ok(RowEvent::Migration(event))
    }
}

pub(super) enum RowEvent {
    Bootstrap,
    Migration(NativeHistoryRow),
}

fn flyway_event(row: &Row) -> Result<NativeHistoryRow, Reason> {
    let kind: String = value::<Option<_>>(row, 6)?.ok_or(Reason::Truncated)?;
    let direction = match kind.as_str() {
        "SQL" => MigrationDirection::Forward,
        "UNDO_SQL" => MigrationDirection::Down,
        _ => return Err(Reason::Unsupported),
    };
    let version_length: Option<i32> = value(row, 3)?;
    let identity = if version_length.is_some() {
        let version: String = value::<Option<_>>(row, 4)?.ok_or(Reason::Truncated)?;
        MigrationIdentity::flyway_version(&version)
    } else if direction == MigrationDirection::Forward {
        let description: String = value::<Option<_>>(row, 5)?.ok_or(Reason::Truncated)?;
        MigrationIdentity::flyway_repeatable(&description)
    } else {
        return Err(Reason::InvalidValue);
    }
    .map_err(|_| Reason::InvalidValue)?;

    Ok(NativeHistoryRow {
        identity,
        direction,
        rank: Some(positive_rank(value(row, 0)?)?),
        state: success(value(row, 1)?),
        checksum: value::<Option<i32>>(row, 2)?.map(|value| NativeChecksum::FlywayCrc32 { value }),
    })
}

fn migration(identity: MigrationIdentity) -> NativeHistoryRow {
    NativeHistoryRow {
        identity,
        direction: MigrationDirection::Forward,
        rank: None,
        state: NativeHistoryState::Applied,
        checksum: None,
    }
}

fn positive_rank(rank: i32) -> Result<u64, Reason> {
    u64::try_from(rank)
        .ok()
        .filter(|rank| *rank > 0)
        .ok_or(Reason::InvalidValue)
}

fn success(success: bool) -> NativeHistoryState {
    if success {
        NativeHistoryState::Applied
    } else {
        NativeHistoryState::Failed
    }
}

pub(super) fn value<T: FromSqlOwned>(row: &Row, index: usize) -> Result<T, Reason> {
    row.try_get(index).map_err(|_| Reason::InvalidValue)
}
