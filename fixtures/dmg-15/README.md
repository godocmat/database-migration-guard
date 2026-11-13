# Migration Guard performance benchmark

This fixture measures the release executable on generated PostgreSQL migration and evidence inputs. It needs Python 3.11 or later and Git. On Linux, install GNU time. The fixture reads only local files. Run it from a clean source revision with the pinned Rust toolchain and native parser prerequisites.

~~~sh
DOCS_RS=1 cargo build --locked --offline --release -p dmg-cli
python3 -B fixtures/dmg-15/benchmark.py \
  --binary target/release/dmg \
  --source-commit "$(git rev-parse HEAD)" \
  --rustc-version "$(rustc --version)" \
  --scratch /path/to/existing/scratch \
  --output /path/to/new-evidence.json
~~~

On Windows, pass the full source commit as `--source-commit` and use `target/release/dmg.exe`. The scratch parent and output parent must already exist. The fixture creates and cleans a unique subdirectory and writes evidence to a new output path. Select two logical CPUs with `--cpu-ids` if the first two allowed IDs share a physical core. For the Linux planning budget, run on an isolated 2-vCPU, 8-GiB x86-64 runner and record any configured memory limit. The harness itself sets CPU affinity and a per-command deadline. The machine or container supplies any memory limit.

## Workloads

| Workload | Inputs and validation |
| --- | --- |
| Worktree scan | 5,000 plain migration files totaling exactly 52,428,800 bytes on target PostgreSQL 18. The version sequence runs ten distinct DDL/DML operations per table across 500 tables, including create, alter, index, check constraint, validate, filtered update, rename, filtered delete, type change, and default change. Each file also inserts 16 synthetic rows. The bytes consist of SQL, with text column values sized to reach the target. Each run must retain complete coverage and 5,000 inventory entries. |
| Large single script | One Goose migration of exactly 8,388,608 bytes with 32 StatementBegin groups below the 256-KiB parser segment ceiling. It creates a table and inserts 2,048 synthetic rows. The result must cover the full file and all groups. |
| Saved catalog comparison | Expected, current, and target snapshots each contain eight schemas, 512 tables, 8,192 columns, 1,024 indexes, 1,016 constraints, 504 dependency edges, and 24 partition edges. Current uses a saved inspection envelope with estimates. The comparison evaluates an application contract across the eight schemas, verifies 192 drift changes and 240 proposed changes, and matches 64 reviewed column renames. |
| Saved report rendering | A production scan of ten synthetic migrations yields exactly 10,000 distinct PG006 findings. Render the resulting report to HTML, Markdown, SARIF 2.1.0, and GitLab Code Quality. Validate the human formats' omission counts and the full 10,000 results in each machine format. |
| Four concurrent scans | Four separate CLI processes read the same worktree fixture on the same two logical CPUs. Each process atomically writes a separate output. Every report must match the serial scan digest. |

The timer includes process creation, analysis or rendering, and atomic output to the fixture disk. The parser, rule engine, CLI, and serializers run through the same entry points as ordinary use. Each workload has one excluded warmup and 30 measured repetitions by default. Fixture generation leaves the input cache warm. The run does not flush the operating system cache. Timing excludes fixture generation, JSON validation, and per-run output hashing. The evidence records raw times, peak process memory, workload sizes and hashes, and outputs.

The p50 is the median. The p95 is the nearest-rank percentile. Windows reads the kernel's peak working set from the CLI process handle after exit. Linux uses GNU time's maximum resident set size. Concurrent timings describe each batch of four. The result records the maximum peak of any CLI process and the maximum sum of four individual high-water marks. The latter is an upper bound on the simultaneous total for those four processes and may exceed the actual combined peak. [Windows peak working set](https://learn.microsoft.com/en-us/windows/win32/api/psapi/ns-psapi-process_memory_counters) · [Windows CPU affinity](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-setprocessaffinitymask)


## Windows baseline

The Windows x86-64 run used Windows 11 Pro build 26200.9974, an AMD Ryzen 5 5600X with six cores and 12 logical processors, 31.9 GiB of total physical memory, and an NTFS volume on an NVMe SSD. The harness and children used logical CPUs 6 and 8 on different physical cores. The host was shared. The harness applied a 60-second deadline to each command and used the native OS memory accounting without a memory cap.

The optimized binary was built with Rust 1.100.0 (0ef1bc386) and the locked dependencies at commit `22d9b06e5dad1ee87c2c1013e440c73f116f3fb5`. It reports `dmg 0.1.0`, measures 12,838,400 bytes, and has SHA-256 `bfea4047f8419bae60043ede31a64d6a309ac4f24e38b752bd1c15c6a93b10ed`. The evidence run used Python 3.11.6 and Git 2.45.1.windows.1. The command used this argument set with the local binary, scratch directory, and evidence destination.

~~~powershell
python -B fixtures/dmg-15/benchmark.py --binary $binary --source-commit 22d9b06e5dad1ee87c2c1013e440c73f116f3fb5 --rustc-version '1.100.0 (0ef1bc386 2026-11-09)' --scratch $scratch --cpu-ids 6 8 --output $output --runs 30
~~~

| Workload | p50 (s) | p95 (s) | Max (s) | Peak per CLI (MiB) |
| --- | ---: | ---: | ---: | ---: |
| Worktree scan | 1.980 | 2.474 | 2.478 | 108.46 |
| Four concurrent scans | 3.183 | 4.052 | 4.916 | 108.40 |
| 8-MiB Goose script | 0.103 | 0.109 | 0.117 | 26.41 |
| Catalog diff | 0.146 | 0.270 | 0.285 | 27.59 |
| HTML | 0.041 | 0.045 | 0.047 | 33.84 |
| Markdown | 0.043 | 0.055 | 0.055 | 33.85 |
| SARIF | 0.112 | 0.173 | 0.186 | 33.86 |
| GitLab Code Quality | 0.064 | 0.065 | 0.066 | 33.86 |

The four-process run had a maximum sum of individual working-set peaks of 432.91 MiB. The serial scan produced 2,500 findings with complete coverage. The [raw evidence](evidence/windows-22d9b06.json) includes the warmups and each measured invocation. The 15-second p95 and 512-MiB maximum RSS planning thresholds apply to a Linux x86-64 runner with 2 vCPUs and 8 GiB of memory. Linux execution and release budget acceptance remain pending.
