//! Disposable PostgreSQL workers for authorized simulation.
#![forbid(unsafe_code)]

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use dmg_core::inspection::InspectionScope;
use thiserror::Error;

mod rehearsal;

pub use dmg_core::rehearsal::{RehearsalBindingV1, RehearsalOutcome, RehearsalReportV1, SeedKind};
pub use rehearsal::{
    MAX_REHEARSAL_SETUP_BYTES, PreparedRehearsal, RehearsalContext, RehearsalInput,
    RehearsalPlanError, expected_rehearsal_plan, prepare_rehearsal, run_rehearsal,
};

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod linux;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub use linux::{DockerWorker, WorkerStream};

/// Approved linux/amd64 variants of the PostgreSQL Docker Official Image.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum PostgresImage {
    V15_20,
    V16_16,
    V17_12,
    V18_7,
}

impl PostgresImage {
    pub const fn server_version_num(self) -> u32 {
        match self {
            Self::V15_20 => 150020,
            Self::V16_16 => 160016,
            Self::V17_12 => 170012,
            Self::V18_7 => 180007,
        }
    }

    /// Registry manifest digest of the exact platform image.
    pub const fn manifest_digest(self) -> &'static str {
        match self {
            Self::V15_20 => {
                "sha256:675e329d71b3d00534e50356ad068fa95d7bdd9c68f498f17a016ae19990d85e"
            }
            Self::V16_16 => {
                "sha256:a2f7e20374cd8fd8ed4a588b79549e10589bea7b1686e77922f5ac741b80b8a1"
            }
            Self::V17_12 => {
                "sha256:005c5c37db96c1e6652a79ea31f9886402fba81ac95dc2db897739121dd7f8f2"
            }
            Self::V18_7 => {
                "sha256:fa1b61f5a4844f4d4b73c9b15716cfc75fe70c931f2d32723630f3b900056366"
            }
        }
    }

    /// Docker image ID from the immutable image config linked by the manifest.
    pub const fn config_id(self) -> &'static str {
        match self {
            Self::V15_20 => {
                "sha256:24084862c6bcb8a9b348892f7135c1099cb581dbca4d046aec95734717714b18"
            }
            Self::V16_16 => {
                "sha256:3cbd775daa28461ef484ed88ca1e5a5a72ff126e196deeebce6fec764edf1200"
            }
            Self::V17_12 => {
                "sha256:6b75136145ddabf3d9a4b69660402082de7c56b734cfe50555b0674774dd742a"
            }
            Self::V18_7 => {
                "sha256:0f6c51e48b3458e87ab43692e49c9dafc72ffe0a0dbde3fd8cdb6ab92d5c025e"
            }
        }
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    const fn volume(self) -> &'static str {
        match self {
            Self::V18_7 => "/var/lib/postgresql",
            _ => "/var/lib/postgresql/data",
        }
    }
}

/// Time limits for the worker. Container resource limits are fixed.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct WorkerLimits {
    startup: Duration,
    operation: Duration,
    statement_ms: u32,
    lock_ms: u32,
    cleanup: Duration,
}

impl WorkerLimits {
    pub const MEMORY_BYTES: u64 = 192 * 1024 * 1024;
    pub const DATA_BYTES: u64 = 128 * 1024 * 1024;
    pub const PIDS: u64 = 36;
    pub const NANO_CPUS: u64 = 1_000_000_000;

    pub fn new(
        startup_ms: u32,
        operation_ms: u32,
        statement_ms: u32,
        lock_ms: u32,
        cleanup_ms: u32,
    ) -> Result<Self, WorkerError> {
        if !(1..=120_000).contains(&startup_ms)
            || !(1..=300_000).contains(&operation_ms)
            || !(1..=60_000).contains(&statement_ms)
            || !(1..=60_000).contains(&lock_ms)
            || !(1..=30_000).contains(&cleanup_ms)
        {
            return Err(WorkerError::InvalidLimits);
        }
        Ok(Self {
            startup: Duration::from_millis(startup_ms.into()),
            operation: Duration::from_millis(operation_ms.into()),
            statement_ms,
            lock_ms,
            cleanup: Duration::from_millis(cleanup_ms.into()),
        })
    }

    pub const fn operation_timeout(self) -> Duration {
        self.operation
    }
}

impl Default for WorkerLimits {
    fn default() -> Self {
        Self {
            startup: Duration::from_secs(30),
            operation: Duration::from_secs(60),
            statement_ms: 10_000,
            lock_ms: 1_000,
            cleanup: Duration::from_secs(10),
        }
    }
}

/// Protected runtime settings. Paths never appear in worker diagnostics.
#[cfg_attr(
    not(all(target_os = "linux", target_arch = "x86_64")),
    allow(dead_code)
)]
pub struct DockerWorkerConfig {
    executable: PathBuf,
    socket: PathBuf,
    image: PostgresImage,
    limits: WorkerLimits,
    schemas: Option<InspectionScope>,
}

impl DockerWorkerConfig {
    pub fn new(
        executable: impl AsRef<Path>,
        unix_socket: impl AsRef<Path>,
        image: PostgresImage,
        limits: WorkerLimits,
    ) -> Result<Self, WorkerError> {
        let (executable, socket) = (executable.as_ref(), unix_socket.as_ref());
        if !valid_local_path(executable) || !valid_local_path(socket) {
            return Err(WorkerError::InvalidEndpoint);
        }
        Ok(Self {
            executable: executable.into(),
            socket: socket.into(),
            image,
            limits,
            schemas: None,
        })
    }

    /// Provision these schemas as migration-role owned. The default is only `public`.
    pub fn with_schemas(mut self, schemas: InspectionScope) -> Self {
        self.schemas = Some(schemas);
        self
    }
}

fn valid_local_path(path: &Path) -> bool {
    // These are Linux runtime paths even when the plan is reviewed on another OS.
    let bytes = path.as_os_str().as_encoded_bytes();
    bytes
        .strip_prefix(b"/")
        .is_some_and(|rest| !rest.is_empty() && !rest.starts_with(b"/"))
        && !bytes
            .iter()
            .any(|&byte| byte.is_ascii_control() || matches!(byte, b'\\' | b':'))
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum WorkerStage {
    Runtime,
    Image,
    Create,
    VerifyControls,
    Start,
    Ready,
    Initialize,
    Connect,
    Execute,
    Cleanup,
}

#[derive(Debug, Error)]
pub enum WorkerError {
    #[error("disposable PostgreSQL workers require a Linux x86-64 host and Docker Engine")]
    UnsupportedPlatform,
    #[error("worker requires an explicit absolute Docker executable and local Unix socket")]
    InvalidEndpoint,
    #[error("worker time limits are outside the supported range")]
    InvalidLimits,
    #[error("could not allocate the worker's private process state")]
    LocalSetup,
    #[error("the worker runtime could not establish required isolation controls")]
    UnsupportedRuntime,
    #[error("could not query the explicitly configured local Docker runtime")]
    RuntimeUnavailable,
    #[error("the approved PostgreSQL image is unavailable or does not match its pin")]
    ImageUnavailable,
    #[error("worker command failed at {0:?}")]
    Command(WorkerStage),
    #[error("worker command output exceeded its limit at {0:?}")]
    OutputLimit(WorkerStage),
    #[error("worker deadline expired at {0:?}")]
    Deadline(WorkerStage),
    #[error("worker PostgreSQL version or migration role settings did not match")]
    InvalidSession,
    #[error("worker PostgreSQL operation failed at {stage:?} (SQLSTATE {sqlstate:?})")]
    Postgres {
        stage: WorkerStage,
        sqlstate: Option<String>,
    },
    #[error("could not confirm removal of disposable worker {worker}")]
    CleanupUnconfirmed {
        worker: String,
        #[source]
        prior: Option<Box<WorkerError>>,
    },
}

impl WorkerError {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn with_prior(self, prior: WorkerError) -> Self {
        match self {
            Self::CleanupUnconfirmed { worker, .. } => Self::CleanupUnconfirmed {
                worker,
                prior: Some(Box::new(prior)),
            },
            other => other,
        }
    }
}

#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
pub struct DockerWorker {
    _private: (),
}

#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
impl DockerWorker {
    pub async fn start(_config: DockerWorkerConfig) -> Result<Self, WorkerError> {
        Err(WorkerError::UnsupportedPlatform)
    }

    pub async fn run<T>(
        _config: DockerWorkerConfig,
        _operation: impl AsyncFnOnce(&DockerWorker) -> Result<T, WorkerError>,
    ) -> Result<T, WorkerError> {
        Err(WorkerError::UnsupportedPlatform)
    }

    pub async fn close(self) -> Result<(), WorkerError> {
        Err(WorkerError::UnsupportedPlatform)
    }
}
