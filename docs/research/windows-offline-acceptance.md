# Disconnected Windows x86-64 installation check

Use an authorized clean Windows x86-64 guest to verify install, upgrade, and rollback without a virtual network adapter. The local Windows 11 Pro host had Windows Sandbox disabled and an existing Visual C++ runtime. The runbook supports Windows Sandbox on a suitable host or a separately installed Windows guest. Microsoft publishes x64 Evaluation media for Windows Server 2025 and documents the initial 10-day activation window. [Windows Sandbox setup](https://learn.microsoft.com/en-us/windows/security/application-security/application-isolation/windows-sandbox/windows-sandbox-install) · [Windows Server evaluation](https://www.microsoft.com/en-us/evalcenter/evaluate-windows-server-2025) · [Evaluation ISO download](https://www.microsoft.com/en-us/evalcenter/download-windows-server-2025)

## Run in Windows Sandbox

1. On an x86-64 host with Windows Sandbox enabled, create dedicated input and evidence directories. Copy the transfer ZIP, the portable Python and MinGit ZIPs listed below, and the reviewed `verify-offline.ps1` into the input directory. Include the pinned Microsoft redistributable if the guest needs it. Keep the evidence directory empty. [Guest runner](../../examples/windows/verify-offline.ps1)
2. Copy the `.wsb` example and set both `HostFolder` entries to the absolute paths of those directories. Open the edited `.wsb` file to start the guest. It disables networking and clipboard redirection, maps the input directory read-only, and maps the separate evidence directory with write access. [Sandbox configuration example](../../examples/windows/windows-offline.wsb) · [Microsoft configuration reference](https://learn.microsoft.com/en-us/windows/security/application-security/application-isolation/windows-sandbox/windows-sandbox-configure-using-wsb-file)
3. If the guest lacks `vcruntime140.dll` in System32, verify the staged redistributable against the pin below, copy it to guest-local storage, and install it there. Open 64-bit Windows PowerShell inside the guest and run the reviewed script.

   ```powershell
   powershell.exe -NoProfile -ExecutionPolicy RemoteSigned -File C:\dmg-windows-inputs\verify-offline.ps1
   ```

4. Check that `guard.exit.txt` records exit 0 and `guard.stdout.log` reports all three phases as verified. Keep both output streams, the guest record and the exact `.wsb` configuration with the release evidence. The guest record includes the Windows build, network adapter status, runtime and tool versions, and archive digests. The runner refuses to start the replay if it sees an active adapter or a mismatched digest.

Windows Sandbox was disabled on the verification host. A Windows PowerShell 5.1 preflight validated the configuration, extracted the pinned inputs, and ran the portable tool versions and the harness help command. It rejected an active adapter and a changed archive and verified log and exit recording with simulated replay outcomes. The full disconnected replay used the [local Windows Server evaluation guest](#local-windows-server-evaluation-guest).

## Prepare the guest and inputs

- Use an authorized Windows x86-64 guest snapshot with no existing Guard installation. Disconnect its virtual NIC before the test. If using Windows Sandbox, set `Networking` to `Disable` and map only the reviewed test inputs. A fresh snapshot allows the check to be repeated independently. [Windows Sandbox configuration](https://learn.microsoft.com/en-us/windows/security/threat-protection/windows-sandbox/windows-sandbox-configure-using-wsb-file)
- Stage the reviewed source tree containing this helper, both unsigned Guard packages, a native Python 3.11 or later interpreter, and a reviewed Git for Windows 2.43 or later installation. Stage the official signed x64 Microsoft Visual C++ v14 Redistributable if the guest lacks a suitable runtime. Record the guest image identity, OS build, Git and Python versions, UCRT and `VCRUNTIME140.dll` versions, and the method used to disconnect networking. [Microsoft x64 runtime prerequisites](https://learn.microsoft.com/en-us/cpp/windows/latest-supported-vc-redist/)
- Verify the artifact pins from the release record. The normalized candidate package from `2d3e5e0` uses manifest SHA-256 `eb875e3be11e1504d683849ff19def662c6658c7a377c7b2937db82c95f6c619`. The retained Windows pilot from `c4974cd` uses executable SHA-256 `e1c374c1a0c2b08c2fdd34f04f3ebe38dc811fbd5077000ff8f5853449be3c97`. The pilot's manifest SHA-256 is `6e39f898a0d8b08d1f2c7c316bb788afde7859284b9fb4776f1228698078f104`. [Candidate package record](integrated-windows-verification.md) · [Pilot package record](clean-install-verification.md#installation-and-rollback)

## Run the check

Run from the reviewed checkout with a fresh, writable guest scratch directory. Keep the package and pilot inputs read-only and use their staged guest paths. The command below uses the existing candidate and rollback pins.

```powershell
python -B scripts/verify-windows-upgrade.py `
  --package $candidateDirectory `
  --manifest-sha256 eb875e3be11e1504d683849ff19def662c6658c7a377c7b2937db82c95f6c619 `
  --previous $pilotExecutable `
  --previous-sha256 e1c374c1a0c2b08c2fdd34f04f3ebe38dc811fbd5077000ff8f5853449be3c97 `
  --git $gitExecutable --scratch $guestScratch
```

The helper verifies the candidate manifest, binary, SBOM and checksum list with the standard package verifier. It installs the pinned pilot, upgrades the same temporary path to the candidate, and restores the pilot. It verifies the installed digest before and after each phase. Each phase runs the local GitHub replay for exits 0, 1 and 3. The candidate also runs the installed report smoke and GitLab replay. Capture the console output and exit status alongside the guest record. The temporary installations and repositories are removed at the end. [Installed smoke](../../scripts/smoke-release.py) · [GitHub replay](../../integrations/github/tests/replay.py) · [GitLab replay](../../integrations/gitlab/tests/replay.py)

The helper passed on the existing Windows 11 Pro x86-64 host with Git for Windows 2.45.1, Python 3.11.6, UCRT 10.0.26100.9711 and Visual C++ runtime 14.51.36247.0. The host retained its network configuration. The helper also rejected an incorrect manifest digest, a modified SBOM, and an incorrect rollback executable digest before invoking the package binaries. A repeat of the integrated helper passed with log SHA-256 `94311298e3ac0539ee8c971cd7414a8b3a14816c364ce7f9a67d73a76daf6603`.

## Transfer bundle

The local `dmg-windows-offline-acceptance.zip` contains the candidate, rollback package, exact harness and replay files from `87c0e26`, checksums and a guest README. Two independent ZIP writes yielded SHA-256 `a7c5d22bbf14cfd3d839379a52846881ea6e1aba675376c4c92bdf82716bfeed`. Verify that digest before extraction and use the README to check the extracted files. The archive contains only the reviewed test inputs and requires a Windows x86-64 guest.

The exact extracted bundle passed the host replay with the separately staged portable Python 3.14.10 and MinGit 2.56.1 after adding the bundle's verified `scripts` directory to the extracted copy of Python's isolated module path. Log SHA-256 is `9f94776b79006b73e5790b30a2f7a8f40db5a7721f689293bfa9e4ee60c97641`. The optional Microsoft redistributable was signature-checked for transfer. The replay used the existing host runtime. [Python release](https://www.python.org/downloads/release/python-31410/) · [Git for Windows release](https://github.com/git-for-windows/git/releases/tag/v2.56.1.windows.1) · [Microsoft x64 runtime](https://learn.microsoft.com/en-us/cpp/windows/latest-supported-vc-redist/)

| Optional guest input | SHA-256 |
| --- | --- |
| `python-3.14.10-embed-amd64.zip` | `90905e64a840557d8c1bc4f5b74db3d7ee85e3ffb217611cac1c7813acea4159` |
| `MinGit-2.56.1-64-bit.zip` | `4a92850b6e917da4fd010669b614b55dccf5b0ec333a534cc4be4700dfefdafd` |
| `vc_redist.x64.exe` 14.52.37029.0 | `9c02c9ed120f671b900e6b9471d890e3b2414117a7e53f4ae2e855466fa2b7c9` |

## Local Windows Server evaluation guest

The local test provisioned Windows Server 2025 Standard Evaluation Core x86-64, build `10.0.26100.32230`, on 2026-11-18. Microsoft's official x64 en-US ISO was 8,152,356,864 bytes. An ordinary HTTPS download and a second download assembled from 162 checked HTTPS ranges both produced SHA-256 `7b052573ba7894c9924e3e87ba732ccd354d18cb75a883efa9b900ea125bfd51`. The host verified Microsoft's Authenticode signatures on the ISO's setup executable and EFI loader. WIM index 1 selected Standard Evaluation Core. [Evaluation ISO download](https://www.microsoft.com/en-us/evalcenter/download-windows-server-2025)

QEMU 11.1.0 used four virtual CPUs, 6 GiB of RAM, UEFI firmware, and a blank 72 GiB QCOW2. Each boot used `-nodefaults -nic none` and the QEMU monitor reported no network controller. Windows Setup applied Microsoft's unmodified image with WHPX. WHPX paused at the first guest reboot. From that checkpoint, WinPE corrected the locally supplied answer file on a new overlay before Windows Setup ran specialize. A read-only NTFS check matched the corrected file byte for byte. The installed system completed its subsequent reboot under TCG. The installed Windows console reported both `SystemSetupInProgress` and `OOBEInProgress` as zero before the staged runner started.

The installed guest added the pinned Visual C++ redistributable from its read-only inputs. The installer exited 0 without requiring a restart. The first TCG replay reached the pilot's third GitHub fixture and exited 1 with Windows error 32 during temporary repository cleanup. The retry cold-booted the completed Windows installation under WHPX on a new overlay. After signing in to Server Core, the local Administrator ran the same pinned transfer ZIP and runner in a fresh scratch directory with separate evidence. The guest recorded no visible network adapters, UCRT 10.0.26100.32230, Visual C++ runtime 14.52.37029.0, Python 3.14.10, and Git 2.56.1.windows.1.

The WHPX replay passed installation of the `c4974cd` pilot, upgrade to the `2d3e5e0` candidate, and rollback to the same pilot digest. All three phases verified the local GitHub replay's pass, denied, and incomplete outcomes with original exits 0, 1, and 3. The candidate also passed the installed smoke for those exits and saved HTML, SARIF, and GitLab Code Quality rendering, followed by all ten local GitLab replay cases. The harness returned 0 and powered down the guest. An independent check matched the four serial-exported files to their decoded copies, verified the package and fixture outcomes, and checked all four guest stages for virtual network hardware.

| Retained result | SHA-256 |
| --- | --- |
| WHPX replay stdout | `bdfef93855012e42de6021f1c089ae33e6fdf4d27e7281cb451cd16380c32a65` |
| WHPX replay stderr | `b75d705e3475cb7da36858c872b1d56d8d65385cc56300432dc9edc819516873` |
| WHPX serial export | `ca419bade1b751188063f5b0a6634b5cfe3ab48b50a8052c7e7e16c568b8467b` |
| Independent evidence record | `14adfcdd8e73bfbe03e710597d1c9937dd8d16e9fb8820891ff3ba2d92c170f0` |
| TCG replay stderr | `631fc5cd5080e1de8c34557b2216778109fde010630bc9ee8441885fb2c84de2` |
