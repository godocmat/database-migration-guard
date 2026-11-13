"""Measure restricted catalog reads on a disposable PostgreSQL server."""

from __future__ import annotations

import argparse
from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path
import secrets
import socket
import subprocess
import tempfile
import time


ADMIN = "dmg_catalog_setup"
READER = "dmg_catalog_reader"
SCOPE = "ARRAY['catalog_scope', 'catalog_hidden', 'catalog_missing', 'catalog_public', 'catalog_empty']"
EXACT_SIZE = "SELECT pg_catalog.pg_total_relation_size('catalog_scope.accounts'::regclass)"
READ_SETTINGS = """BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY;
SET LOCAL search_path = pg_catalog, pg_temp;
SET LOCAL statement_timeout = '1000ms';
SET LOCAL lock_timeout = '100ms';
SET LOCAL idle_in_transaction_session_timeout = '3s';
"""
SETUP = """
CREATE ROLE dmg_catalog_reader LOGIN INHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
CREATE ROLE dmg_catalog_scope NOLOGIN;
CREATE ROLE dmg_catalog_owner NOLOGIN;
GRANT dmg_catalog_scope TO dmg_catalog_reader;
REVOKE CONNECT, TEMPORARY ON DATABASE postgres FROM PUBLIC;
GRANT CONNECT ON DATABASE postgres TO dmg_catalog_reader;
CREATE SCHEMA catalog_scope;
CREATE SCHEMA catalog_hidden;
CREATE SCHEMA catalog_public;
CREATE SCHEMA catalog_empty;
CREATE SCHEMA catalog_owned AUTHORIZATION dmg_catalog_owner;
GRANT USAGE ON SCHEMA catalog_scope, catalog_empty TO dmg_catalog_scope;
GRANT USAGE ON SCHEMA catalog_public TO PUBLIC;
CREATE TABLE catalog_scope.accounts (id integer PRIMARY KEY, quota integer DEFAULT 3);
INSERT INTO catalog_scope.accounts SELECT n, n FROM generate_series(1, 256) n;
ALTER TABLE catalog_scope.accounts ADD CONSTRAINT quota_positive CHECK (quota > 0) NOT VALID;
CREATE INDEX accounts_quota_idx ON catalog_scope.accounts (quota);
ANALYZE catalog_scope.accounts;
CREATE TABLE catalog_scope.unanalyzed (id integer);
CREATE TABLE catalog_scope.events (id integer NOT NULL) PARTITION BY RANGE (id);
CREATE TABLE catalog_scope.events_first PARTITION OF catalog_scope.events FOR VALUES FROM (0) TO (10);
CREATE TABLE catalog_hidden.hidden_table (id integer);
CREATE TABLE catalog_public.metadata_only (id integer);
"""


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def sha256(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


class Cluster:
    def __init__(self, pg_bin, scratch, tls):
        self.pg_bin = pg_bin
        self.scratch = scratch
        self.data = scratch / "data"
        self.tls = tls
        self.password = secrets.token_hex(24)
        self.env = {
            "PATH": os.defpath, "HOME": str(scratch), "LC_ALL": "C.UTF-8", "TZ": "UTC",
            "PGCONNECT_TIMEOUT": "3",
        }
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
            sock.bind(("127.0.0.1", 0))
            self.port = sock.getsockname()[1]

    def run(self, executable, *args, sql=None, check=True, extra_env=None):
        env = {**self.env, **(extra_env or {})}
        result = subprocess.run(
            [executable, *args], cwd=self.scratch, env=env, input=sql, capture_output=True,
            text=True, encoding="utf-8", timeout=25,
        )
        if check and result.returncode:
            detail = result.stderr.replace(self.password, "<fixture password>")
            raise RuntimeError(f"{Path(executable).name} exited {result.returncode}\n{detail}")
        return result

    def certificates(self):
        for label in ("ca", "untrusted"):
            self.run(
                "openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
                "-subj", f"/CN=catalog-fixture-{label}", "-keyout", f"{label}.key",
                "-out", f"{label}.crt",
            )
        self.run(
            "openssl", "req", "-newkey", "rsa:2048", "-nodes", "-subj", "/CN=localhost",
            "-keyout", "server.key", "-out", "server.csr",
        )
        (self.scratch / "server.ext").write_text(
            "subjectAltName = DNS:localhost\nextendedKeyUsage = serverAuth\n", encoding="ascii",
        )
        self.run(
            "openssl", "x509", "-req", "-days", "1", "-in", "server.csr", "-CA", "ca.crt",
            "-CAkey", "ca.key", "-CAcreateserial", "-extfile", "server.ext", "-out", "server.crt",
        )
        (self.scratch / "server.key").chmod(0o600)

    @contextmanager
    def running(self):
        password_file = self.scratch / "bootstrap-password"
        password_file.write_text(secrets.token_hex(24) + "\n", encoding="ascii")
        password_file.chmod(0o600)
        self.run(
            self.pg_bin / "initdb", "-D", str(self.data), "--auth-local=trust",
            "--auth-host=scram-sha-256", "--no-locale", "--encoding=UTF8", f"--username={ADMIN}",
            f"--pwfile={password_file}",
        )
        password_file.unlink()
        socket_path = str(self.scratch).replace("'", "''")
        with (self.data / "postgresql.conf").open("a", encoding="utf-8") as config:
            host = "127.0.0.1" if self.tls else ""
            config.write(
                f"\nlisten_addresses = '{host}'\nport = {self.port}\n"
                f"unix_socket_directories = '{socket_path}'\n"
                "autovacuum = off\nlog_error_verbosity = verbose\n"
            )
            if self.tls:
                self.certificates()
                config.write(
                    f"ssl = on\nssl_cert_file = '{socket_path}/server.crt'\n"
                    f"ssl_key_file = '{socket_path}/server.key'\n"
                )
                (self.data / "pg_hba.conf").write_text(
                    "local all all trust\n"
                    f"hostssl postgres {READER} 127.0.0.1/32 scram-sha-256\n"
                    "host all all 127.0.0.1/32 reject\n", encoding="ascii",
                )
        ctl = self.pg_bin / "pg_ctl"
        try:
            self.run(ctl, "-D", str(self.data), "-l", "postgres.log", "-w", "-t", "15", "start")
            self.psql(SETUP)
            if self.tls:
                self.psql(f"ALTER ROLE {READER} PASSWORD '{self.password}'")
            yield
        finally:
            if (self.data / "postmaster.pid").exists():
                self.run(ctl, "-D", str(self.data), "-w", "-t", "15", "-m", "immediate", "stop")

    def psql_args(self, user):
        return [self.pg_bin / "psql", "-XqAtw", "-v", "ON_ERROR_STOP=1", "-h", str(self.scratch),
                "-p", str(self.port), "-U", user, "-d", "postgres"]

    def psql(self, sql, *, user=ADMIN, check=True):
        return self.run(*self.psql_args(user), sql=sql, check=check)

    @contextmanager
    def hold_access_exclusive(self):
        with subprocess.Popen(
            self.psql_args(ADMIN), cwd=self.scratch, env=self.env, text=True,
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        ) as blocker:
            try:
                blocker.stdin.write("BEGIN; LOCK TABLE catalog_scope.accounts IN ACCESS EXCLUSIVE MODE;\n")
                blocker.stdin.flush()
                deadline = time.monotonic() + 4
                while time.monotonic() < deadline:
                    lock = self.psql("""
SELECT 1 FROM pg_catalog.pg_locks
WHERE relation = 'catalog_scope.accounts'::regclass AND mode = 'AccessExclusiveLock'
  AND database = (SELECT oid FROM pg_catalog.pg_database WHERE datname = current_database())
  AND granted;
""")
                    if lock.stdout.strip() == "1":
                        break
                    time.sleep(0.025)
                else:
                    raise RuntimeError("fixture lock did not become available")
                yield
            finally:
                try:
                    blocker.communicate("ROLLBACK;\n", timeout=5)
                except subprocess.TimeoutExpired:
                    blocker.kill()
                    blocker.communicate()
                    raise
                require(blocker.returncode == 0, "fixture locker failed")

    def read(self, query):
        result = self.psql(f"{READ_SETTINGS}\n{query}\nROLLBACK;\n", user=READER)
        return [json.loads(line) for line in result.stdout.splitlines()]

    def failed_read(self, query, timeout=None):
        settings = READ_SETTINGS
        if timeout:
            settings += f"SET LOCAL statement_timeout = '{timeout}';\nSET LOCAL lock_timeout = '1s';\n"
        start = time.monotonic()
        result = self.psql(
            settings + "SAVEPOINT optional_read;\n"
            "\\set ON_ERROR_STOP off\n\\set VERBOSITY verbose\n"
            f"{query.rstrip(';')};\n\\echo :SQLSTATE\n"
            "\\set ON_ERROR_STOP on\nROLLBACK TO SAVEPOINT optional_read;\n"
            "SELECT current_setting('transaction_read_only');\nROLLBACK;\n",
            user=READER,
        )
        sqlstate, read_only = result.stdout.strip().splitlines()[-2:]
        require(read_only == "on", "savepoint recovery left read-only transaction")
        return sqlstate, round((time.monotonic() - start) * 1000, 2)

    def tls_read(self, hostname, certificate, mode):
        conninfo = (
            f"host={hostname} hostaddr=127.0.0.1 port={self.port} dbname=postgres user={READER} "
            f"sslmode={mode} sslrootcert={self.scratch / certificate}"
        )
        return self.run(
            self.pg_bin / "psql", "-XqAtw", "-d", conninfo,
            sql=READ_SETTINGS + "SELECT ssl FROM pg_catalog.pg_stat_ssl WHERE pid = pg_backend_pid();\nROLLBACK;\n",
            check=False, extra_env={"PGPASSWORD": self.password},
        )


def observe(cluster, queries):
    query_names = ("scope", "relations", "columns", "indexes", "constraints", "partitions")
    definition_sections = ("columns", "indexes", "constraints")
    commands = {
        name: f"EXECUTE inspect_{name}({SCOPE}{'' if name == 'scope' else ', 33'}"
              f"{', true' if name in definition_sections else ''});"
        for name in query_names
    }
    session, = cluster.read("""
SELECT json_build_object('server_version_num', current_setting('server_version_num')::int,
    'read_only', current_setting('transaction_read_only') = 'on',
    'repeatable_read', current_setting('transaction_isolation') = 'repeatable read',
    'observed_at_valid', transaction_timestamp() <= clock_timestamp(),
    'elevated_role', (SELECT rolsuper OR rolcreatedb OR rolcreaterole OR rolreplication OR rolbypassrls
        FROM pg_catalog.pg_roles WHERE rolname = current_user),
    'read_all_stats', pg_catalog.pg_has_role(current_user, 'pg_read_all_stats', 'USAGE'),
    'table_select', pg_catalog.has_table_privilege(current_user, 'catalog_scope.accounts', 'SELECT'),
    'schema_create', pg_catalog.has_schema_privilege('catalog_scope', 'CREATE'),
    'owner_usage', pg_catalog.has_schema_privilege('dmg_catalog_owner', 'catalog_owned', 'USAGE'),
    'owner_create', pg_catalog.has_schema_privilege('dmg_catalog_owner', 'catalog_owned', 'CREATE'),
    'hidden_catalog_visible', EXISTS (SELECT FROM pg_catalog.pg_namespace WHERE nspname = 'catalog_hidden'),
    'block_size', current_setting('block_size')::int);
""")
    for key in ("read_only", "repeatable_read", "observed_at_valid", "owner_usage", "owner_create", "hidden_catalog_visible"):
        require(session[key], f"expected enabled session evidence {key}")
    for key in ("elevated_role", "read_all_stats", "table_select", "schema_create"):
        require(not session[key], f"unexpected reader privilege {key}")

    data = {name: cluster.read(queries + commands[name]) for name in query_names}
    scope = {row["schema"]: row["state"] for row in data["scope"]}
    require(scope == {
        "catalog_scope": "visible", "catalog_hidden": "denied", "catalog_missing": "missing",
        "catalog_public": "visible", "catalog_empty": "visible",
    }, "scope visibility differed")
    relations = {row["relation"]: row for row in data["relations"]}
    require(set(relations) == {"accounts", "unanalyzed", "events", "events_first", "metadata_only"},
            "catalog scope returned an unexpected relation")
    require(relations["accounts"]["estimated_rows"] == 256, "ANALYZE estimate differed")
    require(relations["accounts"]["estimated_heap_bytes"] > 0, "analyzed heap estimate unavailable")
    require(relations["unanalyzed"]["estimated_rows"] is None, "unknown row estimate was treated as a count")
    require(relations["events"]["estimated_heap_bytes"] is None, "parent heap estimate is not applicable")
    require(relations["events_first"]["is_partition"], "partition identity missing")
    require(data["partitions"] == [{"child": "events_first", "parent": "events", "detach_pending": False}],
            "partition edge differed")
    quota, = [row for row in data["columns"] if row["relation"] == "accounts" and row["column"] == "quota"]
    require(quota["type"] == "integer" and quota["default_definition"] == "3", "quota catalog shape differed")
    check, = [row for row in data["constraints"] if row["constraint"] == "quota_positive"]
    require(not check["validated"], "unvalidated CHECK lost state")
    quota_index, = [row for row in data["indexes"] if row["index"] == "accounts_quota_idx"]
    require(quota_index["valid"] and quota_index["ready"], "index readiness not visible")
    capped = cluster.read(queries + f"EXECUTE inspect_relations({SCOPE}, 2);")
    require(len(capped) == 2 and len(data["relations"]) > len(capped), "limit sentinel unavailable")
    size, = cluster.read(EXACT_SIZE + ";")
    require(size >= relations["accounts"]["estimated_heap_bytes"], "unexpected exact table size")
    denied, _ = cluster.failed_read("SELECT id FROM catalog_scope.accounts")
    read_only, _ = cluster.failed_read("CREATE TABLE catalog_scope.unexpected (id integer)")
    require(denied == "42501" and read_only == "25006", "reader privilege or transaction check failed")

    names, = cluster.read("""
SELECT json_build_object('named_not_null', count(*) FILTER (WHERE k.contype = 'n'),
  'enforcement_field', EXISTS (SELECT FROM pg_catalog.pg_attribute
     WHERE attrelid = 'pg_catalog.pg_constraint'::regclass AND attname = 'conenforced'))
FROM pg_catalog.pg_constraint k WHERE k.conrelid = 'catalog_scope.accounts'::regclass;
""")
    supports_named = session["server_version_num"] >= 180000
    require(names == {"named_not_null": int(supports_named), "enforcement_field": supports_named},
            "major-specific constraint shape differed")
    if supports_named:
        enforcement, = cluster.read("""
SELECT json_build_object('enforced', conenforced) FROM pg_catalog.pg_constraint
WHERE conrelid = 'catalog_scope.accounts'::regclass AND conname = 'quota_positive';
""")
        require(enforcement == {"enforced": True}, "PG18 CHECK enforcement unavailable")

    with cluster.hold_access_exclusive():
        require(cluster.read(queries + commands["relations"]) == data["relations"],
                "estimates changed while the table was locked")
        locked = {}
        for name in query_names:
            sqlstate, elapsed = cluster.failed_read(queries + commands[name])
            require(sqlstate in ("00000", "55P03"), f"unexpected {name} SQLSTATE under DDL lock")
            locked[name] = {"sqlstate": sqlstate, "elapsed_ms": elapsed}
        raw_sections = {}
        for name in definition_sections:
            require(locked[name]["sqlstate"] == "55P03", f"expected {name} definition to wait for the lock")
            command = commands[name].replace(", true)", ", false)")
            sqlstate, elapsed = cluster.failed_read(queries + command)
            require(sqlstate == "00000", f"raw {name} catalog fields unexpectedly waited for the lock")
            raw_sections[name] = {"sqlstate": sqlstate, "elapsed_ms": elapsed}
        blocked_size, size_ms = cluster.failed_read(EXACT_SIZE)
        statement_timeout, timeout_ms = cluster.failed_read(EXACT_SIZE, "50ms")
        require(locked["relations"]["sqlstate"] == "00000", "estimate unexpectedly waited for table lock")
        require((blocked_size, statement_timeout) == ("55P03", "57014"), "timeout distinction differed")

    cluster.psql("REVOKE EXECUTE ON FUNCTION pg_catalog.pg_total_relation_size(regclass) FROM PUBLIC;")
    denied_size, _ = cluster.failed_read(EXACT_SIZE)
    require(denied_size == "42501", "revoked optional function was accessible")
    require(cluster.read(queries + commands["relations"]) == data["relations"], "catalog estimates did not survive denial")
    evidence = {
        "session": session, "scope": scope, "relations": relations,
        "constraint_catalog": names, "quota_default": quota["default_definition"],
        "unvalidated_check": check["definition"], "valid_index": quota_index["definition"],
        "partition_edges": data["partitions"], "limit_sentinel": len(capped),
        "physical_total_bytes_before_lock": size, "app_select_sqlstate": denied,
        "read_only_sqlstate": read_only, "denied_optional_size_sqlstate": denied_size,
        "under_ddl_lock": locked,
        "raw_fields_under_ddl_lock": raw_sections,
        "size_lock_timeout": {"sqlstate": blocked_size, "elapsed_ms": size_ms},
        "size_statement_timeout": {"sqlstate": statement_timeout, "elapsed_ms": timeout_ms},
    }
    if cluster.tls:
        success = cluster.tls_read("localhost", "ca.crt", "verify-full")
        mismatch = cluster.tls_read("wrong.localhost", "ca.crt", "verify-full")
        untrusted = cluster.tls_read("localhost", "untrusted.crt", "verify-full")
        plaintext = cluster.tls_read("localhost", "ca.crt", "disable")
        require(success.returncode == 0 and success.stdout.strip() == "t", "verified TLS connection failed")
        require(mismatch.returncode != 0 and "does not match host name" in mismatch.stderr, "wrong host passed")
        require(untrusted.returncode != 0 and "certificate verify failed" in untrusted.stderr, "untrusted CA passed")
        require(plaintext.returncode != 0 and "no encryption" in plaintext.stderr, "plaintext passed TLS-only HBA")
        evidence["tls"] = {"valid_ca_and_name": True, "wrong_name_rejected": True,
                           "untrusted_ca_rejected": True, "plaintext_rejected": True}
    return evidence


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pg-bin", required=True, type=Path)
    parser.add_argument("--tls", action="store_true", help="also exercise libpq against a TLS-enabled build")
    args = parser.parse_args()
    pg_bin = args.pg_bin.resolve(strict=True)
    queries = Path(__file__).with_name("catalog.sql").read_text(encoding="utf-8")
    with tempfile.TemporaryDirectory(prefix="dmg-catalog-") as scratch:
        cluster = Cluster(pg_bin, Path(scratch), args.tls)
        with cluster.running():
            evidence = observe(cluster, queries)
        evidence["postgres_binary_sha256"] = sha256(pg_bin / "postgres")
        evidence["psql_binary_sha256"] = sha256(pg_bin / "psql")
    print(json.dumps(evidence, sort_keys=True, indent=2))


if __name__ == "__main__":
    main()
