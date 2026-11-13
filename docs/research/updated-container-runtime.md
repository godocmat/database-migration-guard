# Wolfi Linux x86-64 image runtime verification

The CLI and GitLab image recipes from commit `eb19d1c0c9cc58be97a42d30771c9bb094389b7a` were built with the native Linux x86-64 Guard executable from `af2140159686067995a4d374ea824400f5453d07`, SHA-256 `503ccfcd47ba561d826a4d0cea0b0da6723d260a421f26677b9bc98175093ea2`. Both recipes use the exact Linux x86-64 Wolfi base manifest, SHA-256 `7b01dd2f4364d52e4c9ceaf149eb61e5510a41a3b178f230ef785e9bf26c6c4b`. Two independent builds used the same Git archive of the reviewed recipes, staging helper, lockfiles, gate source and replay fixtures. The CLI and GitLab archives each reproduced byte for byte, including their OCI manifests, image configs and all compressed layers. The build metadata and installed APK listings also matched. [CLI recipe](../../integrations/cli/Containerfile) · [GitLab recipe](../../integrations/gitlab/Containerfile) · [Native executable](integrated-linux-verification.md)

| Image | OCI manifest digest | OCI config digest | OCI archive SHA-256 | Archive bytes |
| --- | --- | --- | --- | --- |
| CLI | `sha256:2f1057e80106f41c3d8842f19c38786b003761c50c429f452057d22b3c5d3ce5` | `sha256:e2397a78485e7b6585217d45a7cb7d291834558627d83229df761442c746d67a` | `9502c0fdfd1a9979991f08af4928847e3bf3c96c2b6da51589753b4f839066eb` | 31,563,776 |
| GitLab | `sha256:087492119ce01ebb8ad4ca1a773475975fe283f7d927f8aee57bf1db1166985f` | `sha256:59e5f73878ecde789b86483e9657219dac405c45b590fe1981c18ded96ef1d71` | `2ab2aa8819a711ccd2b06ef5f7ed3106a1e330e154da1548d1a9e921bea40909` | 50,126,336 |

## Disconnected build and verification

Each fresh Ubuntu 24.04.5 guest used kernel `6.8.0-147-generic`, two vCPUs, 8 GiB of RAM, cgroup v2, and QEMU 11.1.0 with Windows Hypervisor Platform. Each had no network device and ran the fixtures in a separate mount and loopback-only network namespace. The verified Canonical image and kernel supplied the runtime. Each guest started with a new disk overlay and an empty BuildKit state directory. A read-only ISO carried the reviewed OCI layout, package snapshot and Git archive. BuildKit 0.34.1 used a private Unix socket and an OCI worker with the native snapshotter. Each recipe loaded the base from the OCI layout, verified the role's package archives and repository index, and installed the exact package versions through the local repository with signature verification enabled. The signed snapshot and base layout traveled in one verified input archive. A separately altered package archive was rejected by the staging helper before creating a build context. [Canonical image verification](https://documentation.ubuntu.com/public-images/en/latest/public-images-how-to/verify-image-integrity/) · [BuildKit local OCI layouts](https://github.com/moby/buildkit/blob/v0.34.1/docs/reference/buildctl.md#local-sources)

The builds used the following options with each recipe and its staged context. The epoch is the reviewed recipe commit timestamp. Set the verified binary digest and source commit from above. Use the approved architecture-specific base reference including its digest. Set `local_tag` to `dmg-cli:af21401` or `dmg-gitlab:af21401` for the matching recipe.

~~~sh
base_digest=sha256:7b01dd2f4364d52e4c9ceaf149eb61e5510a41a3b178f230ef785e9bf26c6c4b
base_ref="cgr.dev/chainguard/wolfi-base@$base_digest"
buildctl --addr "$builder_socket" build --frontend dockerfile.v0 \
  --local "context=$build_context" --local "dockerfile=$recipe_directory" \
  --oci-layout "dmg-base=$pinned_base_layout" \
  --opt filename=Containerfile --opt platform=linux/amd64 --opt force-network-mode=none \
  --opt "context:$base_ref=oci-layout://dmg-base@$base_digest" \
  --opt "build-arg:DMG_BINARY_SHA256=$binary_sha256" \
  --opt "build-arg:DMG_SOURCE_COMMIT=$binary_source_commit" \
  --opt "build-arg:SOURCE_DATE_EPOCH=1794597834" \
  --metadata-file "$build_metadata" \
  --output "type=oci,name=$local_tag,dest=$archive,rewrite-timestamp=true"
~~~

The verifier checked the platform, all descriptor sizes and digests, and all uncompressed layer digests. Both images retained the base's original 11 layers. The CLI had 15 layers and the GitLab image had 16. The verifier also checked the executable bytes and mode in both images and the gate script in the GitLab image. Docker Engine 29.9.3 used a private socket and vfs store. It loaded the same image config and layers in a Docker archive wrapper and reported identical config IDs and uncompressed layer digests. Each guest exported the original OCI tar files and package listings on a dedicated block device. The host verified each exported tar and its members against the guest's SHA-256 values. [Docker image load](https://docs.docker.com/reference/cli/docker/image/load/)

Two pre-fix builds of commit `32a8f15` passed runtime checks and differed in one file, `/var/cache/ldconfig/aux-cache`. glibc keys this optional cache by filesystem device, inode, file size, and status-change time. The revised recipes remove it after APK finishes installing packages in the same build layer. Runtime verification in both guests found the auxiliary cache absent and a nonempty `/etc/ld.so.cache`. Comparing the first updated archive to the corresponding pre-fix archive for each role confirmed the auxiliary cache was the sole removed file. All retained file contents and package records matched, with file timestamps reflecting the updated recipe epoch. [glibc 2.44 cache implementation](https://sources.debian.org/src/glibc/2.44-1/elf/cache.c/#L2266)

## Runtime checks

- **CLI.** The default entrypoint returned help and version as UID and GID 65532, with `HOME=/tmp` and working directory `/workspace`. The read-only repository remained clean through exact-revision pass, deny and incomplete checks with exits 0, 1 and 3. The denial reported PG001 and PG002. Each JSON result had the expected revisions and coverage.
- **GitLab.** The default UID and GID were 0, the command was `/bin/bash`, and the image supplied no entrypoint. Bash with BusyBox `grep` passed a job-shell smoke test. `/opt/dmg` was mode 0755, its executable 0555 and its gate script 0444. Running the installed gate without CI context returned 3. All ten installed-gate replays passed as UID and GID 65532. The replays covered protected and advisory decisions, exact revisions and parents, protected policy inputs, artifacts and token isolation.
- **Git HTTPS.** Both images used Git 2.56.1 and the exact system CA bundle SHA-256 `5961c0a3c65459931c023e0075c09733f09aea4ca439cb85bc4a08bb4f3f568b`. Each fetched the expected revision from a synthetic Git remote on the guest loopback after selecting the fixture CA. Each rejected the same certificate with default trust. Transport checks used a private guest network namespace and made no external requests.
- **Installed packages.** The APK database contained 43 packages for the CLI and 54 for GitLab. Both filesystems lacked libmount and a setuid mount executable. The CLI package inventory had no Python and the image had no `python3` executable. The GitLab gate used `/usr/local/bin/python3` with Wolfi Python 3.15.2 `3.15.2_git20261113-r0`, imported `tomllib`, `_elementtree` and `pyexpat`, and reported Expat 2.10.0. ELF inspection found the extension's dependency on `libexpat.so.1` and its import of `XML_SetHashSalt16Bytes`, which the packaged library defines. The GitLab image had `libuuid` `2.42.4-r2` from the util-linux source package.

Runtime checks used read-only container filesystems and dropped capabilities. The replay had a writable job tmpfs for the pinned copied executable. No containers remained. In each run the daemon exited and the guest powered down with status 0.

| Verification input or output | SHA-256 |
| --- | --- |
| CLI recipe | `d75b44963cc068c219bba6dd69e18b51459477070dade4aa97b46ad15471cca6` |
| GitLab recipe | `0eba14d5c36ccca6c9f235745ee8c5cb163ab7b688eb3d57eb7e3e70f32f2d7d` |
| Staged CLI package checksum file | `c367f0c06a49115428fc7fe46349346181e6b1bdeb752bda1a2a99b7e974edee` |
| Staged GitLab package checksum file | `f708658277015471c6f1806ecd78cc9973567dd4b725fc1c4fc6ece35495053f` |
| Signed Wolfi repository index | `cfb798f5d8c68ebfb02105222eebdd4bc77f43e4053f9e5f5a3269a5a1747a10` |
| Combined base layout and package inputs | `66563cfe96d29adfc8312bf51bfbdc2ca848118b7cb29dcc328d42c7d99039f1` |
| BuildKit 0.34.1 archive | `a59539daf547fe0958efe2d07e41c0a6944e1112a66d7e519ec1cf5b673ef15b` |
| Docker Engine 29.9.3 archive | `3fb5d1f13014c18cc54945bc177c58a9056a482dada031e17d12cf2e6922c973` |
| Guest input source archive | `0305c637000138e0b2e433f7b79a6d5339509abe590e401038e480bb3b24e292` |
| First guest serial log | `b0fcf024b3f866f1aa5426b8df5d6867317562c00df36e100ca7166b594a160e` |
| Second guest serial log | `537069d511dd35c1b50843c25e40d3c30304358ab4475b110ffeffaa53b212dd` |
| Host archive comparison | `14c44b6bd40cdce5b1153153bdfe0d335db6954a21cd85ff0923c90392de8dde` |
| CLI installed APK listing | `598f4dc9c608763390fc8e9f6295e56a72d3883ff9ac1f2b72131a318f2e3267` |
| GitLab installed APK listing | `3a7d62751d495d2bde45cf3563228678950ef6547153c7a9feb289aea1f10089` |
