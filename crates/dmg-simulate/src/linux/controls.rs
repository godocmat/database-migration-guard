use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

use super::{PostgresImage, WorkerLimits};

pub(super) const RUN_AS: &str = "70:70";
pub(super) const OWNER_LABEL: &str = "io.database-migration-guard.worker";
const SHM_BYTES: u64 = 16 * 1024 * 1024;
const DATA_TMPFS: &str = "rw,noexec,nosuid,nodev,size=128m,mode=0700,uid=70,gid=70";
const TMP_TMPFS: &str = "rw,noexec,nosuid,nodev,size=16m,mode=1777";
const IMAGE_VOLUME_TMPFS: &str = "rw,noexec,nosuid,nodev,size=1m,mode=0700,uid=70,gid=70";

// All shell commands in the runtime come from this file. No input SQL or caller
// configuration is passed to them. The optional database schemas are provisioned
// separately as escaped SQL identifiers via the administrator's local socket.
const BOOTSTRAP: &str = "
umask 077
initdb --no-sync --no-locale -E UTF8 --auth-local=peer --auth-host=scram-sha-256 \
    -U postgres -D /data/pgdata >/dev/null
printf '%s\\n' \
    'local all postgres peer' \
    'host postgres dmg_migration 127.0.0.1/32 scram-sha-256' \
    > /data/pgdata/pg_hba.conf
exec /bin/busybox timeout -s KILL 450 postgres -D /data/pgdata \
    -c listen_addresses=127.0.0.1 -c unix_socket_directories=/data \
    -c shared_buffers=16MB -c max_connections=12 -c dynamic_shared_memory_type=mmap \
    -c fsync=off -c max_parallel_workers=0 -c log_statement=none -c log_min_messages=panic \
    -c password_encryption=scram-sha-256
";

// Inspect the actual worker namespace before sending the bootstrap SQL. Refuse
// to proceed when cgroup settings or the expected seccomp filter are missing.
pub(super) const VERIFY_KERNEL: &str = r#"
test "$(id -u)" = 70
test "$(id -g)" = 70
test "$(ls -1 /sys/class/net)" = lo
test "$(wc -l < /proc/net/route)" -eq 1
awk '
    $1 == "CapEff:" || $1 == "CapPrm:" || $1 == "CapInh:" ||
    $1 == "CapBnd:" || $1 == "CapAmb:" { if ($2 !~ /^0+$/) exit 1; caps++ }
    $1 == "NoNewPrivs:" { nnp = $2 }
    $1 == "Seccomp:" { sec = $2 }
    END { if (caps != 5 || nnp != 1 || sec != 2) exit 1 }
' /proc/self/status
awk '
    $2 == "/" { if ($4 !~ /(^|,)ro(,|$)/) exit 1; root = 1 }
    $2 == "/data" || $2 == "/tmp" || $2 == "/dev/shm" ||
    $2 == "/var/lib/postgresql" || $2 == "/var/lib/postgresql/data" {
        if ($3 != "tmpfs" || $4 !~ /(^|,)noexec(,|$)/ ||
            $4 !~ /(^|,)nosuid(,|$)/ || $4 !~ /(^|,)nodev(,|$)/) exit 1
        if ($2 == "/data" && $4 !~ /(^|,)size=131072k(,|$)/) exit 1
        if (($2 == "/tmp" || $2 == "/dev/shm") && $4 !~ /(^|,)size=16384k(,|$)/) exit 1
        if ($2 ~ /^\/var\/lib\/postgresql/ && $4 !~ /(^|,)size=1024k(,|$)/) exit 1
        mounts++
    }
    END { if (root != 1 || mounts != 4) exit 1 }
' /proc/mounts
if test -r /sys/fs/cgroup/cgroup.controllers; then
    test "$(cat /sys/fs/cgroup/memory.max)" = 201326592
    test "$(cat /sys/fs/cgroup/memory.swap.max)" = 0
    test "$(cat /sys/fs/cgroup/pids.max)" = 36
    read -r quota period < /sys/fs/cgroup/cpu.max
else
    test "$(cat /sys/fs/cgroup/memory/memory.limit_in_bytes)" = 201326592
    test "$(cat /sys/fs/cgroup/memory/memory.memsw.limit_in_bytes)" = 201326592
    test "$(cat /sys/fs/cgroup/pids/pids.max)" = 36
    quota=$(cat /sys/fs/cgroup/cpu/cpu.cfs_quota_us)
    period=$(cat /sys/fs/cgroup/cpu/cpu.cfs_period_us)
fi
test "$quota" -gt 0
test "$quota" = "$period"
"#;

pub(super) struct CreateSettings {
    image: PostgresImage,
    memory: String,
    pids: String,
    owner: String,
    data: String,
    tmp: String,
    image_volume: String,
}

impl CreateSettings {
    pub(super) fn new(image: PostgresImage, token: &str) -> Self {
        Self {
            image,
            memory: WorkerLimits::MEMORY_BYTES.to_string(),
            pids: WorkerLimits::PIDS.to_string(),
            owner: format!("{OWNER_LABEL}={token}"),
            data: format!("/data:{DATA_TMPFS}"),
            tmp: format!("/tmp:{TMP_TMPFS}"),
            image_volume: format!("{}:{IMAGE_VOLUME_TMPFS}", image.volume()),
        }
    }

    pub(super) fn args<'a>(&'a self, name: &'a str) -> [&'a str; 49] {
        [
            "container",
            "create",
            "--pull",
            "never",
            "--platform",
            "linux/amd64",
            "--name",
            name,
            "--label",
            &self.owner,
            "--network",
            "none",
            "--ipc",
            "private",
            "--cgroupns",
            "private",
            "--read-only",
            "--security-opt",
            "no-new-privileges",
            "--cap-drop",
            "ALL",
            "--pids-limit",
            &self.pids,
            "--memory",
            &self.memory,
            "--memory-swap",
            &self.memory,
            "--cpus",
            "1",
            "--user",
            RUN_AS,
            "--tmpfs",
            &self.tmp,
            "--tmpfs",
            &self.image_volume,
            "--tmpfs",
            &self.data,
            "--log-driver",
            "none",
            "--no-healthcheck",
            "--restart",
            "no",
            "--ulimit",
            "core=0:0",
            "--shm-size=16m",
            "--entrypoint=/bin/sh",
            self.image.config_id(),
            "-ec",
            BOOTSTRAP,
        ]
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct RuntimeInfo {
    #[serde(rename = "OSType")]
    os: String,
    architecture: String,
    memory_limit: bool,
    swap_limit: bool,
    #[serde(rename = "CpuCfsQuota")]
    cpu_quota: bool,
    pids_limit: bool,
    cgroup_driver: String,
    cgroup_version: String,
    security_options: Vec<String>,
}

impl RuntimeInfo {
    pub(super) fn supports_controls(&self) -> bool {
        self.os == "linux"
            && self.architecture == "x86_64"
            && self.memory_limit
            && self.swap_limit
            && self.cpu_quota
            && self.pids_limit
            && matches!(self.cgroup_driver.as_str(), "cgroupfs" | "systemd")
            && matches!(self.cgroup_version.as_str(), "1" | "2")
            && self
                .security_options
                .iter()
                .any(|item| item == "name=seccomp,profile=builtin")
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct ImageInfo {
    id: String,
    architecture: String,
    os: String,
    config: ImageConfig,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ImageConfig {
    volumes: BTreeMap<String, Value>,
}

impl ImageInfo {
    pub(super) fn matches_pin(&self, image: PostgresImage) -> bool {
        self.id == image.config_id()
            && self.architecture == "amd64"
            && self.os == "linux"
            && self.config.volumes.len() == 1
            && self.config.volumes.contains_key(image.volume())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct ContainerInfo {
    #[serde(flatten)]
    pub(super) identity: ContainerIdentity,
    host_config: HostConfig,
    mounts: Option<Vec<Value>>,
    network_settings: NetworkSettings,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct ContainerIdentity {
    pub(super) id: String,
    name: String,
    image: String,
    config: ContainerConfig,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ContainerConfig {
    user: String,
    labels: BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct NetworkSettings {
    ports: Option<BTreeMap<String, Option<Vec<Value>>>>,
    networks: Option<BTreeMap<String, Value>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct HostConfig {
    network_mode: String,
    ipc_mode: String,
    cgroupns_mode: String,
    pid_mode: String,
    #[serde(rename = "UTSMode")]
    uts_mode: String,
    readonly_rootfs: bool,
    privileged: bool,
    publish_all_ports: bool,
    port_bindings: Option<BTreeMap<String, Value>>,
    binds: Option<Vec<Value>>,
    mounts: Option<Vec<Value>>,
    volumes_from: Option<Vec<Value>>,
    cap_add: Option<Vec<Value>>,
    cap_drop: Vec<String>,
    security_opt: Vec<String>,
    devices: Option<Vec<Value>>,
    device_cgroup_rules: Option<Vec<Value>>,
    device_requests: Option<Vec<Value>>,
    sysctls: Option<BTreeMap<String, Value>>,
    group_add: Option<Vec<Value>>,
    links: Option<Vec<Value>>,
    runtime: String,
    memory: u64,
    memory_swap: u64,
    nano_cpus: u64,
    pids_limit: u64,
    shm_size: u64,
    tmpfs: BTreeMap<String, String>,
    auto_remove: bool,
    restart_policy: RestartPolicy,
    log_config: LogConfig,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RestartPolicy {
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct LogConfig {
    #[serde(rename = "Type")]
    driver: String,
}

impl ContainerIdentity {
    pub(super) fn is_owned(&self, name: &str, token: &str, image: PostgresImage) -> bool {
        self.name.strip_prefix('/') == Some(name)
            && self.image == image.config_id()
            && self.config.labels.get(OWNER_LABEL).map(String::as_str) == Some(token)
    }
}

impl ContainerInfo {
    pub(super) fn is_isolated(&self, image: PostgresImage) -> bool {
        self.identity.config.user == RUN_AS
            && self.mounts.as_ref().is_none_or(Vec::is_empty)
            && self.network_settings.ports.as_ref().is_none_or(|ports| {
                ports
                    .values()
                    .all(|port| port.as_ref().is_none_or(Vec::is_empty))
            })
            && self
                .network_settings
                .networks
                .as_ref()
                .is_none_or(|networks| networks.keys().all(|name| name == "none"))
            && self.host_config.is_isolated(image)
    }
}

impl HostConfig {
    fn is_isolated(&self, image: PostgresImage) -> bool {
        self.network_mode == "none"
            && self.ipc_mode == "private"
            && self.cgroupns_mode == "private"
            && matches!(self.pid_mode.as_str(), "" | "private")
            && matches!(self.uts_mode.as_str(), "" | "private")
            && self.readonly_rootfs
            && !self.privileged
            && !self.publish_all_ports
            && self.port_bindings.as_ref().is_none_or(BTreeMap::is_empty)
            && self.binds.as_ref().is_none_or(Vec::is_empty)
            && self.mounts.as_ref().is_none_or(Vec::is_empty)
            && self.volumes_from.as_ref().is_none_or(Vec::is_empty)
            && self.cap_add.as_ref().is_none_or(Vec::is_empty)
            && self.cap_drop == ["ALL"]
            && self.security_opt == ["no-new-privileges"]
            && self.devices.as_ref().is_none_or(Vec::is_empty)
            && self.device_cgroup_rules.as_ref().is_none_or(Vec::is_empty)
            && self.device_requests.as_ref().is_none_or(Vec::is_empty)
            && self.sysctls.as_ref().is_none_or(BTreeMap::is_empty)
            && self.group_add.as_ref().is_none_or(Vec::is_empty)
            && self.links.as_ref().is_none_or(Vec::is_empty)
            && self.runtime == "runc"
            && self.memory == WorkerLimits::MEMORY_BYTES
            && self.memory_swap == WorkerLimits::MEMORY_BYTES
            && self.nano_cpus == WorkerLimits::NANO_CPUS
            && self.pids_limit == WorkerLimits::PIDS
            && self.shm_size == SHM_BYTES
            && self.tmpfs.len() == 3
            && self.tmpfs.get("/data").map(String::as_str) == Some(DATA_TMPFS)
            && self.tmpfs.get("/tmp").map(String::as_str) == Some(TMP_TMPFS)
            && self.tmpfs.get(image.volume()).map(String::as_str) == Some(IMAGE_VOLUME_TMPFS)
            && !self.auto_remove
            && self.restart_policy.name == "no"
            && self.log_config.driver == "none"
    }
}
