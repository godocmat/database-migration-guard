# Disconnected Windows x86-64 installation check

The Windows package still needs an install, upgrade, and rollback on an authorized clean Windows x86-64 guest with its virtual NIC disconnected. The host used for the local replay had Windows 11 Pro and an existing Visual C++ runtime. On this host, Windows Sandbox is disabled and Hyper-V inventory requires administrator access. Microsoft’s Server evaluation route requires registration. Its public Windows 11 x64 ISO uses a product key to select the edition, and the Azure trial route requires account registration and identity verification. A prepared Windows VM or an already-enabled Windows Sandbox on a suitable host supplies the remaining environment. [Windows Sandbox setup](https://learn.microsoft.com/en-us/windows/security/application-security/application-isolation/windows-sandbox/windows-sandbox-install) · [Windows Server evaluation](https://www.microsoft.com/en-us/evalcenter/evaluate-windows-server-2025) · [Windows 11 download](https://www.microsoft.com/en-us/software-download/windows11) · [Azure account requirements](https://azure.microsoft.com/en-us/pricing/purchase-options/azure-account#faq)

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

The helper passed on the existing Windows 11 Pro x86-64 host with Git for Windows 2.45.1, Python 3.11.6, UCRT 10.0.26100.9711 and Visual C++ runtime 14.51.36247.0. The host retained its network configuration. The helper also rejected an incorrect manifest digest, a modified SBOM, and an incorrect rollback executable digest before invoking the package binaries. The disconnected guest acceptance remains pending.
