use std::{
    collections::BTreeSet,
    fs::File,
    io::{self, Read},
    path::Path,
};

use dmg_core::{source::RepoPath, target::PostgresTarget};
use dmg_discovery::{
    Adapter, DiscoveryLimits, DiscoveryOptions, PlainSql, PlainTransaction, SqlxLayout,
    VersionOrder,
};
use dmg_rules::RuleOptions;
use serde::Deserialize;
use thiserror::Error;

const MAX_CONFIG_BYTES: u64 = 64 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectFile {
    schema_version: u32,
    target: PostgresTarget,
    roots: Vec<RepoPath>,
    adapter: AdapterFile,
    #[serde(default = "default_search_path")]
    search_path: Vec<String>,
    #[serde(default)]
    exit_codes: ExitCodes,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum AdapterFile {
    Plain {
        filename_pattern: String,
        version_order: VersionOrderFile,
        transaction: PlainTransactionFile,
    },
    Sqlx {
        layout: SqlxLayoutFile,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum VersionOrderFile {
    PositiveInteger,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum PlainTransactionFile {
    File,
    Autocommit,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum SqlxLayoutFile {
    Simple,
    Reversible,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ExitCodes {
    pub(crate) denied: u8,
    pub(crate) incomplete: u8,
}

impl Default for ExitCodes {
    fn default() -> Self {
        Self {
            denied: 1,
            incomplete: 3,
        }
    }
}

impl ExitCodes {
    fn valid(self) -> bool {
        self.denied != self.incomplete
            && [self.denied, self.incomplete]
                .into_iter()
                .all(|code| (1..=125).contains(&code) && code != 2)
    }
}

pub(crate) struct Project {
    pub(crate) target: PostgresTarget,
    pub(crate) discovery: DiscoveryOptions,
    pub(crate) rules: RuleOptions,
    pub(crate) exit_codes: ExitCodes,
}

impl Project {
    pub(crate) fn load(path: &Path) -> Result<Self, ConfigError> {
        let bytes = read_input(path, MAX_CONFIG_BYTES)?;
        let input = std::str::from_utf8(&bytes).map_err(|_| ConfigError::Encoding)?;
        let file: ProjectFile = toml::from_str(input)
            .map_err(|error: toml::de::Error| ConfigError::Format(toml_location(input, &error)))?;
        if file.schema_version != 1 {
            return Err(ConfigError::Version);
        }
        if !file.exit_codes.valid() {
            return Err(ConfigError::ExitCodes);
        }
        let unique_schemas = file.search_path.iter().collect::<BTreeSet<_>>();
        if file.search_path.is_empty()
            || file.search_path.len() > 32
            || unique_schemas.len() != file.search_path.len()
            || file.search_path.iter().any(|name| {
                name.is_empty()
                    || name.len() > 63
                    || name.chars().any(char::is_control)
                    || matches!(name.as_str(), "$user" | "pg_temp")
            })
        {
            return Err(ConfigError::SearchPath);
        }
        let adapter = match file.adapter {
            AdapterFile::Plain {
                filename_pattern,
                version_order,
                transaction,
            } => {
                let version_order = match version_order {
                    VersionOrderFile::PositiveInteger => VersionOrder::PositiveInteger,
                };
                let transaction = match transaction {
                    PlainTransactionFile::File => PlainTransaction::File,
                    PlainTransactionFile::Autocommit => PlainTransaction::Autocommit,
                };
                Adapter::Plain(PlainSql::new(
                    &filename_pattern,
                    version_order,
                    transaction,
                )?)
            }
            AdapterFile::Sqlx { layout } => Adapter::Sqlx(match layout {
                SqlxLayoutFile::Simple => SqlxLayout::Simple,
                SqlxLayoutFile::Reversible => SqlxLayout::Reversible,
            }),
        };
        Ok(Self {
            target: file.target,
            discovery: DiscoveryOptions::new(file.roots, adapter, DiscoveryLimits::default())?,
            rules: RuleOptions {
                search_path: file.search_path,
            },
            exit_codes: file.exit_codes,
        })
    }
}

fn default_search_path() -> Vec<String> {
    RuleOptions::default().search_path
}

pub(crate) fn template(target: PostgresTarget) -> String {
    format!(
        r#"schema_version = 1
target = {target}
roots = ["migrations"]
search_path = ["public"]

[adapter]
kind = "plain"
filename_pattern = '^([0-9]+)_[a-z][a-z0-9_]*\.sql$'
version_order = "positive_integer"
transaction = "file"
"#,
    )
}

pub(crate) fn toml_location(input: &str, error: &toml::de::Error) -> String {
    error.span().map_or_else(String::new, |span| {
        let prefix = &input.as_bytes()[..span.start.min(input.len())];
        let line = prefix.iter().filter(|&&byte| byte == b'\n').count() + 1;
        let column = prefix
            .iter()
            .rev()
            .take_while(|&&byte| byte != b'\n')
            .count()
            + 1;
        format!(" at line {line}, byte column {column}")
    })
}

pub(crate) fn read_input(path: &Path, limit: u64) -> Result<Vec<u8>, InputError> {
    let metadata = path.metadata().map_err(InputError::from)?;
    if !metadata.is_file() {
        return Err(InputError::NotFile);
    }
    if metadata.len() > limit {
        return Err(InputError::Limit);
    }
    let file = File::open(path).map_err(InputError::from)?;
    if !file.metadata().map_err(InputError::from)?.is_file() {
        return Err(InputError::NotFile);
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(InputError::from)?;
    if bytes.len() as u64 > limit {
        return Err(InputError::Limit);
    }
    Ok(bytes)
}

#[derive(Debug, Error)]
pub(crate) enum InputError {
    #[error("could not read the input file ({0})")]
    Read(io::ErrorKind),
    #[error("input must be a regular file")]
    NotFile,
    #[error("input exceeds the file size limit")]
    Limit,
}

impl From<io::Error> for InputError {
    fn from(error: io::Error) -> Self {
        Self::Read(error.kind())
    }
}

#[derive(Debug, Error)]
pub(crate) enum ConfigError {
    #[error(transparent)]
    Input(#[from] InputError),
    #[error("project configuration must be UTF-8")]
    Encoding,
    #[error("invalid project configuration TOML{0}. Check the documented fields and value types")]
    Format(String),
    #[error("project configuration requires schema_version = 1")]
    Version,
    #[error("exit codes must be distinct integers from 1 through 125 and exclude 2")]
    ExitCodes,
    #[error("search_path must contain 1 to 32 distinct static PostgreSQL schema names")]
    SearchPath,
    #[error(transparent)]
    Discovery(#[from] dmg_discovery::DiscoveryConfigError),
}
