# Hosted and macOS release acceptance

Use disposable pilot repositories with synthetic migrations. A public platform probe used the standalone `codex/platform-acceptance-probe` branch in the designated GitHub repository. Standard GitHub runners confirmed the Apple silicon Seatbelt control, the stock Windows Datacenter runner, and an isolated Windows Server 2025 Evaluation Core installation under KVM. The full Guard macOS package check later passed on protected `main`. Hosted merge enforcement remains pending. [GitHub platform evidence](github-platform-probe.md) · [macOS package evidence](#verified-hosted-run)

| Check | Inputs the release owner needs to supply |
| --- | --- |
| GitHub protected workflow | GitHub.com pilot repository and target branch, organization or enterprise with the required workflow rule, controlled workflow source repository and ref, accessible action repository and commit, approved runner-reachable HTTPS executable URL and SHA-256, and a maintainer and fork contributor for the test. |
| GitLab protected pipeline | GitLab Ultimate HTTPS origin, pilot project path and numeric ID, protected branch, linked security policy project and controlled CI configuration project, unprivileged Linux x86-64 Docker executor, approved registry image digest and binary SHA-256, and a source fork that can pull the image. |
| macOS arm64 | Protected release repository and Actions access or a native Apple silicon build host. For installation, use a clean Apple silicon Mac or guest with a disconnected network, the verified candidate package and manifest digest, and a reviewed arm64 rollback executable and digest. |

## Prepare the same migration cases

Start with [the plain SQL configuration and policy](../../examples/github) under `.dmg/` and add `migrations/001_base.sql` containing `CREATE TABLE public.demo (id bigint);` to the protected test branch. On a source branch use these contents for `migrations/002_candidate.sql` in successive commits. Keep each result before advancing to the next revision.

| SQL | Expected gate and merge result |
| --- | --- |
| `DROP TABLE public.demo;` | Exit 1, a PG001 finding on the migration, and a blocked merge. |
| `ALTER TABLE public.demo ADD COLUMN note text;` | Exit 0, complete coverage, and a merge permitted once other requirements pass. |
| `ALTER TABLE public.demo ADD COLUMN;` | Exit 3, incomplete coverage, and a blocked merge. |

In the denied revision, add comments to the candidate config and policy so their hashes differ from the protected inputs. Restore those files before the passing revision. Repeat the passing case after a reviewed target update and capture the new test merge. Use the offline verifier below for each run. Keep the request and job links, source and target SHAs, checked merge SHA and ordered parents, active protection settings, and JSON, Markdown and provenance artifacts.

## GitHub protected pull requests

1. Configure [the required workflow example](../../examples/github/dmg-required.yml) in the controlled workflow source. Replace the action pin, executable URL and SHA-256. The URL must serve the raw executable to the runner over HTTPS without credentials or query parameters. Keep the target's Actions access compatible with the source repository's visibility and the selected action. [GitHub workflow visibility and events](https://docs.github.com/en/enterprise-cloud@latest/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/available-rules-for-rulesets#require-workflows-to-pass-before-merging)
2. Apply the **Require workflows to pass before merging** rule to the pilot target branch and select the controlled workflow. Use **Active** for the merge enforcement evidence and a merge-capable reviewer outside any bypass list. Use ordinary pull requests with an up-to-date branch requirement. The current action expects the two-parent `pull_request` test merge. If enabling the rule on an open request, update the source branch or close and reopen it to start a run. [GitHub ruleset setup](https://docs.github.com/en/organizations/managing-organization-settings/creating-rulesets-for-repositories-in-your-organization) · [GitHub required workflow troubleshooting](https://docs.github.com/en/enterprise-cloud@latest/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/troubleshooting-rules#ruleset-workflows-for-open-pull-requests)
3. Run the three cases first in a same-repository PR, then in a fork PR. If the fork requires run approval, have a maintainer review the synthetic changes and approve the workflow. Confirm `base_commit`, `pull_head_commit` and `test_merge_commit` in provenance, the source annotation and Markdown job summary, and the expected merge state for every revision. Check access to the seven-day artifacts under the repository's settings. [Fork run approval](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/approve-runs-from-forks) · [GitHub pilot](../github-pilot.md#verification)

## GitLab protected merge requests

1. Configure the pilot branch and digest-pinned job from [the GitLab setup](../gitlab-ci.md#set-up-the-protected-job). GitLab documents that a rule matching merge request pipelines must be present directly in the application project's `.gitlab-ci.yml`. For the pilot, seed the following file. The policy's `override_project_ci` should replace this fallback with the reviewed job. GitLab 18.1 or later supports the example's variable override restriction. [Merge request pipeline configuration](https://docs.gitlab.com/ci/pipelines/merge_request_pipelines/#configure-merge-request-pipelines) · [Policy variable overrides](https://docs.gitlab.com/user/application_security/policies/pipeline_execution_policies/#variables_override-type)

   ```yaml
   workflow:
     rules:
       - if: '$CI_PIPELINE_SOURCE == "merge_request_event"'
   dmg-policy-required:
     script:
       - echo "Expected the protected Guard policy to replace this job."
       - exit 1
   ```

2. Enable merged-results pipelines and **Pipelines must succeed**. Keep **Skipped pipelines are considered successful** off. Use **Merge commit with semi-linear history** with automatic rebase disabled for this pilot. The source then has to include the current target before merging, and the gate can check the refreshed two-parent result. Disable merge trains for the tested branch. Set `ci_allow_fork_pipelines_to_run_in_parent_project` to false via the project settings API. Use a fresh fork pipeline to check this setting because prior pipelines can retain their original context. [Merged-results setup](https://docs.gitlab.com/ci/pipelines/merged_results_pipelines/#enable-merged-results-pipelines) · [Merge methods](https://docs.gitlab.com/user/project/merge_requests/methods/) · [GitLab merge checks](https://docs.gitlab.com/user/project/merge_requests/auto_merge/#require-a-successful-pipeline-for-merge) · [Fork pipeline setting](https://docs.gitlab.com/ci/pipelines/merge_request_pipelines/#prevent-pipelines-from-fork-projects)
3. Run the cases in a same-project MR. Confirm the selected pipeline is a merged result, the protected policy job replaces the fallback, and `base_commit`, `source_commit` and `scan_commit` match the target, source and internal merge. In the denied revision, change the fallback job's message to a unique test marker and confirm that the protected policy's job and pins still apply. After the target moves, verify that the old passing pipeline cannot qualify the MR and that the updated branch triggers a new merged result. Check that failed and incomplete jobs retain artifacts and downloads follow `artifacts.access: maintainer`. [GitLab artifact access](https://docs.gitlab.com/ci/jobs/job_artifacts/#control-who-can-download-artifacts)
4. Run [the fork advisory example](../../examples/gitlab/fork-advisory.yml) from the source fork. Verify a detached pipeline with fork project identity, the expected diff base and source commit, and a report without a policy decision. Confirm the parent-project run is unavailable. Separately record the fork MR's merge state after the advisory passes. GitLab creates the pipeline from fork-owned configuration, and **Pipelines must succeed** checks merge request pipeline status. Require contributors' changes to enter through a reviewed same-project branch unless a trusted status check verifies the exact revision and blocks direct fork merges. [GitLab fork pipeline selection](https://docs.gitlab.com/ci/pipelines/merge_request_pipelines/#use-with-forked-projects) · [GitLab fork profile](../gitlab-ci.md#forks)

## Verify retained pilot artifacts

For each run, record the exact commits from the provider's request and job before another push refreshes the merge ref. Fetch and retain the Git commit objects locally, including both parents for a protected run. Keep the approved binary SHA-256 and the expected case from your reviewed setup and test matrix. Download artifacts from the selected job and extract them into a fresh directory.

Run the offline verifier from the Guard checkout with Python 3.11 or later and Git. `DMG_PILOT_REPO` is the local repository containing the retained commits, and `DMG_ARTIFACTS` is the directory containing `report.json` and `provenance.json` from that job.

```sh
python3 -B scripts/verify-hosted-gate.py github \
  --repo "$DMG_PILOT_REPO" --artifacts "$DMG_ARTIFACTS" \
  --base "$DMG_BASE" --source "$DMG_SOURCE" --scan "$DMG_SCAN" \
  --binary-sha256 "$DMG_BINARY_SHA256" --expect denied
```

Use `gitlab` for the protected merged-results pipeline. The protected variants read the config and policy blobs from the pinned target commit and require the target and source parents in that order. Use `gitlab-fork` for the fork advisory, set `--base` to its diff base, and supply the source commit for both `--source` and `--scan`. Use `--expect pass` for a complete advisory scan, which reads the config from the source commit and omits a policy decision. The verifier uses the documented pilot paths by default. Override them with `--config`, `--policy` for a protected check, or `--migration`.

The verifier checks the binary pin recorded by the job, the protected input and report hashes, the migration's inventory digest, and the report's Git basis, decision and coverage. A denial must include PG001 on the pilot migration. An incomplete report must include a parse error on that migration. Separately compare the provenance's repository or project and request or pipeline IDs with the provider records. Use the provider's settings, job logs, and observed merge state to verify enforcement, and check report presentation and artifact access in the provider. The JSON consistency check cannot establish those hosted results.

## Native macOS arm64

Dispatch [the native package workflow](../../.github/workflows/release-verification.yml) on the protected default branch after the release commit lands. The `macos-15` job uses an arm64 runner with LLVM 18. Check the run's commit, runner image, SDK and compiler versions, the two-build comparison, the smoke and replay results, and the logged manifest and archive SHA-256. Download artifact `dmg-native-aarch64-apple-darwin-$DMG_SOURCE_COMMIT` from that run before it expires. An owner with workflow dispatch access can use the following commands. Set `DMG_RUN_ID` to the verified run's ID and `DMG_OUTPUT` to a fresh local artifact directory. [Manual dispatch](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/manually-run-a-workflow) · [macOS runner image](https://github.com/actions/runner-images/blob/main/images/macos/macos-15-arm64-Readme.md) · [Artifact download](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/download-workflow-artifacts)

```sh
gh workflow run release-verification.yml --repo "$DMG_RELEASE_REPO" --ref "$DMG_DEFAULT_BRANCH"
gh run download "$DMG_RUN_ID" --repo "$DMG_RELEASE_REPO" \
  --name "dmg-native-aarch64-apple-darwin-$DMG_SOURCE_COMMIT" --dir "$DMG_OUTPUT"
```

### Hosted clean-runner simulation

When a disconnected Apple silicon host is unavailable, dispatch [the macOS install acceptance workflow](../../.github/workflows/macos-install-acceptance.yml) from a reviewed commit on the protected default branch. The repository needs Actions access and Git history containing the pinned `c4974cd11180440d710c0843a952caa11ce786e3` pilot. Use a repository where the source and unsigned artifact visibility have been approved. Run `gh workflow run macos-install-acceptance.yml --repo "$DMG_RELEASE_REPO" --ref "$DMG_DEFAULT_BRANCH"` and record the actual run ID and commit.

The first `macos-15` job verifies the native candidate with the release staging helper and builds the pinned rollback on arm64. It retains both inputs in one artifact and passes its artifact ID, candidate archive and manifest SHA-256, and rollback executable SHA-256 to a second job in the same run. The install job downloads those bytes onto a fresh Apple silicon runner, verifies the pins and artifact source commit, and checks install, upgrade, and rollback at one installed path.

The install job applies macOS `sandbox-exec` with `(deny network*)` to each Guard command and its child processes. It first opens a temporary IPv4 loopback listener, checks that the unsandboxed connection succeeds, and requires the same connection to fail with a permission error under the sandbox. The job records the macOS and tool versions, installed binary digests, arm64 Mach-O dependencies, and local gate exits 0, 1, and 3. It retains the transcript as `dmg-macos-install-log-<commit>-<run-attempt>`. The runner stays connected for checkout and artifact transfer. This verifies a clean hosted installation with process-level network isolation. Whole-guest disconnection, signed provenance, and approval for distribution remain separate evidence. [macOS install verifier](../../scripts/verify-macos-upgrade.py) · [Hosted runner lifecycle](https://docs.github.com/en/actions/concepts/runners/github-hosted-runners)

#### Verified hosted run

On 2026-11-18, run `44406264422` passed on protected `main` at `3b0c23de74c6a7d1e03206dbd7b3d60229f14bde`. Two previous attempts hit HTTP 403 at the anonymous GitHub API endpoint for the SBOM generator's producer attestation. The successful run used the retained signed Apple silicon bundle and verified the same upstream generator archive, signer workflow, signer commit, source commit, and tag. [Passing Actions run](https://github.com/godocmat/database-migration-guard/actions/runs/44406264422) · [Producer bundle](../../scripts/attestations/cargo-cyclonedx-0.5.9-aarch64-apple-darwin.jsonl)

The first standard `macos-15` runner reproduced the candidate across two native builds and built rollback from `c4974cd`. A separate standard runner on macOS 15.8.2 arm64 verified the uploaded files and their digests. The baseline loopback connection succeeded and the same connection was blocked with `EPERM` under Seatbelt. The runner verified install, upgrade, and rollback. Each phase passed the local GitHub replay with exits 0, 1, and 3. The candidate also passed all three saved-report smoke cases and all ten local GitLab replay tests. The candidate imports SystemConfiguration.framework, CoreFoundation.framework, `libiconv.2.dylib` and `libSystem.B.dylib`. The rollback fixture imports the two dylibs. Both executables carry linker-generated ad hoc signatures. [Packaging job](https://github.com/godocmat/database-migration-guard/actions/runs/44406264422/job/135162193608) · [Install job](https://github.com/godocmat/database-migration-guard/actions/runs/44406264422/job/135166754673) · [Exact notice inventory](dmg-16-distribution-review.md#macos-arm64-notice-draft)

Both downloaded artifact ZIPs matched the digests published by GitHub. The archive, manifest, package files, and rollback executable matched the job pins, and both executable headers identified arm64 Mach-O. GitHub set a 14-day retention period for the uploads. [Candidate and rollback](https://github.com/godocmat/database-migration-guard/actions/runs/44406264422/artifacts/14476020974) · [Transcript](https://github.com/godocmat/database-migration-guard/actions/runs/44406264422/artifacts/14476220859)

| Retained file | SHA-256 |
| --- | --- |
| Candidate package archive | `b2f5dfd6798de074e81c4b9facdbdeb3dcf11520f549a7b3ba30ae3140d0c07c` |
| Candidate package manifest | `1413b7a1b8a357c0f1584232884aca0a3e64117a7e4ec72e1da2a5e6da3516c6` |
| Candidate executable | `95eb4233ab8793850ad6272eb3f4c71da1ef2fbc7318e5db67174fb63a725e72` |
| Rollback executable | `0faab8a82b7e766675ec1948014494c98a41a05e5485be01121e051343599a59` |
| Install transcript | `8ce8ce05485696086823031994ce703e732628571459f6402f296effc8353f16` |

### Local native build and disconnected installation

For a local native build, use a clean checkout at the reviewed revision, native Rust from the checked-in toolchain, Xcode Command Line Tools and arm64 LLVM 18. Follow the workflow's LLVM configuration and run `python3 -B scripts/verify-release-ci.py aarch64-apple-darwin "$DMG_SOURCE_COMMIT" "$DMG_OUTPUT/dmg-native.tar.gz"` with an existing output directory after installing the [hash-pinned Python requirements](../../scripts/release-ci-requirements.txt). This helper uses GitHub CLI to verify the generator attestation and needs network access during staging to fetch Cargo dependencies, the reviewed SBOM generator and its attestation, and the pinned schema files. [Native build prerequisites](../../CONTRIBUTING.md#native-postgresql-parser) · [Package workflow details](release-verification-ci.md#native-packages)

Use a previously verified macOS arm64 executable for rollback. If only the source pilot is available, check out `c4974cd11180440d710c0843a952caa11ce786e3` in a clean native worktree. With the same LLVM setup, run `cargo fetch --locked --target aarch64-apple-darwin` and `DOCS_RS=1 cargo build --locked --offline --release --target aarch64-apple-darwin -p dmg-cli` there. Use [the pilot staging helper](../../scripts/stage-pilot.py) with `--source` pointing to that checkout, `--binary` pointing to its `target/aarch64-apple-darwin/release/dmg`, `--target aarch64-apple-darwin` and a new `--output` directory. Record the resulting manifest and executable digests through the same review channel as the candidate. Treat the rebuilt pilot as a locally reviewed rollback fixture.

Verify the archive against the digest from the run or local staging log before extracting it into a new directory. Stage the verified package, rollback executable, reviewed source containing the verification scripts, Python 3.11 or newer, Git 2.43 or newer, and the Apple tools `lipo` and `otool` on a clean macOS arm64 system with no Guard installation. Record `uname -m`, `sw_vers`, the test tools' versions and how networking was disconnected. Run from the reviewed source directory after disconnecting. Set `DMG_CANDIDATE` to the extracted candidate directory, `DMG_CANDIDATE_BINARY` to the executable named in its verified manifest, and `DMG_PREVIOUS_BINARY` to the retained arm64 pilot. Supply both binary SHA-256 pins and the candidate manifest pin from the trusted record.

```sh
set -eu
test "$(uname -m)" = arm64
python3 -B scripts/stage-release.py verify --package "$DMG_CANDIDATE" \
  --manifest-sha256 "$DMG_MANIFEST_SHA256"
work="$(mktemp -d)"
active="$work/dmg"

check_install() {
  install -m 0755 "$1" "$work/dmg.next"
  printf '%s  %s\n' "$2" "$work/dmg.next" | shasum -a 256 -c -
  mv -f "$work/dmg.next" "$active"
  test "$(lipo -archs "$active")" = arm64
  otool -L "$active"
  "$active" --version
  python3 -B integrations/github/tests/replay.py --binary "$active"
  printf '%s  %s\n' "$2" "$active" | shasum -a 256 -c -
}

check_install "$DMG_PREVIOUS_BINARY" "$DMG_PREVIOUS_SHA256"
check_install "$DMG_CANDIDATE_BINARY" "$DMG_CANDIDATE_SHA256"
python3 -B scripts/smoke-release.py "$active"
python3 -B integrations/gitlab/tests/replay.py --binary "$active"
printf '%s  %s\n' "$DMG_CANDIDATE_SHA256" "$active" | shasum -a 256 -c -
check_install "$DMG_PREVIOUS_BINARY" "$DMG_PREVIOUS_SHA256"
```

Keep the transcript, package manifest and tested digests. Remove the temporary installation when the check ends. Record the observed Mach-O dependencies for redistribution review. Signed provenance, the macOS distribution signing and notarization check for the approved distribution method, and publication approval remain release steps. [Mach-O architecture inspection](https://developer.apple.com/documentation/apple-silicon/building-a-universal-macos-binary) · [Apple signing and notarization](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution) · [Release acceptance](../release.md#acceptance-matrix)
