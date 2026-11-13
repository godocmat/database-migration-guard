use std::{
    env, fs, io,
    time::{Duration, Instant},
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    time::{sleep, timeout},
};

use super::*;

const FIXTURE_VERSION: u32 = 180007;
const FIXTURE_USER: &str = "dmg_catalog_reader";
const FIXTURE_APP: &str = "dmg-inspect";

#[test]
fn validates_protected_inputs_without_exposing_them() {
    let attempt = |host, port, database, user, password, ca| {
        InspectionTarget::new(host, port, database, user, password, ca).unwrap_err()
    };
    assert_eq!(
        attempt(
            "postgres://protected.example",
            5432,
            "db",
            "role",
            vec![1],
            b""
        ),
        TransportError::InvalidTarget
    );
    assert_eq!(
        attempt("host", 0, "db", "role", vec![1], b""),
        TransportError::InvalidTarget
    );
    assert_eq!(
        attempt("host", 5432, "db\0", "role", vec![1], b""),
        TransportError::InvalidTarget
    );
    assert_eq!(
        attempt("host", 5432, "db", "role", vec![], b""),
        TransportError::InvalidTarget
    );
    assert_eq!(
        attempt("host", 5432, "db", "role", vec![1], b"invalid CA"),
        TransportError::InvalidCa
    );
    assert_eq!(
        attempt(
            "host",
            5432,
            "db",
            "role",
            vec![1],
            &vec![b'a'; MAX_CA_BYTES + 1],
        ),
        TransportError::InvalidCa
    );
    assert_eq!(
        InspectionTimeouts::new(0, 100, 10, 200).unwrap_err(),
        TransportError::InvalidTimeouts
    );
    assert_eq!(
        InspectionTimeouts::new(500, MAX_TIMEOUT_MS + 1, 10, 200).unwrap_err(),
        TransportError::InvalidTimeouts
    );
}

#[tokio::test]
#[ignore = "requires the disposable PostgreSQL TLS fixture"]
async fn native_tls_read_only_transport() {
    let port = fixture_port("DMG_TEST_POSTGRES_PORT");
    let no_tls_port = fixture_port("DMG_TEST_POSTGRES_NO_TLS_PORT");
    let ca = fs::read(env::var_os("DMG_TEST_POSTGRES_CA").unwrap()).unwrap();
    let wrong_ca = fs::read(env::var_os("DMG_TEST_POSTGRES_WRONG_CA").unwrap()).unwrap();
    let password = Zeroizing::new(env::var("DMG_TEST_POSTGRES_PASSWORD").unwrap().into_bytes());
    let target = InspectionTarget::new(
        "localhost",
        port,
        "postgres",
        FIXTURE_USER,
        password.to_vec(),
        &ca,
    )
    .unwrap();

    let expected = ServerInfo {
        server_version_num: FIXTURE_VERSION,
        block_size: 8192,
    };
    assert_eq!(
        target.probe(InspectionTimeouts::default()).await,
        Ok(expected)
    );
    assert_redacted(&format!("{target:?}"), &password);

    let without_tls = connect_failure(
        InspectionTarget::new(
            "localhost",
            no_tls_port,
            "postgres",
            FIXTURE_USER,
            password.to_vec(),
            &ca,
        )
        .unwrap(),
    )
    .await;
    assert_redacted(&format!("{without_tls:?} {without_tls}"), &password);

    // Both names route to the fixture. Its leaf certificate has a DNS SAN and no IP SAN.
    for invalid in [
        InspectionTarget::new(
            "127.0.0.1",
            port,
            "postgres",
            FIXTURE_USER,
            password.to_vec(),
            &ca,
        )
        .unwrap(),
        InspectionTarget::new(
            "localhost",
            port,
            "postgres",
            FIXTURE_USER,
            password.to_vec(),
            &wrong_ca,
        )
        .unwrap(),
    ] {
        let failure = connect_failure(invalid).await;
        assert_redacted(&format!("{failure:?} {failure}"), &password);
    }

    let invalid_password = InspectionTarget::new(
        "localhost",
        port,
        "postgres",
        FIXTURE_USER,
        b"invalid-fixture-password".to_vec(),
        &ca,
    )
    .unwrap();
    let failure = connect_failure(invalid_password).await;
    assert_eq!(failure.sqlstate(), Some("28P01"));
    assert_redacted(&format!("{failure:?} {failure}"), &password);

    let plaintext = Config::new()
        .host("127.0.0.1")
        .port(port)
        .user(FIXTURE_USER)
        .dbname("postgres")
        .ssl_mode(SslMode::Disable)
        .connect(tokio_postgres::NoTls)
        .await;
    let error = plaintext.err().expect("server accepted a plaintext login");
    assert_eq!(
        error.code().map(tokio_postgres::error::SqlState::code),
        Some("28000")
    );

    let restricted_write = target
        .with_read_only(InspectionTimeouts::default(), async |tx, _| {
            tx.batch_execute("CREATE TABLE public.unexpected_transport_write (id int)")
                .await
                .map_err(|error| TransportError::postgres(TransportStage::Query, &error))
        })
        .await
        .unwrap_err();
    assert_eq!(restricted_write.sqlstate(), Some("25006"));

    let sqlstate = target
        .with_read_only(
            InspectionTimeouts::new(1000, 75, 35, 300).unwrap(),
            async |tx, _| {
                tx.query_one("SELECT pg_catalog.pg_sleep(2)", &[])
                    .await
                    .map_err(|error| TransportError::postgres(TransportStage::Query, &error))
            },
        )
        .await
        .unwrap_err();
    assert_eq!(sqlstate.sqlstate(), Some("57014"));
    assert_redacted(&format!("{sqlstate:?} {sqlstate}"), &password);

    let started = Instant::now();
    let client_deadline = target
        .with_read_only(
            InspectionTimeouts::new(300, 700, 50, 1000).unwrap(),
            async |tx, _| {
                tx.query_one("SELECT pg_catalog.pg_sleep(3)", &[])
                    .await
                    .map_err(|error| TransportError::postgres(TransportStage::Query, &error))
            },
        )
        .await;
    assert_eq!(client_deadline.unwrap_err(), TransportError::Deadline);
    assert!(started.elapsed() < Duration::from_millis(800));
    require_backend_cleanup(&target).await;
    assert_eq!(
        target.probe(InspectionTimeouts::default()).await,
        Ok(expected)
    );

    observe_stalled_and_refused_tls(&ca, &password).await;
    println!(
        "verified TLS, hostname, CA, plaintext refusal, SQLSTATE redaction, deadlines and cleanup"
    );
}

fn fixture_port(variable: &str) -> u16 {
    env::var(variable).unwrap().parse().unwrap()
}

async fn connect_failure(target: InspectionTarget) -> TransportError {
    let failure = target
        .probe(InspectionTimeouts::default())
        .await
        .unwrap_err();
    assert!(matches!(
        failure,
        TransportError::Postgres {
            stage: TransportStage::Connect,
            ..
        }
    ));
    failure
}

fn assert_redacted(text: &str, password: &[u8]) {
    for sensitive in [
        FIXTURE_USER,
        "localhost",
        "invalid-fixture-password",
        str::from_utf8(password).unwrap(),
    ] {
        assert!(
            !text.contains(sensitive),
            "unexpected protected field in diagnostic"
        );
    }
}

async fn require_backend_cleanup(target: &InspectionTarget) {
    timeout(Duration::from_secs(2), async {
        loop {
            let active = target
                .with_read_only(InspectionTimeouts::default(), async |tx, _| {
                    let row = tx
                        .query_one(
                            "SELECT pg_catalog.count(*) FROM pg_catalog.pg_stat_activity \
                             WHERE application_name = $1 AND pid <> pg_catalog.pg_backend_pid()",
                            &[&FIXTURE_APP],
                        )
                        .await
                        .map_err(|error| TransportError::postgres(TransportStage::Query, &error))?;
                    Ok(row.get::<_, i64>(0))
                })
                .await
                .unwrap();
            if active == 0 {
                break;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("inspection backend survived client cleanup and server statement timeout");
}

async fn observe_stalled_and_refused_tls(ca: &[u8], password: &[u8]) {
    for stall in [false, true] {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let target = InspectionTarget::new(
            "127.0.0.1",
            port,
            "postgres",
            FIXTURE_USER,
            password.to_vec(),
            ca,
        )
        .unwrap();
        let test_server = async {
            let (mut peer, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 8];
            peer.read_exact(&mut request).await.unwrap();
            assert_eq!(request, [0, 0, 0, 8, 4, 210, 22, 47]);
            if !stall {
                peer.write_all(b"N").await.unwrap();
            }
            let next = peer.read(&mut request).await;
            assert!(
                matches!(next, Ok(0))
                    || next.is_err_and(|error| error.kind() == io::ErrorKind::ConnectionReset),
                "client did not close the connection after TLS failed"
            );
        };
        let (failure, ()) = timeout(Duration::from_secs(2), async {
            tokio::join!(
                target.probe(InspectionTimeouts::new(300, 100, 20, 200).unwrap()),
                test_server
            )
        })
        .await
        .expect("TLS timeout did not close the client connection");
        let failure = failure.unwrap_err();
        if stall {
            assert_eq!(failure, TransportError::Deadline);
        } else {
            assert!(matches!(
                failure,
                TransportError::Postgres {
                    stage: TransportStage::Connect,
                    ..
                }
            ));
        }
    }
}
