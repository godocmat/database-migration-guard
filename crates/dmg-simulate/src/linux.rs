mod controls;
mod docker;
mod session;
#[cfg(test)]
mod tests;

use std::{fmt::Write as _, sync::Arc, time::Duration};

use serde::de::DeserializeOwned;
use tokio::{
    task::JoinHandle,
    time::{Instant, sleep},
};
use zeroize::Zeroizing;

use crate::{DockerWorkerConfig, PostgresImage, WorkerError, WorkerLimits, WorkerStage};
use controls::{
    ContainerIdentity, ContainerInfo, CreateSettings, ImageInfo, OWNER_LABEL, RUN_AS, RuntimeInfo,
};
use docker::{Docker, valid_container_id, valid_local_path};

pub use session::WorkerStream;

/// An owned disposable container. Use `close` to obtain teardown confirmation.
pub struct DockerWorker {
    owned: Option<OwnedContainer>,
    password: Zeroizing<String>,
    limits: WorkerLimits,
    operation_deadline: Option<Instant>,
}

impl DockerWorker {
    pub async fn start(config: DockerWorkerConfig) -> Result<Self, WorkerError> {
        if !valid_local_path(config.executable.as_os_str())
            || !valid_local_path(config.socket.as_os_str())
        {
            return Err(WorkerError::InvalidEndpoint);
        }
        let deadline = Instant::now() + config.limits.startup;
        let docker = Arc::new(Docker::new(&config)?);
        verify_runtime(&docker, config.image, deadline).await?;

        let token = random_hex::<16>()?;
        let password = random_hex::<32>()?;
        let name = format!("dmg-worker-{}", token.as_str());
        let mut worker = Self {
            owned: Some(OwnedContainer {
                docker,
                token: token.to_string(),
                name,
                image: config.image,
                id: None,
                create: None,
                cleanup: config.limits.cleanup,
            }),
            password,
            limits: config.limits,
            operation_deadline: None,
        };
        match worker.provision(config, deadline).await {
            Ok(()) => Ok(worker),
            Err(error) => finish(Err(error), worker.close().await),
        }
    }

    pub fn image(&self) -> PostgresImage {
        self.container().image
    }

    pub fn operation_timeout(&self) -> Duration {
        self.limits.operation_timeout()
    }

    /// Owns one rehearsal operation and always waits for verified removal.
    pub async fn run<T>(
        config: DockerWorkerConfig,
        operation: impl AsyncFnOnce(&DockerWorker) -> Result<T, WorkerError>,
    ) -> Result<T, WorkerError> {
        let mut worker = Self::start(config).await?;
        let deadline = Instant::now() + worker.operation_timeout();
        worker.operation_deadline = Some(deadline);
        let result = tokio::time::timeout_at(deadline, operation(&worker))
            .await
            .unwrap_or(Err(WorkerError::Deadline(WorkerStage::Execute)));
        finish(result, worker.close().await)
    }

    /// Removes the container by its immutable ID and confirms its absence.
    pub async fn close(mut self) -> Result<(), WorkerError> {
        let owned = self.owned.take().expect("worker owns a container");
        let worker = owned.name.clone();
        // Spawn so cancellation of the waiting caller does not cancel cleanup.
        tokio::spawn(owned.remove())
            .await
            .map_err(|_| WorkerError::CleanupUnconfirmed {
                worker,
                prior: None,
            })?
    }

    fn container(&self) -> &OwnedContainer {
        self.owned.as_ref().expect("worker owns a container")
    }

    fn session_deadline(&self) -> Instant {
        self.operation_deadline
            .unwrap_or_else(|| Instant::now() + self.operation_timeout())
    }

    async fn provision(
        &mut self,
        config: DockerWorkerConfig,
        deadline: Instant,
    ) -> Result<(), WorkerError> {
        let owned = self.owned.as_mut().expect("worker owns a container");
        let docker = owned.docker.clone();
        let settings = CreateSettings::new(config.image, &owned.token);
        let name = owned.name.clone();
        owned.create = Some(tokio::spawn(async move {
            let bytes = docker
                .run(&settings.args(&name), None, WorkerStage::Create, deadline)
                .await?
                .checked(WorkerStage::Create)?;
            let id = std::str::from_utf8(&bytes)
                .map_err(|_| WorkerError::Command(WorkerStage::Create))?
                .trim();
            if !valid_container_id(id) {
                return Err(WorkerError::Command(WorkerStage::Create));
            }
            Ok(id.to_owned())
        }));
        let id = owned.await_create().await?;
        let configured: ContainerInfo =
            inspect(&owned.docker, &id, WorkerStage::VerifyControls, deadline).await?;
        if configured.identity.id != id
            || !configured
                .identity
                .is_owned(&owned.name, &owned.token, owned.image)
        {
            return Err(WorkerError::UnsupportedRuntime);
        }
        owned.id = Some(id);
        if !configured.is_isolated(owned.image) {
            return Err(WorkerError::UnsupportedRuntime);
        }

        let id = owned.id();
        owned
            .docker
            .run(
                &["container", "start", id],
                None,
                WorkerStage::Start,
                deadline,
            )
            .await?
            .checked(WorkerStage::Start)?;
        let running: ContainerInfo =
            inspect(&owned.docker, id, WorkerStage::VerifyControls, deadline).await?;
        if running.identity.id != id
            || !running
                .identity
                .is_owned(&owned.name, &owned.token, owned.image)
            || !running.is_isolated(owned.image)
        {
            return Err(WorkerError::UnsupportedRuntime);
        }
        owned
            .docker
            .run(
                &[
                    "container",
                    "exec",
                    "--user",
                    RUN_AS,
                    id,
                    "/bin/sh",
                    "-ec",
                    controls::VERIFY_KERNEL,
                ],
                None,
                WorkerStage::VerifyControls,
                deadline,
            )
            .await?
            .checked(WorkerStage::VerifyControls)
            .map_err(|_| WorkerError::UnsupportedRuntime)?;

        owned.wait_until_ready(deadline).await?;
        let schemas = config.schemas.as_ref().map(|scope| scope.schemas());
        let sql = session::initialize_sql(&self.password, schemas);
        owned.initialize(sql.as_bytes(), deadline).await?;
        self.verify_session(deadline).await
    }
}

impl Drop for DockerWorker {
    fn drop(&mut self) {
        if let Some(owned) = self.owned.take()
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            // The explicit close path waits for this result. Drop is a fallback
            // for cancellation while the caller's runtime is still running.
            runtime.spawn(owned.remove());
        }
    }
}

struct OwnedContainer {
    docker: Arc<Docker>,
    token: String,
    name: String,
    image: PostgresImage,
    id: Option<String>,
    create: Option<JoinHandle<Result<String, WorkerError>>>,
    cleanup: Duration,
}

impl OwnedContainer {
    fn id(&self) -> &str {
        self.id.as_deref().expect("worker container ID is verified")
    }

    async fn await_create(&mut self) -> Result<String, WorkerError> {
        let created = self
            .create
            .as_mut()
            .expect("worker create task is active")
            .await;
        self.create = None;
        created.map_err(|_| WorkerError::Command(WorkerStage::Create))?
    }

    async fn wait_until_ready(&self, deadline: Instant) -> Result<(), WorkerError> {
        loop {
            let status = self
                .docker
                .run(
                    &[
                        "container",
                        "exec",
                        "--user",
                        RUN_AS,
                        self.id(),
                        "pg_isready",
                        "-q",
                        "-h",
                        "/data",
                        "-U",
                        "postgres",
                        "-d",
                        "postgres",
                    ],
                    None,
                    WorkerStage::Ready,
                    deadline,
                )
                .await?
                .status;
            if status.success() {
                return Ok(());
            }
            sleep(Duration::from_millis(100)).await;
        }
    }

    async fn initialize(&self, sql: &[u8], deadline: Instant) -> Result<(), WorkerError> {
        self.docker
            .run(
                &[
                    "container",
                    "exec",
                    "-i",
                    "--user",
                    RUN_AS,
                    self.id(),
                    "psql",
                    "-X",
                    "-q",
                    "-v",
                    "ON_ERROR_STOP=1",
                    "-h",
                    "/data",
                    "-U",
                    "postgres",
                    "-d",
                    "postgres",
                ],
                Some(sql),
                WorkerStage::Initialize,
                deadline,
            )
            .await?
            .checked(WorkerStage::Initialize)
            .map(|_| ())
    }

    async fn remove(mut self) -> Result<(), WorkerError> {
        if let Some(create) = self.create.as_mut() {
            // The bounded create command may have reserved a name even when
            // its caller was cancelled or its output did not arrive.
            let _ = create.await;
        }
        let deadline = Instant::now() + self.cleanup;
        if self.remove_owned(deadline).await.is_err() {
            return Err(WorkerError::CleanupUnconfirmed {
                worker: self.name,
                prior: None,
            });
        }
        Ok(())
    }

    async fn remove_owned(&self, deadline: Instant) -> Result<(), WorkerError> {
        let ids = self.find_by_label(deadline).await?;
        for id in ids {
            if self.id.as_deref().is_some_and(|known| known != id) {
                return Err(WorkerError::Command(WorkerStage::Cleanup));
            }
            let candidate: ContainerIdentity =
                inspect(&self.docker, &id, WorkerStage::Cleanup, deadline).await?;
            if candidate.id != id || !candidate.is_owned(&self.name, &self.token, self.image) {
                return Err(WorkerError::Command(WorkerStage::Cleanup));
            }
            let _ = self
                .docker
                .run(
                    &["container", "rm", "--force", "--volumes", &id],
                    None,
                    WorkerStage::Cleanup,
                    deadline,
                )
                .await?
                .checked(WorkerStage::Cleanup);
            if !self.list_id(&id, deadline).await?.is_empty() {
                return Err(WorkerError::Command(WorkerStage::Cleanup));
            }
        }
        if let Some(known) = &self.id
            && !self.list_id(known, deadline).await?.is_empty()
        {
            return Err(WorkerError::Command(WorkerStage::Cleanup));
        }
        if self.find_by_label(deadline).await?.is_empty() {
            Ok(())
        } else {
            Err(WorkerError::Command(WorkerStage::Cleanup))
        }
    }

    async fn find_by_label(&self, deadline: Instant) -> Result<Vec<String>, WorkerError> {
        self.list_filtered(&format!("label={OWNER_LABEL}={}", self.token), deadline)
            .await
    }

    async fn list_id(&self, id: &str, deadline: Instant) -> Result<Vec<String>, WorkerError> {
        self.list_filtered(&format!("id={id}"), deadline).await
    }

    async fn list_filtered(
        &self,
        filter: &str,
        deadline: Instant,
    ) -> Result<Vec<String>, WorkerError> {
        let bytes = self
            .docker
            .run(
                &[
                    "container",
                    "ls",
                    "--all",
                    "--no-trunc",
                    "--quiet",
                    "--filter",
                    filter,
                ],
                None,
                WorkerStage::Cleanup,
                deadline,
            )
            .await?
            .checked(WorkerStage::Cleanup)?;
        let ids =
            std::str::from_utf8(&bytes).map_err(|_| WorkerError::Command(WorkerStage::Cleanup))?;
        ids.lines()
            .map(|id| {
                if valid_container_id(id) {
                    Ok(id.into())
                } else {
                    Err(WorkerError::Command(WorkerStage::Cleanup))
                }
            })
            .collect()
    }
}

async fn verify_runtime(
    docker: &Docker,
    image: PostgresImage,
    deadline: Instant,
) -> Result<(), WorkerError> {
    let runtime: RuntimeInfo = docker
        .json(
            &["info", "--format", "{{json .}}"],
            WorkerStage::Runtime,
            deadline,
        )
        .await
        .map_err(|error| match error {
            WorkerError::Command(WorkerStage::Runtime) => WorkerError::RuntimeUnavailable,
            error => error,
        })?;
    if !runtime.supports_controls() {
        return Err(WorkerError::UnsupportedRuntime);
    }
    let pinned: ImageInfo = docker
        .json(
            &[
                "image",
                "inspect",
                "--format",
                "{{json .}}",
                image.config_id(),
            ],
            WorkerStage::Image,
            deadline,
        )
        .await
        .map_err(|error| match error {
            WorkerError::Command(WorkerStage::Image) => WorkerError::ImageUnavailable,
            error => error,
        })?;
    if !pinned.matches_pin(image) {
        return Err(WorkerError::ImageUnavailable);
    }
    Ok(())
}

async fn inspect<T: DeserializeOwned>(
    docker: &Docker,
    id: &str,
    stage: WorkerStage,
    deadline: Instant,
) -> Result<T, WorkerError> {
    docker
        .json(
            &["container", "inspect", "--format", "{{json .}}", id],
            stage,
            deadline,
        )
        .await
}

fn random_hex<const N: usize>() -> Result<Zeroizing<String>, WorkerError> {
    let mut bytes = Zeroizing::new([0; N]);
    getrandom::fill(bytes.as_mut()).map_err(|_| WorkerError::LocalSetup)?;
    let mut hex = Zeroizing::new(String::with_capacity(N * 2));
    for byte in bytes.iter() {
        write!(&mut *hex, "{byte:02x}").map_err(|_| WorkerError::LocalSetup)?;
    }
    Ok(hex)
}

fn finish<T>(
    result: Result<T, WorkerError>,
    cleanup: Result<(), WorkerError>,
) -> Result<T, WorkerError> {
    match (result, cleanup) {
        (result, Ok(())) => result,
        (Ok(_), Err(cleanup)) => Err(cleanup),
        (Err(prior), Err(cleanup)) => Err(cleanup.with_prior(prior)),
    }
}
