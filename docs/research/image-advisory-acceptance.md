# Wolfi image advisory acceptance

The Linux x86-64 CLI and GitLab images built from recipe revision `eb19d1c0c9cc58be97a42d30771c9bb094389b7a` contain the native Linux `af21401` Guard executable, SHA-256 `503ccfcd47ba561d826a4d0cea0b0da6723d260a421f26677b9bc98175093ea2`. Both derive from the Cosign-verified Wolfi base `sha256:7b01dd2f4364d52e4c9ceaf149eb61e5510a41a3b178f230ef785e9bf26c6c4b`. The installed package inventories match the reviewed base and the signed repository snapshot. [Runtime and reproducibility](updated-container-runtime.md) · [Native package evidence](integrated-linux-verification.md) · [Build and signature verification](../release.md#retain-the-container-build-inputs)

| Image | OCI manifest | OCI archive SHA-256 | Installed APKs |
| --- | --- | --- | ---: |
| CLI | `sha256:2f1057e80106f41c3d8842f19c38786b003761c50c429f452057d22b3c5d3ce5` | `9502c0fdfd1a9979991f08af4928847e3bf3c96c2b6da51589753b4f839066eb` | 43 |
| GitLab | `sha256:087492119ce01ebb8ad4ca1a773475975fe283f7d927f8aee57bf1db1166985f` | `2ab2aa8819a711ccd2b06ef5f7ed3106a1e330e154da1548d1a9e921bea40909` | 54 |

## Disposition of the original eight matches

The two previous Noble images contained Python 3.12 and util-linux mount components. The replacement CLI omits Python. GitLab uses `python-3.15-base` and `python-3.15-runtime` at `3.15.2_git20261113-r0`. The authenticated Wolfi recipe identifies CPython commit `64b5cfdf7548a21741398ce0826728818327aea7`, a descendant of the patched 3.15.2 release. [Original advisory review](image-advisory-vendor-review.md) · [CPython 3.15.2 release](https://blog.python.org/2026/10/python-3152-31410-31318-31216-31118/) · [CPython ancestry](https://github.com/python/cpython/compare/v3.15.2...64b5cfdf7548a21741398ce0826728818327aea7)

| Original match | Evidence in the rebuilt images |
| --- | --- |
| CVE-2026-11940, CVE-2026-11972, CVE-2026-19445, CVE-2026-19553 | CLI omits Python. The GitLab Python source descends from 3.15.2, which includes all four fixes. |
| CVE-2026-82049 | CLI omits Python. GitLab uses Python 3.15, outside the publisher's affected ranges. [Python CNA record](https://www.cve.org/CVERecord?id=CVE-2026-82049) |
| CVE-2026-7210 | The installed GitLab Python imports `pyexpat` and `_elementtree` and reports Expat 2.10.0. Its `pyexpat` extension links to `libexpat.so.1` and imports `XML_SetHashSalt16Bytes`, which the bundled library exports. [CPython XML fix](https://github.com/python/cpython/issues/149018) |
| CVE-2026-115557 | The GitLab CPython source includes the 3.15.2 denylist fix. Both images have system CA bundle `20261020-r0`, SHA-256 `5961c0a3c65459931c023e0075c09733f09aea4ca439cb85bc4a08bb4f3f568b`. The GitLab pip wheel contains certifi `2026.10.23` with PEM SHA-256 `2ef52ad56b4803e264045c915e55c236cbfa045b84b81efefc218a19eb44bd2b`. Both distinct bundles have 118 roots and exclude emSign G3, C1, C3 and Comodo AAA Certificate Services. [CPython release](https://blog.python.org/2026/10/python-3152-31410-31318-31216-31118/) · [Certifi advisory](https://github.com/certifi/python-certifi/security/advisories/GHSA-6q48-6c7w-9gw2) |
| CVE-2026-76642 | Both images omit the util-linux mount frontend and libmount. The GitLab image includes only the `libuuid` subpackage from util-linux `2.42.4-r2`, newer than the upstream 2.42.3 fix. [util-linux 2.42.3 release notes](https://github.com/util-linux/util-linux/blob/v2.42.3/Documentation/releases/v2.42.3-ReleaseNotes) |

## Exact scans and upstream cross-checks

Syft 1.57.1 cataloged each OCI archive. Both CycloneDX 1.5 SBOMs pass the official schema, identify the correct image manifest, and enumerate 43 and 54 packages plus discovered files. Grype 0.121.3 reported zero matches on each full Syft inventory without a custom exclusion. It used the same offline v6.1.10 database as the Noble scan, built `2026-11-13T15:40:34Z` from archive SHA-256 `45d983a9834f587403cb814a0f8f6078c905a7c85a84c3c52efb994f125bcf84`. [CycloneDX 1.5 schema](https://github.com/CycloneDX/specification/tree/1.5/schema) · [Grype matching](https://docs.anchore.com/current/docs/vulnerability_management/how_it_works/)

| Image | CycloneDX 1.5 components | CycloneDX SHA-256 | Grype JSON SHA-256 |
| --- | ---: | --- | --- |
| CLI | 647 | `5490d1322c77f2c10592c7544b63fa6fa42352f4c80e43a1878f0d00cf9e2633` | `c1cb5d7b45b169a897479cce59e8719bc70386137683a218368b8bb5b62e3742` |
| GitLab | 2063 | `879e89a5f8911af229f266e744135a1c78165d73686737a9ff8d1ad34a86d8fd` | `6d0bd8f9e700885ca81ef196756893646e664d8678da7b0eee3bc019bfa7e1e2` |

The split Wolfi Python packages do not match the generic CPython CPE automatically. An explicit upstream `python:python:3.15.2` CPE lookup returned CVE-2026-116566 (Medium) and CVE-2026-103995 (Low). The installed Python descends from the 3.15 branch fix for CVE-2026-116566. CVE-2026-103995 remains a Low advisory in `http.client`. The gate uses Git's HTTPS helper for remote fetches and does not use that Python client. Retain the Low advisory in release review. [CPython 3.15 fix](https://github.com/python/cpython/commit/85736acae75467a6ec565f6b609147e8ae8655fc) · [Fix ancestry](https://github.com/python/cpython/compare/85736acae75467a6ec565f6b609147e8ae8655fc...64b5cfdf7548a21741398ce0826728818327aea7) · [Python CNA record for CVE-2026-103995](https://cveawg.mitre.org/api/cve/CVE-2026-103995)

The exact GitLab image contains the reviewed pip wheel with SHA-256 `38f4aacff7c3b0d7b216cac0a106a1036eef98b0a7ff5b932ea2fcb787e97bc5`. A separate lookup of pip and all 18 dependencies named in its embedded vendor inventory returned three setuptools version matches. The wheel retains `pkg_resources` and omits the affected setuptools package index, sdist and legacy wheel installers. The High, Medium and Low version matches therefore do not apply to the shipped files. Both images also have `sqlite-libs` 3.54.1-r0 without the `sqlite3` shell affected by CVE-2026-119586. [pip vendoring policy](https://github.com/pypa/pip/blob/26.2.2/src/pip/_vendor/README.rst) · [PackageIndex advisory](https://github.com/pypa/setuptools/security/advisories/GHSA-5rjg-fvgr-3xxf) · [sdist advisory](https://github.com/pypa/setuptools/security/advisories/GHSA-h35f-9h28-mq5c) · [Legacy wheel advisory](https://github.com/pypa/setuptools/security/advisories/GHSA-xmj3-9gf7-h52w) · [SQLite vendor record](https://sqlite.org/cves.html)

The standalone CPython CPE report has SHA-256 `42df409e5b8a2766f889f9e46d61a8ac1b9e79767cdf7aaed91de9ae6914930b`. The wheel dependency report has SHA-256 `ca9deb211e3891315ce95c52ab973ae401ef7ff9bbd665e9fc8ee7a89204f1f3`. The exact GitLab image still matches the separately reviewed pip wheel and installed Python and mpdecimal notice bytes. Approval of the image notices and corresponding-source bundle remains part of distribution review. [Image redistribution review](image-redistribution-review.md)

## Retained build inputs

| Input | SHA-256 |
| --- | --- |
| `wolfi-amd64-snapshot.tar` with the signed index and all 35 locked APKs under `x86_64/` | `65d11211c5d78e0df0e4ac87a8169cf6baaf5e3eb10f17854a60591919a93e66` |
| Signed `x86_64/APKINDEX.tar.gz` | `cfb798f5d8c68ebfb02105222eebdd4bc77f43e4053f9e5f5a3269a5a1747a10` |
| `wolfi-amd64-base-layout.tar` with the pinned manifest, config and layers | `3ebe1042864af9a20d5d8be43c74ab7b733133797420d575bd52062b4a49cc9c` |

The public Wolfi index had rolled forward by the time of the build. The retained signed snapshot reproduced the package locks for both roles. A modified index or APK and a missing APK failed staging without leaving a partial output. Keep the retained inputs with the release candidate for disconnected builds. [Offline build steps](../release.md#retain-the-container-build-inputs)
