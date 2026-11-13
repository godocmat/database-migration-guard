use std::{
    fmt::Write as _,
    future::{Future, poll_fn},
    io,
    marker::PhantomData,
    ops::AsyncFnOnce,
    pin::Pin,
    process::Stdio,
    task::{Context, Poll},
};

use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    process::{Child, ChildStdin, ChildStdout},
    time::{Instant, Sleep, sleep_until, timeout_at},
};
use tokio_postgres::{Client, Config, NoTls, config::SslMode};
use zeroize::Zeroizing;

use super::{DockerWorker, WorkerError, WorkerStage, controls::RUN_AS};

const ROLE: &str = "dmg_migration";
const DATABASE: &str = "postgres";
const PORT: &str = "5432";

const SESSION_CHECK: &str = "
SELECT pg_catalog.current_setting('server_version_num')::pg_catalog.int4,
       current_user = 'dmg_migration' AND session_user = current_user,
       pg_catalog.inet_server_addr() = '127.0.0.1'::pg_catalog.inet,
       (SELECT NOT (rolsuper OR rolcreatedb OR rolcreaterole OR rolinherit OR
                    rolreplication OR rolbypassrls)
        FROM pg_catalog.pg_roles WHERE rolname = current_user),
       NOT EXISTS (SELECT 1 FROM pg_catalog.pg_roles
                   WHERE rolname <> current_user AND pg_catalog.pg_has_role(oid, 'MEMBER')),
       NOT pg_catalog.has_database_privilege(current_database(), 'CREATE'),
       NOT pg_catalog.has_database_privilege(current_database(), 'TEMP')
";

pub(super) fn initialize_sql(password: &str, schemas: Option<&[String]>) -> Zeroizing<String> {
    let mut sql = Zeroizing::new(format!(
        "BEGIN;
         SET LOCAL standard_conforming_strings = on;
         CREATE ROLE {ROLE} LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT
           NOREPLICATION NOBYPASSRLS PASSWORD '{password}';
         REVOKE ALL ON DATABASE {DATABASE} FROM PUBLIC;
         GRANT CONNECT ON DATABASE {DATABASE} TO {ROLE};
         REVOKE ALL ON SCHEMA public FROM PUBLIC;\n"
    ));
    match schemas {
        Some(schemas) => {
            for schema in schemas {
                let quoted = schema.replace('"', "\"\"");
                if schema == "public" {
                    writeln!(&mut *sql, "ALTER SCHEMA public OWNER TO {ROLE};")
                } else {
                    writeln!(
                        &mut *sql,
                        "CREATE SCHEMA \"{quoted}\" AUTHORIZATION {ROLE};"
                    )
                }
                .expect("writing a String cannot fail");
            }
        }
        None => sql.push_str("ALTER SCHEMA public OWNER TO dmg_migration;\n"),
    }
    sql.push_str("COMMIT;\n");
    sql
}

impl DockerWorker {
    /// Builds the fixed startup parameters and ephemeral login for a worker stream.
    /// These parameters are private and must stay out of evidence and logs.
    pub fn connection_config(&self) -> Config {
        let mut config = Config::new();
        config
            .user(ROLE)
            .dbname(DATABASE)
            .password(self.password.as_bytes())
            .ssl_mode(SslMode::Disable)
            .application_name("dmg-simulate")
            .options(format!(
                "-c statement_timeout={}ms -c lock_timeout={}ms -c idle_in_transaction_session_timeout={}ms",
                self.limits.statement_ms,
                self.limits.lock_ms,
                self.limits.statement_ms,
            ));
        config
    }

    /// Opens bounded PostgreSQL protocol I/O over a loopback-only worker bridge.
    /// A client must authenticate with the worker login before sending queries.
    /// The deadline bounds I/O time. Protocol clients bound frames and retained results.
    pub fn connect_stream(&self) -> Result<WorkerStream<'_>, WorkerError> {
        self.stream_with_deadline(self.session_deadline())
    }

    /// Run one operation with a worker-only connection under a wall-clock limit.
    pub async fn with_client<T>(
        &self,
        operation: impl AsyncFnOnce(&mut Client) -> Result<T, WorkerError>,
    ) -> Result<T, WorkerError> {
        self.with_client_until(self.session_deadline(), operation)
            .await
    }

    async fn with_client_until<T>(
        &self,
        deadline: Instant,
        operation: impl AsyncFnOnce(&mut Client) -> Result<T, WorkerError>,
    ) -> Result<T, WorkerError> {
        let config = self.connection_config();
        let stream = self.stream_with_deadline(deadline)?;
        let (mut client, mut connection) = timeout_at(deadline, config.connect_raw(stream, NoTls))
            .await
            .map_err(|_| WorkerError::Deadline(WorkerStage::Connect))?
            .map_err(|error| postgres(WorkerStage::Connect, &error, deadline))?;
        drop(config);

        let driver = async {
            while let Some(message) = poll_fn(|cx| connection.poll_message(cx)).await {
                message.map_err(|error| postgres(WorkerStage::Execute, &error, deadline))?;
            }
            Err(WorkerError::Postgres {
                stage: WorkerStage::Execute,
                sqlstate: None,
            })
        };
        timeout_at(deadline, async {
            tokio::select! {
                biased;
                result = operation(&mut client) => result,
                result = driver => result,
            }
        })
        .await
        .map_err(|_| WorkerError::Deadline(WorkerStage::Execute))?
    }

    pub(super) async fn verify_session(&self, deadline: Instant) -> Result<(), WorkerError> {
        self.with_client_until(deadline, async |client| {
            let row = client
                .query_one(SESSION_CHECK, &[])
                .await
                .map_err(|error| postgres(WorkerStage::Initialize, &error, deadline))?;
            if row.try_get::<_, i32>(0).ok()
                != i32::try_from(self.image().server_version_num()).ok()
                || (1..=6).any(|index| row.try_get::<_, bool>(index).ok() != Some(true))
            {
                return Err(WorkerError::InvalidSession);
            }
            Ok(())
        })
        .await
    }

    fn stream_with_deadline(&self, deadline: Instant) -> Result<WorkerStream<'_>, WorkerError> {
        if Instant::now() >= deadline {
            return Err(WorkerError::Deadline(WorkerStage::Connect));
        }
        let container = self.container();
        let mut command = container.docker.command(&[
            "container",
            "exec",
            "-i",
            "--user",
            RUN_AS,
            container.id(),
            "/bin/busybox",
            "nc",
            "127.0.0.1",
            PORT,
        ]);
        let mut process = command
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|_| WorkerError::Command(WorkerStage::Connect))?;
        let stdin = process
            .stdin
            .take()
            .ok_or(WorkerError::Command(WorkerStage::Connect))?;
        let stdout = process
            .stdout
            .take()
            .ok_or(WorkerError::Command(WorkerStage::Connect))?;
        Ok(WorkerStream {
            _process: process,
            stdin,
            stdout,
            deadline: Box::pin(sleep_until(deadline)),
            worker: PhantomData,
        })
    }
}

fn postgres(stage: WorkerStage, error: &tokio_postgres::Error, deadline: Instant) -> WorkerError {
    if Instant::now() >= deadline {
        return WorkerError::Deadline(stage);
    }
    let sqlstate = error
        .code()
        .map(tokio_postgres::error::SqlState::code)
        .filter(|code| {
            code.len() == 5
                && code
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || byte.is_ascii_uppercase())
        })
        .map(str::to_owned);
    WorkerError::Postgres { stage, sqlstate }
}

/// PostgreSQL bytes over the fixed worker bridge. Dropping it closes the bridge.
pub struct WorkerStream<'worker> {
    _process: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
    deadline: Pin<Box<Sleep>>,
    worker: PhantomData<&'worker DockerWorker>,
}

impl WorkerStream<'_> {
    fn check_deadline(&mut self, cx: &mut Context<'_>) -> io::Result<()> {
        if self.deadline.as_mut().poll(cx).is_ready() {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "worker I/O deadline",
            ))
        } else {
            Ok(())
        }
    }
}

impl AsyncRead for WorkerStream<'_> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.check_deadline(cx)?;
        Pin::new(&mut self.stdout).poll_read(cx, buf)
    }
}

impl AsyncWrite for WorkerStream<'_> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.check_deadline(cx)?;
        Pin::new(&mut self.stdin).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.check_deadline(cx)?;
        Pin::new(&mut self.stdin).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stdin).poll_shutdown(cx)
    }
}
