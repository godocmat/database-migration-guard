use std::{
    ffi::OsString,
    os::unix::fs::FileTypeExt,
    path::PathBuf,
    process::{ExitStatus, Stdio},
};

use serde::de::DeserializeOwned;
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
    time::{Instant, timeout_at},
};

use super::{DockerWorkerConfig, WorkerError, WorkerStage};

const MAX_OUTPUT: usize = 64 * 1024;

pub(super) struct Docker {
    executable: PathBuf,
    host: OsString,
    config: TempDir,
}

impl Docker {
    pub(super) fn new(config: &DockerWorkerConfig) -> Result<Self, WorkerError> {
        if !config.executable.is_file()
            || !config
                .socket
                .metadata()
                .is_ok_and(|info| info.file_type().is_socket())
        {
            return Err(WorkerError::InvalidEndpoint);
        }
        let mut host = OsString::from("unix://");
        host.push(&config.socket);
        let config_dir = tempfile::Builder::new()
            .prefix("dmg-docker-")
            .tempdir_in("/tmp")
            .map_err(|_| WorkerError::LocalSetup)?;
        Ok(Self {
            executable: config.executable.clone(),
            host,
            config: config_dir,
        })
    }

    pub(super) fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(&self.executable);
        command
            .env_clear()
            .env("LC_ALL", "C")
            .current_dir(self.config.path())
            .arg("--config")
            .arg(self.config.path())
            .arg("--host")
            .arg(&self.host)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        command
    }

    pub(super) async fn run(
        &self,
        args: &[&str],
        input: Option<&[u8]>,
        stage: WorkerStage,
        deadline: Instant,
    ) -> Result<Output, WorkerError> {
        timeout_at(deadline, async {
            if Instant::now() >= deadline {
                return Err(WorkerError::Deadline(stage));
            }
            let mut command = self.command(args);
            if input.is_some() {
                command.stdin(Stdio::piped());
            }
            let mut child = command.spawn().map_err(|_| WorkerError::Command(stage))?;
            let mut stdout = child.stdout.take().ok_or(WorkerError::Command(stage))?;
            let read = async {
                let mut bytes = Vec::new();
                (&mut stdout)
                    .take(MAX_OUTPUT as u64 + 1)
                    .read_to_end(&mut bytes)
                    .await
                    .map_err(|_| WorkerError::Command(stage))?;
                if bytes.len() > MAX_OUTPUT {
                    return Err(WorkerError::OutputLimit(stage));
                }
                Ok(bytes)
            };
            let stdin = child.stdin.take();
            let write = async move {
                if let Some(input) = input {
                    let mut pipe = stdin.ok_or(WorkerError::Command(stage))?;
                    pipe.write_all(input)
                        .await
                        .map_err(|_| WorkerError::Command(stage))?;
                    pipe.shutdown()
                        .await
                        .map_err(|_| WorkerError::Command(stage))?;
                }
                Ok(())
            };
            let (stdout, ()) = tokio::try_join!(read, write)?;
            let status = child
                .wait()
                .await
                .map_err(|_| WorkerError::Command(stage))?;
            Ok(Output { status, stdout })
        })
        .await
        .map_err(|_| WorkerError::Deadline(stage))?
    }

    pub(super) async fn json<T: DeserializeOwned>(
        &self,
        args: &[&str],
        stage: WorkerStage,
        deadline: Instant,
    ) -> Result<T, WorkerError> {
        let bytes = self
            .run(args, None, stage, deadline)
            .await?
            .checked(stage)?;
        serde_json::from_slice(&bytes).map_err(|_| WorkerError::Command(stage))
    }
}

pub(super) struct Output {
    pub(super) status: ExitStatus,
    stdout: Vec<u8>,
}

impl Output {
    pub(super) fn checked(self, stage: WorkerStage) -> Result<Vec<u8>, WorkerError> {
        if self.status.success() {
            Ok(self.stdout)
        } else {
            Err(WorkerError::Command(stage))
        }
    }
}

pub(super) fn valid_container_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
