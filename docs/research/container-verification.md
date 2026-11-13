# Linux x86-64 container verification

The local BuildKit build used the native Linux x86-64 release executable from `af2140159686067995a4d374ea824400f5453d07`, SHA-256 `503ccfcd47ba561d826a4d0cea0b0da6723d260a421f26677b9bc98175093ea2`. Both recipes use the Docker Official Ubuntu 24.04 `buildpack-deps:noble-scm` Linux x86-64 manifest `sha256:3499ba66ea3258e86ebedc69188f58882e2738357dea0e1583da9838d61e15bd`. The first OCI build used the exact CLI recipe from `d0a57f07aad03c50a7e41562786fbe517e7649b9`. The GitLab build used the revised recipe from `59e2172b00383ed0256eb7ccfbda57ba48dd1db4`. Host-side extraction checked that the build recipes and installed gate source matched their Git blobs byte for byte. [CLI recipe](../../integrations/cli/Containerfile) · [GitLab recipe](../../integrations/gitlab/Containerfile)

| Input | SHA-256 |
| --- | --- |
| CLI Containerfile | `ca2dac7156d0621077ebab88ed875ba6253fb2ef046c6471d5c51e02698c2576` |
| GitLab Containerfile | `e5ef6056f06b60e064a5f4cf57d695be8cac34750e7caedfc31a85da969a8925` |
| GitLab gate script | `7df72900ea98d207e03b880233d0327baef580f4517190b16e64efe1ab2bebf5` |

The two-CPU Ubuntu 24.04.5 guest had 8 GiB RAM, kernel 6.8.0-147, cgroup v2, and no network interface other than loopback. QEMU 11.1.0 used Windows Hypervisor Platform and direct kernel boot from a kernel and initrd that matched the signed Canonical cloud image. A read-only input ISO carried the base OCI layout, Git archive and build tools. BuildKit 0.34.1 used a local Unix socket, an OCI worker with the native snapshotter, and the Dockerfile frontend. Each build resolved the recipe's exact base digest from the input layout and used `force-network-mode=none`. No registry connection was needed. [BuildKit local OCI layouts](https://github.com/moby/buildkit/blob/v0.34.1/docs/reference/buildctl.md#local-sources) · [Canonical Ubuntu image verification](https://documentation.ubuntu.com/public-images/en/latest/public-images-how-to/verify-image-integrity/)

The build command used the following options with the appropriate recipe directory and minimal context. `familiar_base` is the Docker Hub reference without the `docker.io/library/` prefix, including its `@sha256:` suffix. `base_digest` is that same `sha256:` value.

~~~sh
buildctl --addr "$builder_socket" build --frontend dockerfile.v0 \
  --local "context=$build_context" --local "dockerfile=$recipe_directory" \
  --oci-layout "dmg-base=$pinned_base_layout" \
  --opt filename=Containerfile --opt platform=linux/amd64 --opt force-network-mode=none \
  --opt "context:$familiar_base=oci-layout://dmg-base@$base_digest" \
  --opt "build-arg:DMG_BINARY_SHA256=$binary_sha256" \
  --opt "build-arg:DMG_SOURCE_COMMIT=$binary_source_commit" \
  --opt "build-arg:SOURCE_DATE_EPOCH=$recipe_commit_epoch" \
  --metadata-file "$build_metadata" \
  --output "type=oci,name=$local_tag,dest=$archive"
~~~

The verifier checked each OCI manifest, config and layer size and digest, every uncompressed layer diff ID, the unchanged base layers, and the image platform. It hashed the executable and GitLab gate file inside the layers. A temporary Docker load archive referenced the identical image config and layer blobs, with a Docker `manifest.json` wrapper. Docker Engine 29.9.3 used a private Unix socket and a task-local vfs store. After loading, Docker reported the OCI config IDs and exactly the same ordered diff IDs. The guest copied the original OCI archives to a dedicated block image and powered down. Host extraction verified the exported tar and every member's SHA-256. [Docker image load](https://docs.docker.com/reference/cli/docker/image/load/)

| Image | OCI manifest | OCI config | Archive SHA-256 |
| --- | --- | --- | --- |
| CLI | `sha256:e967dbc57463067fa0a0cec52ec89304b1191be046720f3921b977138e0a19cb` | `sha256:6555ace9d604e07e1e241b5fb169db79eadbfe83e87d7b0810251ef11f6e9e9e` | `afaf4ded410c25b9c8fa370022a753b8802c3b204825d49a3befd05418efccf7` |
| GitLab | `sha256:551331d417b9819f28a25f3dbd07fe4496fe7a2eb1e17d6ce57d9a5e3a1999c5` | `sha256:7cc9f3d3ae2ecf82f3dc46c7785027144cd41e948eb531461e7565a92ac2e54a` | `5a11588479d6f7a25c0656521279f59d83c50dc4b8dfc17c9e160ee60137f7a0` |

The CLI image's normal entrypoint returned help and version. Its configured default user was UID and GID 65532, with `HOME=/tmp` and working directory `/workspace`. With the fixture Git repository owned by that UID and mounted read-only, three exact revision checks produced the expected pass, denial and incomplete results with exits 0, 1 and 3. The denial reported PG001 and PG002. The JSON carried the two exact commit IDs and expected coverage. The fixture stayed clean.

The first GitLab build created `/opt/dmg` with mode 0444 because `COPY --chmod=0444` also set the mode of the newly created parent directory. The revised recipe precreates the directory as 0755. The revised image verified the 0755 directory, 0444 gate script, 0555 executable, Ubuntu Python 3.12.3 with `tomllib`, and Git 2.43.0. A smoke run as UID and GID 65532 invoked the installed gate with isolated Python and received exit 3 when CI context was absent. All ten GitLab replay tests then passed inside the image as that same user, using the installed gate script and real executable. They covered protected and advisory outcomes, exact revisions and parent ordering, protected configuration and policy, artifact checks, and token isolation. The test used a read-only container root and fixture mount, network disabled, dropped capabilities, and a writable job tmpfs that permitted execution of the pinned copied binary. Docker inventory was empty afterward. Hosted GitLab enforcement, artifact ingestion, signed provenance, and release approval remain pending.

The task-local serial logs have SHA-256 `73d29e8ae13e6c147923908eb32b81509e43e335f582ceb5a5a846ae95ce388f` for the CLI build and smoke and `ca0f32afa05ac661569f8ac014c503532d5e5f9483bb4feb9a2b2f0e5851add0` for the corrected GitLab build and replay. The corrected run also rebuilt and rechecked the CLI with a later source-date epoch. The table retains the earlier CLI artifact and its exact tested digest.

## Image inventories and advisory review

Syft 1.57.1 scanned the exact two OCI archives. Each CycloneDX 1.5 export includes 740 filesystem components and identifies its image manifest. The exports used SPDX license names and URLs for five records with identifiers introduced after the schema's SPDX list. Both passed official CycloneDX 1.5 schema validation. The CLI SBOM SHA-256 is `b8a70444d737a89ca90f30ad306d3d88f824133959bef3af4bbbdd758d3f5309`. The GitLab SBOM SHA-256 is `004b1e183d23be92e6d134bd2fdc449c1d1fe7fcefcfdf084efdc29d5979dd79`. The native package SBOM covers the separate application source inventory. [Syft formats](https://oss.anchore.com/docs/overview/sbom/) · [CycloneDX 1.5 schemas](https://github.com/CycloneDX/specification/tree/1.5/schema) · [Native Linux package SBOM](integrated-linux-verification.md#package-verification)

Grype 0.121.3 used the pinned vulnerability database schema `v6.1.10`, built `2026-11-13T15:40:34Z`, archive SHA-256 `45d983a9834f587403cb814a0f8f6078c905a7c85a84c3c52efb994f125bcf84`. The Ubuntu package feed returned identical results for both images, with 295 Medium, 28 Low, and two Negligible matches. Its separate upstream binary CPE matches included 19 Critical and 70 High entries per image. A review against the exact installed Ubuntu package versions found 71 already covered by a vendor fix and ten listed as unaffected. Eight need review. Canonical rates all eight Medium and currently lists the `python3.12` XML advisory and the `util-linux` advisory as requiring fixes, with six other `python3.12` entries awaiting triage. The local scans keep the raw matches and the vendor reconciliation. Complete the remaining review or refresh the base before promotion. [Anchore matching guidance](https://docs.anchore.com/current/docs/vulnerability_management/how_it_works/) · [Canonical Python advisory](https://ubuntu.com/security/CVE-2026-7210) · [Canonical util-linux advisory](https://ubuntu.com/security/CVE-2026-76642)
