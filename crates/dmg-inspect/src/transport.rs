use std::{fmt, future::poll_fn, ops::AsyncFnOnce, time::Duration};

use dmg_core::inspection::InspectionGapReason;
use rustls::{
    ClientConfig, RootCertStore,
    pki_types::{CertificateDer, ServerName, pem::PemObject},
};
use thiserror::Error;
use tokio::time::timeout;
use tokio_postgres::{Config, IsolationLevel, Transaction, config::SslMode};
use tokio_postgres_rustls::MakeRustlsConnect;
use zeroize::Zeroizing;

const MAX_CA_BYTES: usize = 256 * 1024;
const MAX_CA_CERTIFICATES: usize = 32;
const MAX_TIMEOUT_MS: u32 = 30_000;
const APPLICATION_NAME: &str = "dmg-inspect";
const SERVER_INFO: &str = "
    SELECT pg_catalog.current_setting('server_version_num')::pg_catalog.int4,
           pg_catalog.current_setting('block_size')::pg_catalog.int4,
           pg_catalog.current_setting('transaction_read_only') = 'on',
           pg_catalog.current_setting('transaction_isolation') = $1,
           pg_catalog.current_schemas(true) = ARRAY['pg_catalog']::pg_catalog.name[]
";

/// One explicitly authorized TCP endpoint and its trust anchors.
pub struct InspectionTarget {
    host: String,
    port: u16,
    database: String,
    user: String,
    password: Zeroizing<Vec<u8>>,
    tls: ClientConfig,
}

impl InspectionTarget {
    /// Trust only the supplied PEM CA bundle and verify the host as a DNS name or IP.
    ///
    /// This API accepts individual fields from protected configuration. It does not
    /// consume a connection string or libpq environment settings.
    pub fn new(
        host: &str,
        port: u16,
        database: &str,
        user: &str,
        password: Vec<u8>,
        ca_pem: &[u8],
    ) -> Result<Self, TransportError> {
        let password = Zeroizing::new(password);
        if host.len() > 255
            || ServerName::try_from(host).is_err()
            || port == 0
            || !valid_startup_name(database)
            || !valid_startup_name(user)
            || password.is_empty()
            || password.len() > 4096
        {
            return Err(TransportError::InvalidTarget);
        }

        Ok(Self {
            host: host.into(),
            port,
            database: database.into(),
            user: user.into(),
            password,
            tls: verified_tls(ca_pem)?,
        })
    }

    /// Check TLS, login, and the fixed read-only session settings and return server metadata.
    pub async fn probe(&self, timeouts: InspectionTimeouts) -> Result<ServerInfo, TransportError> {
        self.with_read_only(timeouts, async |_, server| Ok(server))
            .await
    }

    /// Drive a bounded read-only transaction for the crate's fixed query projections.
    pub(crate) async fn with_read_only<T>(
        &self,
        timeouts: InspectionTimeouts,
        read: impl for<'tx, 'client> AsyncFnOnce(
            &'tx mut Transaction<'client>,
            ServerInfo,
        ) -> Result<T, TransportError>,
    ) -> Result<T, TransportError> {
        self.with_read_only_at(InspectionIsolation::Catalog, timeouts, read)
            .await
    }

    pub(crate) async fn with_read_only_at<T>(
        &self,
        isolation: InspectionIsolation,
        timeouts: InspectionTimeouts,
        read: impl for<'tx, 'client> AsyncFnOnce(
            &'tx mut Transaction<'client>,
            ServerInfo,
        ) -> Result<T, TransportError>,
    ) -> Result<T, TransportError> {
        timeout(
            timeouts.overall,
            self.run_read_only(isolation, timeouts, read),
        )
        .await
        .map_err(|_| TransportError::Deadline)?
    }

    async fn run_read_only<T>(
        &self,
        isolation: InspectionIsolation,
        timeouts: InspectionTimeouts,
        read: impl for<'tx, 'client> AsyncFnOnce(
            &'tx mut Transaction<'client>,
            ServerInfo,
        ) -> Result<T, TransportError>,
    ) -> Result<T, TransportError> {
        let mut config = Config::new();
        config
            .host(&self.host)
            .port(self.port)
            .dbname(&self.database)
            .user(&self.user)
            .password(self.password.as_slice())
            .application_name(APPLICATION_NAME)
            .ssl_mode(SslMode::Require)
            .connect_timeout(timeouts.overall)
            .options(timeouts.startup_options());

        let (mut client, mut connection) = config
            .connect(MakeRustlsConnect::new(self.tls.clone()))
            .await
            .map_err(|error| TransportError::postgres(TransportStage::Connect, &error))?;
        drop(config);

        let transaction = async {
            let mut tx = client
                .build_transaction()
                .isolation_level(isolation.level())
                .read_only(true)
                .start()
                .await
                .map_err(|error| TransportError::postgres(TransportStage::Setup, &error))?;
            let result = match server_info(&tx, isolation).await {
                Ok(server) => read(&mut tx, server).await,
                Err(error) => Err(error),
            };
            let rollback = tx
                .rollback()
                .await
                .map_err(|error| TransportError::postgres(TransportStage::Rollback, &error));
            result.and_then(|value| rollback.map(|()| value))
        };

        // Poll the connection inline so abandoning the operation closes the
        // socket. Polling messages explicitly discards server notices.
        let driver = async {
            while let Some(message) = poll_fn(|cx| connection.poll_message(cx)).await {
                message.map_err(|error| TransportError::postgres(TransportStage::Query, &error))?;
            }
            Err(TransportError::Postgres {
                stage: TransportStage::Query,
                sqlstate: None,
            })
        };

        tokio::select! {
            biased;
            result = transaction => result,
            result = driver => result,
        }
    }
}

impl fmt::Debug for InspectionTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InspectionTarget")
            .finish_non_exhaustive()
    }
}

fn valid_startup_name(value: &str) -> bool {
    !value.is_empty() && value.len() <= 1024 && !value.contains('\0')
}

fn verified_tls(ca_pem: &[u8]) -> Result<ClientConfig, TransportError> {
    if ca_pem.is_empty() || ca_pem.len() > MAX_CA_BYTES {
        return Err(TransportError::InvalidCa);
    }

    let mut roots = RootCertStore::empty();
    for (index, certificate) in CertificateDer::pem_slice_iter(ca_pem).enumerate() {
        if index >= MAX_CA_CERTIFICATES {
            return Err(TransportError::InvalidCa);
        }
        roots
            .add(certificate.map_err(|_| TransportError::InvalidCa)?)
            .map_err(|_| TransportError::InvalidCa)?;
    }
    if roots.is_empty() {
        return Err(TransportError::InvalidCa);
    }

    Ok(
        ClientConfig::builder_with_provider(rustls::crypto::ring::default_provider().into())
            .with_safe_default_protocol_versions()
            .map_err(|_| TransportError::InvalidCa)?
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

/// Client deadline and PostgreSQL session timeouts. All durations are positive and bounded.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct InspectionTimeouts {
    overall: Duration,
    statement_ms: u32,
    lock_ms: u32,
    idle_transaction_ms: u32,
}

impl InspectionTimeouts {
    pub fn new(
        overall_ms: u32,
        statement_ms: u32,
        lock_ms: u32,
        idle_transaction_ms: u32,
    ) -> Result<Self, TransportError> {
        if [overall_ms, statement_ms, lock_ms, idle_transaction_ms]
            .into_iter()
            .any(|value| !(1..=MAX_TIMEOUT_MS).contains(&value))
        {
            return Err(TransportError::InvalidTimeouts);
        }

        Ok(Self {
            overall: Duration::from_millis(u64::from(overall_ms)),
            statement_ms,
            lock_ms,
            idle_transaction_ms,
        })
    }

    fn startup_options(self) -> String {
        format!(
            "-c default_transaction_read_only=on -c search_path=pg_catalog,pg_temp \
             -c statement_timeout={}ms -c lock_timeout={}ms \
             -c idle_in_transaction_session_timeout={}ms",
            self.statement_ms, self.lock_ms, self.idle_transaction_ms,
        )
    }
}

impl Default for InspectionTimeouts {
    fn default() -> Self {
        Self {
            overall: Duration::from_secs(5),
            statement_ms: 1000,
            lock_ms: 100,
            idle_transaction_ms: 3000,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct ServerInfo {
    pub server_version_num: u32,
    pub block_size: u32,
}

#[derive(Clone, Copy)]
pub(crate) enum InspectionIsolation {
    Catalog,
    History,
}

impl InspectionIsolation {
    fn level(self) -> IsolationLevel {
        match self {
            Self::Catalog => IsolationLevel::RepeatableRead,
            Self::History => IsolationLevel::ReadCommitted,
        }
    }

    fn setting(self) -> &'static str {
        match self {
            Self::Catalog => "repeatable read",
            Self::History => "read committed",
        }
    }
}

pub(crate) async fn server_info(
    tx: &Transaction<'_>,
    isolation: InspectionIsolation,
) -> Result<ServerInfo, TransportError> {
    let row = tx
        .query_one(SERVER_INFO, &[&isolation.setting()])
        .await
        .map_err(|error| TransportError::postgres(TransportStage::Setup, &error))?;
    let positive = |index| {
        row.try_get::<_, i32>(index)
            .ok()
            .and_then(|value| u32::try_from(value).ok())
            .filter(|&value| value > 0)
            .ok_or(TransportError::InvalidSession)
    };
    for index in 2..5 {
        if !matches!(row.try_get::<_, bool>(index), Ok(true)) {
            return Err(TransportError::InvalidSession);
        }
    }
    Ok(ServerInfo {
        server_version_num: positive(0_usize)?,
        block_size: positive(1_usize)?,
    })
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum TransportStage {
    Connect,
    Setup,
    Query,
    Rollback,
}

impl fmt::Display for TransportStage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Connect => "connection setup",
            Self::Setup => "read-only session setup",
            Self::Query => "query",
            Self::Rollback => "rollback",
        };
        formatter.write_str(label)
    }
}

/// Redacted inspection failures. Driver messages can include protected input.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum TransportError {
    #[error("invalid PostgreSQL inspection target fields")]
    InvalidTarget,
    #[error("invalid or oversized PostgreSQL inspection CA bundle")]
    InvalidCa,
    #[error("PostgreSQL inspection timeouts must be 1 to 30000 milliseconds")]
    InvalidTimeouts,
    #[error("PostgreSQL inspection reached its client deadline")]
    Deadline,
    #[error("PostgreSQL inspection session did not provide verified settings and server metadata")]
    InvalidSession,
    #[error("PostgreSQL inspection failed during {stage}")]
    Postgres {
        stage: TransportStage,
        sqlstate: Option<String>,
    },
}

impl TransportError {
    /// Return the SQLSTATE only when the driver supplied a valid five-character code.
    pub fn sqlstate(&self) -> Option<&str> {
        match self {
            Self::Postgres { sqlstate, .. } => sqlstate.as_deref(),
            _ => None,
        }
    }

    pub(crate) fn gap_reason(&self) -> InspectionGapReason {
        use InspectionGapReason as Reason;

        match self {
            Self::Deadline => Reason::Deadline,
            Self::InvalidSession => Reason::InvalidValue,
            _ => match self.sqlstate() {
                Some("28000" | "28P01" | "42501") => Reason::PermissionDenied,
                Some("55P03") => Reason::Contention,
                Some("57014") => Reason::Cancelled,
                _ => Reason::QueryFailed,
            },
        }
    }

    pub(crate) fn postgres(stage: TransportStage, error: &tokio_postgres::Error) -> Self {
        let sqlstate = error
            .code()
            .map(tokio_postgres::error::SqlState::code)
            .filter(|code| {
                code.len() == 5
                    && code
                        .bytes()
                        .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
            })
            .map(str::to_owned);
        Self::Postgres { stage, sqlstate }
    }
}

#[cfg(test)]
mod tests;
