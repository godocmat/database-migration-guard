#!/usr/bin/env bash
# Owns one disposable Docker daemon on a fresh Linux GitHub-hosted runner.
set -euo pipefail
umask 077

if [[ $# != 5 || "$EUID" != 0 || "$(uname -m)" != x86_64 || ! -f /sys/fs/cgroup/cgroup.controllers ]]; then
    printf '%s\n' 'The fixture requires root on a disposable cgroup v2 Linux x86-64 runner.' >&2
    exit 2
fi
if [[ "$(readlink /proc/self/ns/mnt)" = "$(readlink /proc/1/ns/mnt)" || "$(ip -o link show | wc -l)" != 1 ]]; then
    printf '%s\n' 'The fixture requires a private mount namespace and a loopback-only network namespace.' >&2
    exit 2
fi

tool=$(realpath -e -- "$1")
image=$(realpath -e -- "$2")
fixture=$(realpath -e -- "$3")
release=$4
config=$5
test -x "$tool/docker"
test -x "$tool/dockerd"
test -x "$tool/containerd"
test -x "$fixture"
test -f "$image"
[[ "$release" =~ ^(15\.20|16\.16|17\.12|18\.7)$ && "$config" =~ ^sha256:[0-9a-f]{64}$ ]]
ip link set lo up

scratch=$(mktemp -d /tmp/dmg-ci.XXXXXXXX)
daemon_pid=''
masked_containerd=false

# shellcheck disable=SC2329 # Called by the EXIT trap.
cleanup() {
    result=$1
    trap - EXIT
    if [[ "$result" != 0 && -f "$scratch/daemon.log" ]]; then
        tail -n 35 "$scratch/daemon.log" || true
    fi
    if [[ -n "$daemon_pid" ]]; then
        kill -TERM "$daemon_pid" 2>/dev/null || true
        wait "$daemon_pid" || true
    fi
    if [[ "$masked_containerd" = true ]]; then
        umount /run/containerd || result=1
    fi
    if mountpoint -q "$scratch"; then
        umount "$scratch" || result=1
    fi
    rmdir "$scratch" || result=1
    exit "$result"
}
trap 'cleanup "$?"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

mount -t tmpfs -o mode=0700,size=4G,nosuid,nodev tmpfs "$scratch"
mkdir -p "$scratch/client" "$scratch/store" "$scratch/exec"
if [[ -S /run/containerd/containerd.sock ]]; then
    # Keep dockerd's automatic discovery away from the runner's system daemon.
    mkdir "$scratch/containerd-mask"
    mount --bind "$scratch/containerd-mask" /run/containerd
    masked_containerd=true
fi
printf '%s\n' '{"features":{"containerd-snapshotter":false}}' > "$scratch/daemon.json"
socket="$scratch/docker.sock"
runtime_env=(env -i PATH="$tool:/usr/sbin:/usr/bin:/sbin:/bin" HOME="$scratch" LC_ALL=C)
client=("$tool/docker" --config "$scratch/client" --host "unix://$socket")

"${runtime_env[@]}" "$tool/dockerd" --host "unix://$socket" --config-file "$scratch/daemon.json" \
    --data-root "$scratch/store" --exec-root "$scratch/exec" --pidfile "$scratch/docker.pid" \
    --storage-driver vfs --bridge none --iptables=false --ip6tables=false --ip-forward=false \
    --ip-masq=false --userland-proxy=false > "$scratch/daemon.log" 2>&1 &
daemon_pid=$!
for attempt in {1..30}; do
    if info=$(timeout 3 "${client[@]}" info --format '{{.ServerVersion}} {{.Driver}} {{.CgroupVersion}} {{.CgroupDriver}}'); then
        break
    fi
    kill -0 "$daemon_pid" 2>/dev/null
    test "$attempt" != 30
    sleep 1
done
printf 'Docker Engine, storage driver, cgroup version and driver: %s\n' "$info"
test "$info" = '29.9.3 vfs 2 systemd'
images=$("${client[@]}" image ls --quiet)
test -z "$images"
timeout --kill-after=5s 90s "${client[@]}" image load --quiet --input "$image"
test "$("${client[@]}" image inspect --format '{{.Id}} {{.Os}}/{{.Architecture}}' "$config")" = "$config linux/amd64"

export DMG_WORKER_TEST_DOCKER="$tool/docker"
export DMG_WORKER_TEST_SOCKET="$socket"
export DMG_WORKER_TEST_POSTGRES="$release"

status=0
test_names=$("$fixture" --ignored --list)
run_case() {
    if ! grep -Fqx -- "$2: test" <<< "$test_names"; then
        printf 'Integration test is missing: %s\n' "$2" >&2
        exit 1
    fi
    if ! timeout --kill-after=5s "$1" "$fixture" --ignored --exact "$2" --nocapture; then
        status=1
    fi
    if ! containers=$("${client[@]}" container ls --all --quiet) || [[ -n "$containers" ]]; then
        printf 'Container teardown could not be verified after %s\n' "$2" >&2
        exit 1
    fi
    printf 'Verified zero containers after %s\n' "$2"
}

run_case 100s linux::tests::isolated_worker_lifecycle
run_case 90s rehearsal::tests::matrix::isolated_rehearsal_distinguishes_server_and_wall_deadlines
run_case 210s rehearsal::tests::matrix::isolated_rehearsal_matrix
exit "$status"
