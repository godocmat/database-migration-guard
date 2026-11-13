use std::{
    env, fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use dmg_core::inspection::InspectionScope;
use tokio::time::{Instant, sleep};
use tokio_postgres::NoTls;

use super::*;

struct Fixture {
    executable: PathBuf,
    socket: PathBuf,
    image: PostgresImage,
}

impl Fixture {
    fn selected() -> Self {
        // These ignored tests take an explicit disposable daemon. The script
        // that owns the namespace and daemon supplies both absolute paths.
        let executable = env::var_os("DMG_WORKER_TEST_DOCKER").expect("fixture Docker executable");
        let socket = env::var_os("DMG_WORKER_TEST_SOCKET").expect("fixture Docker socket");
        let image = match env::var("DMG_WORKER_TEST_POSTGRES").as_deref() {
            Ok("15.20") => PostgresImage::V15_20,
            Ok("16.16") => PostgresImage::V16_16,
            Ok("17.12") => PostgresImage::V17_12,
            Ok("18.7") => PostgresImage::V18_7,
            _ => panic!("fixture PostgreSQL version"),
        };
        Self {
            executable: executable.into(),
            socket: socket.into(),
            image,
        }
    }

    fn config(&self, limits: WorkerLimits) -> DockerWorkerConfig {
        DockerWorkerConfig::new(&self.executable, &self.socket, self.image, limits).unwrap()
    }

    fn config_with_bootstrap_failure(
        &self,
        limits: WorkerLimits,
        root: &Path,
    ) -> DockerWorkerConfig {
        // The fixture wrapper rejects the fixed admin bootstrap after a real
        // container has started. All other commands use the real local daemon.
        let wrapper = root.join("reject-bootstrap");
        let executable = self.executable.to_str().unwrap().replace('\'', "'\"'\"'");
        let script = format!(
            "#!/bin/sh\nfor arg do\n  [ \"$arg\" != psql ] || exit 42\ndone\nexec '{executable}' \"$@\"\n"
        );
        fs::write(&wrapper, script).unwrap();
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
        DockerWorkerConfig::new(wrapper, &self.socket, self.image, limits).unwrap()
    }

    fn containers(&self) -> Vec<String> {
        let private = tempfile::tempdir().unwrap();
        let output = Command::new(&self.executable)
            .env_clear()
            .arg("--config")
            .arg(private.path())
            .arg("--host")
            .arg(format!("unix://{}", self.socket.display()))
            .args(["container", "ls", "--all", "--no-trunc", "--quiet"])
            .output()
            .expect("list disposable fixture containers");
        assert!(output.status.success());
        let mut containers: Vec<_> = std::str::from_utf8(&output.stdout)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        containers.sort();
        containers
    }

    fn missing_image(&self) -> PostgresImage {
        if self.image == PostgresImage::V18_7 {
            PostgresImage::V15_20
        } else {
            PostgresImage::V18_7
        }
    }
}

#[test]
fn initialization_keeps_schema_identifiers_quoted() {
    let schemas = [String::from("name' with \"quotes\" and \\ slash")];
    let sql = session::initialize_sql("00", Some(&schemas));
    assert!(sql.contains(
        r#"CREATE SCHEMA "name' with ""quotes"" and \ slash" AUTHORIZATION dmg_migration;"#
    ));
}

#[tokio::test]
#[ignore = "requires an explicit disposable Docker fixture and one approved image"]
async fn isolated_worker_lifecycle() {
    let fixture = Fixture::selected();
    let before = fixture.containers();
    let limits = WorkerLimits::new(25_000, 5_000, 500, 100, 5_000).unwrap();
    let schemas = InspectionScope::new(vec![
        "public".into(),
        "review".into(),
        "quoted \"name\" with \\ slash".into(),
    ])
    .unwrap();
    let config = fixture.config(limits).with_schemas(schemas);
    let worker = DockerWorker::start(config)
        .await
        .expect("start pinned worker");
    let id = worker.container().id().to_owned();
    let pid = worker
        .container()
        .docker
        .json::<u32>(
            &[
                "container",
                "inspect",
                "--format",
                "{{json .State.Pid}}",
                &id,
            ],
            WorkerStage::VerifyControls,
            Instant::now() + Duration::from_secs(2),
        )
        .await
        .expect("read isolated fixture PID");
    assert!(pid > 1);
    assert!(fixture.containers().contains(&id));
    assert!(Path::new("/proc").join(pid.to_string()).is_dir());

    let config = worker.connection_config();
    let mut wrong_password = config.clone();
    wrong_password.password("synthetic-wrong-password");
    let denied = wrong_password
        .connect_raw(worker.connect_stream().unwrap(), NoTls)
        .await;
    assert_eq!(
        denied.err().and_then(|err| err.code().cloned()),
        Some(tokio_postgres::error::SqlState::INVALID_PASSWORD)
    );
    let mut wrong_role = config;
    wrong_role.user("postgres");
    assert!(
        wrong_role
            .connect_raw(worker.connect_stream().unwrap(), NoTls)
            .await
            .is_err()
    );

    worker
        .with_client(async |client| {
            client
                .batch_execute(
                    "CREATE TABLE public.control (id integer PRIMARY KEY);
                     CREATE TABLE review.sample (id integer PRIMARY KEY);
                     INSERT INTO review.sample VALUES (7);",
                )
                .await
                .expect("bounded synthetic migration");
            let row = client
                .query_one(
                    "SELECT pg_catalog.count(*),
                            pg_catalog.has_schema_privilege('quoted \"name\" with \\ slash', 'CREATE')
                     FROM review.sample",
                    &[],
                )
                .await
                .expect("synthetic result");
            assert_eq!(row.get::<_, i64>(0), 1);
            assert!(row.get::<_, bool>(1));

            let failure = client
                .batch_execute(
                    "INSERT INTO review.sample VALUES (8);
                     INSERT INTO review.sample VALUES (7);",
                )
                .await
                .unwrap_err();
            assert_eq!(failure.code(), Some(&tokio_postgres::error::SqlState::UNIQUE_VIOLATION));
            assert_eq!(
                client.query_one("SELECT pg_catalog.count(*) FROM review.sample", &[])
                    .await.unwrap().get::<_, i64>(0),
                1
            );
            let timeout = client.query_one("SELECT pg_catalog.pg_sleep(5)", &[]).await.unwrap_err();
            assert_eq!(timeout.code(), Some(&tokio_postgres::error::SqlState::QUERY_CANCELED));
            Ok(())
        })
        .await
        .expect("worker migration control");
    worker.close().await.expect("remove successful worker");
    assert_eq!(fixture.containers(), before);
    assert!(!Path::new("/proc").join(pid.to_string()).is_dir());

    let failed = DockerWorker::run(fixture.config(limits), async |_| {
        Err::<(), _>(WorkerError::Command(WorkerStage::Execute))
    })
    .await;
    assert!(matches!(
        failed,
        Err(WorkerError::Command(WorkerStage::Execute))
    ));
    assert_eq!(fixture.containers(), before);

    let short = WorkerLimits::new(25_000, 120, 500, 100, 5_000).unwrap();
    let timeout = DockerWorker::run(fixture.config(short), async |_| {
        std::future::pending::<Result<(), WorkerError>>().await
    })
    .await;
    assert!(matches!(
        timeout,
        Err(WorkerError::Deadline(WorkerStage::Execute))
    ));
    assert_eq!(fixture.containers(), before);

    let wrapper_root = tempfile::tempdir().unwrap();
    let config = fixture.config_with_bootstrap_failure(limits, wrapper_root.path());
    let startup_failure = DockerWorker::start(config).await;
    assert!(matches!(
        startup_failure,
        Err(WorkerError::Command(WorkerStage::Initialize))
    ));
    assert_eq!(fixture.containers(), before);

    let missing = DockerWorkerConfig::new(
        &fixture.executable,
        &fixture.socket,
        fixture.missing_image(),
        limits,
    )
    .unwrap();
    assert!(matches!(
        DockerWorker::start(missing).await,
        Err(WorkerError::ImageUnavailable)
    ));
    assert_eq!(fixture.containers(), before);

    // Dropping the handle while the runtime is live schedules its fallback.
    let abandoned = DockerWorker::start(fixture.config(limits)).await.unwrap();
    drop(abandoned);
    let deadline = Instant::now() + Duration::from_secs(5);
    while fixture.containers() != before {
        assert!(Instant::now() < deadline, "abandoned worker still present");
        sleep(Duration::from_millis(100)).await;
    }
}
