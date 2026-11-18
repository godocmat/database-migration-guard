# GitHub-hosted platform probe

The public synthetic probe branch uses a separate four-commit history.
Its six files are two workflow definitions, three synthetic test scripts,
and the README. GitHub made it the default branch when it was the
repository's first ref. The jobs used standard public GitHub-hosted
runners. [Probe history](https://github.com/godocmat/database-migration-guard/commits/codex/platform-acceptance-probe) ·
[Standard runner reference](https://docs.github.com/en/actions/reference/runners/github-hosted-runners#standard-github-hosted-runners-for-public-repositories)

## Runner capability results

All three jobs passed on 2026-11-18 from
[probe commit 4c29b1b](https://github.com/godocmat/database-migration-guard/commit/4c29b1bfb0332651182639a16ab25ad514f13341).
[Actions run](https://github.com/godocmat/database-migration-guard/actions/runs/44388580961)

| Requested hosted runner | Observed result |
| --- | --- |
| ubuntu-24.04 | Linux x86-64, 4 CPUs, 14 GiB available memory, 89 GiB free root storage, and a working KVM device returning API version 12. [Ubuntu job](https://github.com/godocmat/database-migration-guard/actions/runs/44388580961/job/135099747642) |
| windows-2025 | Windows Server 2025 Datacenter x64, version 10.0.26100, edition ID ServerDatacenter and Hyper-V feature installed. This job records the stock runner OS. [Windows job](https://github.com/godocmat/database-migration-guard/actions/runs/44388580961/job/135099748169) |
| macos-15 | Darwin arm64, macOS 15.8.2 build 24H232. A local TCP listener accepted an unsandboxed connection. Under a process policy denying network operations, the same connection attempt failed with EPERM. [macOS job](https://github.com/godocmat/database-migration-guard/actions/runs/44388580961/job/135099748076) |

The macOS result verifies native hardware and process network restriction
with synthetic inputs. It supports the planned install and replay test of
the actual arm64 Guard package on a fresh hosted runner.

## Windows Server 2025 Evaluation guest

The corrected hosted test passed from probe commit
[d851f01](https://github.com/godocmat/database-migration-guard/commit/d851f01638e9a6b80719f19adb4f184037e0d332).
The standard Ubuntu 24.04 runner used QEMU 8.2.2, KVM, and UEFI.
It fetched 162 HTTPS ranges from Microsoft's download endpoint and
verified the 8,152,356,864-byte installer ISO against this measured SHA-256.

~~~text
7b052573ba7894c9924e3e87ba732ccd354d18cb75a883efa9b900ea125bfd51
~~~

The guest started with an empty sparse 72 GiB disk and no virtual NIC.
A secondary read-only ISO supplied the answer file and synthetic probe.
Windows Setup installed Standard Evaluation Server Core x64 (WIM index 1).
After about 9 minutes 27 seconds, SetupComplete reported product version
10.0.26100, update build revision 32230, zero physical network adapters,
zero default routes, and the expected synthetic file SHA-256 of
32b472250f1fd90b575707e2c513e40979e45f526689faeedbd3e69bd15d21b7.
The workflow checked those values and concluded as successful. The
evaluation job covers OS installation and network isolation. The Guard
package replay is a separate release check. [Successful evaluation job](https://github.com/godocmat/database-migration-guard/actions/runs/44393283153/job/135116371541) ·
[Microsoft evaluation download](https://www.microsoft.com/en-us/evalcenter/download-windows-server-2025)

The first hosted run booted Windows Boot Manager from the new GPT disk and
reached its 25-minute deadline without a SetupComplete result. The first
two answer files used a 283-character Setup specialize command. Local
Panther logs recorded rejection of an overlong value, and Microsoft
documents a 259-character limit. The successful workflow calls a staging
batch file from a 120-character command. The superseded parallel retries
were cancelled after the successful result. [First evaluation job](https://github.com/godocmat/database-migration-guard/actions/runs/44389852633/job/135104226994) ·
[Microsoft Setup command limit](https://learn.microsoft.com/en-us/windows-hardware/customize/desktop/unattend/microsoft-windows-deployment-runsynchronous-runsynchronouscommand-path)

The retained successful GitHub CLI log copy
windows-evaluation-44393283153.log has SHA-256
f0f7e2363471628c50a0c08a8a6b8e84b71f6802a38be56f3ad60a4f7a8179ff.
