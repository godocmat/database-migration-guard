#!/usr/bin/env python3
"""Measure the production CLI on synthetic PostgreSQL review workloads."""

import argparse
from concurrent.futures import ThreadPoolExecutor
import ctypes
from ctypes import wintypes
from datetime import UTC, datetime
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import re
import shutil
import signal
import statistics
import subprocess
import sys
import tempfile
import threading
import time

MIB = 1024 * 1024
SCAN_FILES = 5000
SCAN_BYTES = 50 * MIB
LARGE_GROUPS = 32
LARGE_BYTES = 8 * MIB
FINDINGS = 10_000
SCHEMAS = [f"tenant_{i:02}" for i in range(8)]
TABLES_PER_SCHEMA = 64
REPORT_FORMATS = ("html", "markdown", "sarif", "gitlab")
WORKLOADS = ("scan", "large", "diff", "report", "concurrent")


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def file_digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def json_file(path, value):
    path.write_bytes(json.dumps(value, ensure_ascii=True, separators=(",", ":")).encode())


def read_json(path):
    return json.loads(path.read_bytes())


def scan_config(root, adapter):
    path = root / "dmg.toml"
    path.write_bytes((
        'schema_version = 1\n'
        'target = 18\n'
        'roots = ["migrations"]\n\n'
        '[adapter]\n'
        f'kind = "{adapter}"\n'
        + ("filename_pattern = '^([0-9]+)_[a-z][a-z0-9_]*\\.sql$'\n"
           'version_order = "positive_integer"\n'
           'transaction = "file"\n' if adapter == "plain" else "")
    ).encode())
    return path


def scan_args(root, config):
    return ["scan", "--repo", root, "--config", config, "--format", "json"]


def primary_statement(table, slot, row_id):
    return (
        f"CREATE TABLE {table} (id bigint PRIMARY KEY, score integer, note text NOT NULL, active boolean);",
        f"ALTER TABLE {table} ADD COLUMN review_label text;",
        f"CREATE INDEX ON {table} (score, id);",
        f"ALTER TABLE {table} ADD CONSTRAINT score_range CHECK (score >= 0) NOT VALID;",
        f"ALTER TABLE {table} VALIDATE CONSTRAINT score_range;",
        f"UPDATE {table} SET active = false WHERE id = {row_id - 16};",
        f"ALTER TABLE {table} RENAME COLUMN review_label TO label;",
        f"DELETE FROM {table} WHERE id = {row_id - 16};",
        f"ALTER TABLE {table} ALTER COLUMN score TYPE bigint;",
        f"ALTER TABLE {table} ALTER COLUMN active SET DEFAULT true;",
    )[slot]


def main_scan_fixture(root):
    migrations = root / "migrations"
    migrations.mkdir(parents=True)
    digest = hashlib.sha256()
    for offset in range(SCAN_FILES):
        table = f"public.events_{offset // 10:04}"
        row_id = offset * 16
        primary = primary_statement(table, offset % 10, row_id)
        seed = "\nINSERT INTO " + table + " (id, score, note, active) VALUES\n"
        rows = [f"({row_id + i}, {i}, 'example event payload {{}}', true)" for i in range(16)]
        insert = seed + ",\n".join(rows) + ";\n"
        base, extra = divmod(SCAN_BYTES, SCAN_FILES)
        total = base + (offset < extra)
        payload, remainder = divmod(total - len(primary + insert.format(*[""] * 16)), 16)
        labels = [("abcdefgh01234567" * (payload // 16 + 1))[:payload + (i < remainder)]
                  for i in range(16)]
        contents = (primary + insert.format(*labels)).encode()
        require(len(contents) == total, "incorrect generated scan size")
        digest.update(contents)
        (migrations / f"{offset + 1:05}_events.sql").write_bytes(contents)
    return scan_args(root, scan_config(root, "plain")), digest.hexdigest()


def large_script_fixture(root):
    migrations = root / "migrations"
    migrations.mkdir(parents=True)
    header = b"-- +goose Up\n"
    begin = b"-- +goose StatementBegin\n"
    end = b"-- +goose StatementEnd\n"
    target_size, extra = divmod(LARGE_BYTES - len(header) - LARGE_GROUPS * (len(begin) + len(end)),
                               LARGE_GROUPS)
    path = migrations / "00001_large.sql"
    with path.open("wb") as stream:
        stream.write(header)
        for group in range(LARGE_GROUPS):
            stream.write(begin)
            create = "CREATE TABLE public.large_fixture (id integer, note text);\n" if group == 0 else ""
            statements = [f"INSERT INTO public.large_fixture VALUES ({group * 64 + i}, '{{}}');\n"
                          for i in range(64)]
            payload_size = target_size + (group < extra) - len(create)
            remaining = payload_size - sum(len(statement.format("")) for statement in statements)
            value_size, remainder = divmod(remaining, len(statements))
            sql = create + "".join(statement.format("x" * (value_size + (i < remainder)))
                                   for i, statement in enumerate(statements))
            stream.write(sql.encode())
            stream.write(end)
    require(path.stat().st_size == LARGE_BYTES, "incorrect generated script size")
    return scan_args(root, scan_config(root, "goose")), file_digest(path)


def name(schema, identifier):
    return {"schema": schema, "name": identifier}


def observed(value):
    return {"state": "observed", "value": value}


def member(kind, schema, table, attribute):
    return {"kind": kind, "schema": schema, "name": table, "member": attribute}


def catalog_snapshot(stage):
    tables, dependencies, partitions, renames = [], [], [], []
    for schema in SCHEMAS:
        for index in range(TABLES_PER_SCHEMA):
            table = f"orders_{index:03}"
            parent = f"orders_{max(index - 1, 0):03}"
            renamed = stage == 2 and index % 8 == 0
            if renamed:
                renames.append({"from": member("column", schema, table, "field_16"),
                                "to": member("column", schema, table, "field_16_current")})
            columns = [{
                "name": "field_16_current" if position == 16 and renamed else f"field_{position:02}",
                "position": position,
                "data_type": name("pg_catalog", "int8" if position == 1 or (
                    position == 16 and stage > 0 and index % 4 == 0) else "text"),
                "type_modifier": -1,
                "array_dimensions": 0,
                "not_null": position == 1 or (
                    position == 10 and stage == 2 and index % 3 == 0),
                "identity": None,
                "generation": None,
                "has_default": position == 6,
                "default_definition": observed("'pending'::text") if position == 6
                else {"state": "not_applicable"},
            } for position in range(1, 17)]
            indexes = []
            for field, unique in (("field_01", True), ("field_02", False)):
                selected = "field_03" if field == "field_02" and stage > 0 and index % 8 == 0 else field
                index_name = f"{table}_{field}_idx"
                indexes.append({
                    "name": index_name,
                    "access_method": "btree",
                    "unique": unique,
                    "primary": unique,
                    "valid": True,
                    "ready": True,
                    "nulls_not_distinct": False,
                    "partial": False,
                    "keys": [{"kind": "column", "name": selected}],
                    "included_columns": [],
                    "definition": observed(f"CREATE INDEX {index_name} ON {schema}.{table} ({selected})"),
                })
            constraints = [
                {"name": "orders_pk", "kind": "primary_key", "columns": ["field_01"],
                 "validated": True, "enforced": True, "deferrable": False, "initially_deferred": False,
                 "references": None, "definition": observed("PRIMARY KEY (field_01)")},
            ]
            if index:
                constraints.append({
                    "name": "orders_fk", "kind": "foreign_key", "columns": ["field_01"],
                    "validated": True, "enforced": True, "deferrable": False, "initially_deferred": False,
                    "references": {"state": "observed", "table": name(schema, parent),
                                   "columns": ["field_01"]},
                    "definition": observed(f"FOREIGN KEY (field_01) REFERENCES {schema}.{parent} (field_01)"),
                })
                dependencies.append({
                    "dependent": member("constraint", schema, table, "orders_fk"),
                    "referenced": member("column", schema, parent, "field_01"),
                    "kind": "normal",
                })
            if 1 <= index <= 3:
                partitions.append({"child": name(schema, table), "parent": name(schema, "orders_000"),
                                   "detach_pending": False})
            tables.append({"name": name(schema, table), "kind": "partitioned" if index == 0 else "ordinary",
                           "is_partition": 1 <= index <= 3, "columns": columns,
                           "indexes": indexes, "constraints": constraints})
    snapshot = {"schema_version": 1, "schemas": SCHEMAS, "tables": tables,
                "partition_edges": partitions, "dependencies": dependencies, "gaps": []}
    return snapshot, renames


def diff_fixture(root):
    root.mkdir()
    inputs = []
    for stage, label in enumerate(("expected", "current", "target")):
        snapshot, renames = catalog_snapshot(stage)
        if label == "current":
            snapshot = {
                "schema_version": 1,
                "observation": {"target_id": "benchmark", "server_version_num": 180007,
                                "observed_at_unix_seconds": 1000},
                "scope": [{"schema": schema, "state": "visible"} for schema in SCHEMAS],
                "snapshot": snapshot,
                "relation_estimates": [
                    {"relation": table["name"], "rows": {"state": "estimated", "value": 10000},
                     "heap_bytes": {"state": "estimated", "value": 2097152},
                     "total_bytes": observed(4194304)}
                    for table in snapshot["tables"]
                ],
                "history": None,
                "gaps": [],
            }
        path = root / f"{label}.json"
        json_file(path, snapshot)
        require(path.stat().st_size < 32 * MIB, "catalog input exceeds the documented limit")
        inputs.append(path)
    mappings = root / "renames.json"
    json_file(mappings, {"schema_version": 1, "renames": renames})
    contract = root / "contract.json"
    json_file(contract, {
        "schema_version": 1, "application": "reporting", "version": "1.0", "critical": True,
        "tables": [
            {"table": name(schema, "orders_000"), "columns": [{
                "name": "field_16", "access": "read",
                "data_type": {"name": name("pg_catalog", "int8"), "type_modifier": -1,
                              "array_dimensions": 0},
            }]}
            for schema in SCHEMAS
        ],
    })
    expected, current, target = inputs
    return (["diff", "--expected", expected, "--current", current, "--target", target,
             "--proposed-renames", mappings, "--application-contract", contract, "--format", "json"],
            [{"bytes": path.stat().st_size, "sha256": file_digest(path)}
             for path in (*inputs, mappings, contract)])


def report_fixture(root, runner):
    migrations = root / "migrations"
    migrations.mkdir(parents=True)
    for index in range(10):
        (migrations / f"{index + 1:02}_update.sql").write_bytes(
            b"UPDATE public.account_ledger SET score = score + 1;\n" * (FINDINGS // 10))
    report = root / "findings.json"
    runner.run(scan_args(root, scan_config(root, "plain")), report)
    value = read_json(report)
    require(value["coverage"]["status"] == "complete" and len(value["findings"]) == FINDINGS,
            "unexpected findings fixture count or coverage")
    require(len({item["fingerprint"] for item in value["findings"]}) == FINDINGS,
            "finding fingerprints are not unique")
    return report


class WindowsMetrics:
    """Read the kernel high-water mark after exit while a handle keeps counters alive."""

    class MemoryCounters(ctypes.Structure):
        _fields_ = [("size", wintypes.DWORD), ("faults", wintypes.DWORD)] + [
            (key, ctypes.c_size_t) for key in
            ("peak_working_set", "working_set", "peak_paged_pool", "paged_pool",
             "peak_non_paged_pool", "non_paged_pool", "pagefile", "peak_pagefile")
        ]

    class SystemMemory(ctypes.Structure):
        _fields_ = [("size", wintypes.DWORD), ("memory_load", wintypes.DWORD)] + [
            (key, ctypes.c_uint64) for key in
            ("total_physical", "available_physical", "total_pagefile", "available_pagefile",
             "total_virtual", "available_virtual", "available_extended_virtual")
        ]

    def __init__(self):
        self.kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        self.kernel.GetCurrentProcess.restype = wintypes.HANDLE
        self.kernel.GetProcessAffinityMask.argtypes = [
            wintypes.HANDLE, ctypes.POINTER(ctypes.c_size_t), ctypes.POINTER(ctypes.c_size_t)]
        self.kernel.SetProcessAffinityMask.argtypes = [wintypes.HANDLE, ctypes.c_size_t]
        self.kernel.GlobalMemoryStatusEx.argtypes = [ctypes.POINTER(self.SystemMemory)]
        self.kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
        self.kernel.OpenProcess.restype = wintypes.HANDLE
        self.kernel.CloseHandle.argtypes = [wintypes.HANDLE]
        self.psapi = ctypes.WinDLL("psapi", use_last_error=True)
        self.psapi.GetProcessMemoryInfo.argtypes = [
            wintypes.HANDLE, ctypes.POINTER(self.MemoryCounters), wintypes.DWORD]

    @staticmethod
    def check(result):
        if not result:
            raise ctypes.WinError(ctypes.get_last_error())
        return result

    def available_cpus(self):
        process = self.kernel.GetCurrentProcess()
        mask, system = ctypes.c_size_t(), ctypes.c_size_t()
        self.check(self.kernel.GetProcessAffinityMask(process, ctypes.byref(mask), ctypes.byref(system)))
        return [index for index in range(mask.value.bit_length()) if mask.value & (1 << index)]

    def set_cpus(self, cpus):
        self.check(self.kernel.SetProcessAffinityMask(self.kernel.GetCurrentProcess(),
                                                      sum(1 << cpu for cpu in cpus)))

    def host_memory_bytes(self):
        counters = self.SystemMemory()
        counters.size = ctypes.sizeof(counters)
        self.check(self.kernel.GlobalMemoryStatusEx(ctypes.byref(counters)))
        return counters.total_physical

    def open(self, pid):
        # PROCESS_QUERY_INFORMATION | PROCESS_VM_READ
        return self.check(self.kernel.OpenProcess(0x0410, False, pid))

    def peak(self, handle):
        counters = self.MemoryCounters()
        counters.size = ctypes.sizeof(counters)
        self.check(self.psapi.GetProcessMemoryInfo(handle, ctypes.byref(counters), counters.size))
        return counters.peak_working_set

    def close(self, handle):
        self.check(self.kernel.CloseHandle(handle))


class Runner:
    def __init__(self, binary, root, cpus, timeout):
        self.binary, self.root, self.timeout = binary, root, timeout
        self.windows = WindowsMetrics() if os.name == "nt" else None
        if self.windows is not None:
            available = self.windows.available_cpus()
            self.host_memory_bytes = self.windows.host_memory_bytes()
            self.peak_metric = "Windows PeakWorkingSetSize"
        else:
            available = sorted(os.sched_getaffinity(0))
            self.host_memory_bytes = os.sysconf("SC_PHYS_PAGES") * os.sysconf("SC_PAGE_SIZE")
            self.time = shutil.which("time")
            require(self.time is not None, "GNU time must be installed")
            tool = subprocess.run([self.time, "--version"], capture_output=True, check=True, timeout=5)
            require(b"GNU" in tool.stdout + tool.stderr, "GNU time is required")
            self.peak_metric = "GNU time maximum resident set size"
        self.cpus = available[:2] if cpus is None else cpus
        require(len(self.cpus) == 2 and len(set(self.cpus)) == 2
                and set(self.cpus) <= set(available), "select two distinct available logical CPUs")
        if self.windows is not None:
            self.windows.set_cpus(self.cpus)
        else:
            os.sched_setaffinity(0, self.cpus)

    def run(self, args, output, barrier=None):
        command = [self.binary, *args, "--output", output]
        counter_path = output.with_suffix(".peak.txt")
        if self.windows is None:
            command = [self.time, "-f", "%M", "-o", counter_path, *command]
        if barrier is not None:
            barrier.wait(timeout=5)
        start = time.perf_counter()
        with subprocess.Popen(command, cwd=self.root, stdin=subprocess.DEVNULL,
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                              start_new_session=self.windows is None) as child:
            handle = None
            try:
                if self.windows is not None:
                    handle = self.windows.open(child.pid)
                try:
                    stdout, stderr = child.communicate(timeout=self.timeout)
                except subprocess.TimeoutExpired:
                    if self.windows is not None:
                        child.kill()
                    else:
                        os.killpg(child.pid, signal.SIGKILL)
                    child.communicate()
                    raise
                elapsed = time.perf_counter() - start
                require(child.returncode == 0 and not stdout and not stderr,
                        f"{args[0]} exited {child.returncode}: {stderr[:400]!r}")
                peak = (self.windows.peak(handle) if handle is not None
                        else int(counter_path.read_text(encoding="ascii").strip()) * 1024)
                require(peak > 0, "missing process memory counter")
            finally:
                if handle is not None:
                    self.windows.close(handle)
        return {"seconds": elapsed, "peak_bytes": peak}


def summarize(samples):
    elapsed = sorted(sample["seconds"] for sample in samples)
    return {
        "p50_seconds": statistics.median(elapsed),
        "p95_seconds": elapsed[math.ceil(len(elapsed) * 0.95) - 1],
        "max_seconds": elapsed[-1],
        "max_process_peak_bytes": max(sample["peak_bytes"] for sample in samples),
    }


def verify_scan(output, files, findings, segments=None):
    report = read_json(output)
    inventory = report["migrations"]["inventory"]
    require(report["coverage"]["status"] == "complete" and len(inventory) == files,
            "scan coverage or inventory count changed")
    require(len(report["findings"]) == findings, "unexpected scan finding count")
    if segments is not None:
        require(len(inventory[0]["source_segments"]) == segments, "large script segment count changed")
    return {"findings": len(report["findings"]), "inventory_entries": len(inventory)}


def verify_diff(output):
    report = read_json(output)
    require(report["coverage"] == "complete", "diff coverage is incomplete")
    drift = report["expected_to_current"]["changes"]
    proposed = report["current_to_target"]["changes"]
    require(len(drift) == 192 and len(proposed) == 240, "diff change counts changed")
    renames = sum(item["kind"] == "renamed" for item in proposed)
    require(renames == 64, "reviewed rename count changed")
    require(len(report["application_contracts"]) == 1, "application contract assessment missing")
    contract = report["application_contracts"][0]
    regressions = sum(check["transition"] == "regression" for check in contract["checks"])
    require(contract["current"]["status"] == "satisfied"
            and contract["target"]["status"] == "violated"
            and contract["critical_target_violation"] and regressions == len(SCHEMAS),
            "application contract did not capture the expected regressions")
    return {"drift_changes": len(drift), "proposed_changes": len(proposed), "renames": renames}


def verify_render(output, format_name):
    if format_name == "sarif":
        count = len(read_json(output)["runs"][0]["results"])
        require(count == FINDINGS, "unexpected SARIF result count")
        return {"results": count}
    if format_name == "gitlab":
        count = len(read_json(output))
        require(count == FINDINGS, "unexpected Code Quality finding count")
        return {"results": count}
    limit = 200 if format_name == "html" else 30
    require(f"{FINDINGS - limit} more findings" in output.read_text(encoding="utf-8"),
            f"{format_name} did not disclose omissions")
    return {"visible_findings": limit, "omitted_findings": FINDINGS - limit}


def measure(runner, args, output, count, validate):
    warmup = runner.run(args, output)
    validation = validate(output)
    digest = file_digest(output)
    samples = []
    for _ in range(count):
        samples.append(runner.run(args, output))
        require(file_digest(output) == digest, f"{args[0]} output changed after warmup")
    return {"warmup": warmup, "samples": samples, "summary": summarize(samples),
            "output_bytes": output.stat().st_size, "output_sha256": digest, "validation": validation}


def measure_concurrent(runner, args, root, count, expected_digest):
    outputs = [root / f"parallel_{i}.json" for i in range(4)]
    batches = []
    with ThreadPoolExecutor(max_workers=len(outputs)) as executor:
        for _ in range(count + 1):
            barrier = threading.Barrier(len(outputs))
            start = time.perf_counter()
            futures = [executor.submit(runner.run, args, output, barrier) for output in outputs]
            children = [future.result() for future in futures]
            batches.append({
                "seconds": time.perf_counter() - start,
                "peak_bytes": max(child["peak_bytes"] for child in children),
                "sum_process_peaks_bytes": sum(child["peak_bytes"] for child in children),
                "children": children,
            })
            for output in outputs:
                digest = file_digest(output)
                if expected_digest is None:
                    verify_scan(output, SCAN_FILES, 2500)
                    expected_digest = digest
                require(digest == expected_digest, "concurrent scan output differs")
    warmup, *samples = batches
    return {
        "warmup": warmup, "samples": samples, "summary": summarize(samples),
        "max_sum_process_peaks_bytes": max(sample["sum_process_peaks_bytes"] for sample in samples),
        "output_bytes_each": outputs[0].stat().st_size, "output_sha256": expected_digest,
    }


def cpu_model():
    if os.name == "nt":
        import winreg
        with winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE,
                            r"HARDWARE\DESCRIPTION\System\CentralProcessor\0") as key:
            return winreg.QueryValueEx(key, "ProcessorNameString")[0].strip()
    for line in Path("/proc/cpuinfo").read_text(encoding="utf-8").splitlines():
        key, _, value = line.partition(":")
        if key.strip() == "model name":
            return value.strip()
    return platform.processor()


def run_benchmarks(args, root):
    binary = args.binary.resolve(strict=True)
    runner = Runner(binary, root, args.cpu_ids, args.timeout)
    version = subprocess.check_output([binary, "--version"], cwd=root, text=True, timeout=10).strip()
    git = subprocess.check_output(["git", "--version"], cwd=root, text=True, timeout=10).strip()
    result = {
        "schema_version": 1, "started_at_utc": datetime.now(UTC).isoformat(timespec="seconds"),
        "source_commit": args.source_commit,
        "binary_sha256": file_digest(binary), "binary_bytes": binary.stat().st_size,
        "tool_version": version, "git_version": git, "rustc_version": args.rustc_version,
        "os": platform.platform(), "architecture": platform.machine(), "python": platform.python_version(),
        "processor": cpu_model(), "host_logical_cpu_count": os.cpu_count(), "cpu_ids": runner.cpus,
        "host_memory_bytes": runner.host_memory_bytes, "peak_memory_metric": runner.peak_metric,
        "per_command_timeout_seconds": args.timeout, "harness_memory_limit_bytes": None,
        "warmups_per_workload": 1, "cache_state": "warm after generation and one excluded run",
        "runs_per_workload": args.runs, "workloads": {},
    }
    workloads = result["workloads"]
    if "scan" in args.workload or "concurrent" in args.workload:
        command, digest = main_scan_fixture(root / "scan")
        result["scan_fixture"] = {"files": SCAN_FILES, "bytes": SCAN_BYTES, "sql_sha256_in_version_order": digest}
        if "scan" in args.workload:
            workloads["scan"] = measure(runner, command, root / "scan.json", args.runs,
                                        lambda output: verify_scan(output, SCAN_FILES, 2500))
            print(f"scan finished ({args.runs} measured runs)", file=sys.stderr, flush=True)
        if "concurrent" in args.workload:
            previous = workloads.get("scan", {}).get("output_sha256")
            workloads["concurrent"] = measure_concurrent(runner, command, root, args.runs, previous)
            print(f"concurrent scan finished ({args.runs} measured batches)", file=sys.stderr, flush=True)
    if "large" in args.workload:
        command, digest = large_script_fixture(root / "large")
        workloads["large"] = measure(runner, command, root / "large.json", args.runs,
                                     lambda output: verify_scan(output, 1, 0, LARGE_GROUPS))
        workloads["large"]["input"] = {"bytes": LARGE_BYTES, "groups": LARGE_GROUPS, "sha256": digest}
        print(f"large script finished ({args.runs} measured runs)", file=sys.stderr, flush=True)
    if "diff" in args.workload:
        command, inputs = diff_fixture(root / "diff")
        workloads["diff"] = measure(runner, command, root / "diff.json", args.runs, verify_diff)
        workloads["diff"]["inputs"] = dict(zip(("expected", "current", "target", "renames", "contract"),
                                                inputs, strict=True))
        print(f"catalog diff finished ({args.runs} measured runs)", file=sys.stderr, flush=True)
    if "report" in args.workload:
        report = report_fixture(root / "report", runner)
        result["report_fixture"] = {"findings": FINDINGS, "bytes": report.stat().st_size,
                                    "sha256": file_digest(report)}
        for format_name in REPORT_FORMATS:
            command = ["report", "--input", report, "--format", format_name]
            workloads[f"report_{format_name}"] = measure(
                runner, command, root / f"report.{format_name}", args.runs,
                lambda output: verify_render(output, format_name))
            print(f"{format_name} finished ({args.runs} measured runs)", file=sys.stderr, flush=True)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--source-commit", required=True, help="full commit ID used to build the executable")
    parser.add_argument("--rustc-version", required=True, help="version used to build the executable")
    parser.add_argument("--scratch", type=Path, required=True, help="parent directory for temporary fixtures")
    parser.add_argument("--output", type=Path, required=True, help="new JSON evidence file")
    parser.add_argument("--cpu-ids", nargs=2, type=int, help="logical CPU IDs, defaults to the first two allowed")
    parser.add_argument("--runs", type=int, default=30, help="measured runs per workload after one warmup")
    parser.add_argument("--timeout", type=int, default=60, help="per-command deadline in seconds")
    parser.add_argument("--workload", nargs="+", choices=WORKLOADS, default=WORKLOADS)
    args = parser.parse_args()
    if os.name != "nt" and sys.platform != "linux":
        parser.error("run on Windows or Linux")
    if not re.fullmatch(r"[a-f0-9]{40}|[a-f0-9]{64}", args.source_commit):
        parser.error("use a full lowercase Git commit ID")
    if args.runs <= 0 or args.timeout <= 0:
        parser.error("runs and timeout must be positive")
    if args.output.exists():
        parser.error("select a new evidence output file")
    try:
        with tempfile.TemporaryDirectory(prefix="dmg-bench-", dir=args.scratch) as directory:
            result = run_benchmarks(args, Path(directory))
        with args.output.open("x", encoding="utf-8", newline="\n") as stream:
            json.dump(result, stream, indent=2)
            stream.write("\n")
    except (OSError, RuntimeError, subprocess.SubprocessError) as error:
        parser.exit(1, f"benchmark failed: {error}\n")


if __name__ == "__main__":
    main()
