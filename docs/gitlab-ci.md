# GitLab CI

The protected GitLab job runs Guard against a same-project merge request's exact merged result. It reads the Guard project config and policy from the protected target commit. The fork example provides a source-project advisory scan. Both jobs use a digest-pinned Linux x86-64 image and retain JSON and Markdown from completed checks, including denied and incomplete decisions.

## Set up the protected job

1. Use a GitLab Ultimate project with a protected target branch. Restrict direct and force pushes to that branch and grant merge access to the appropriate reviewers. Enable merged-results pipelines and **Pipelines must succeed**. Keep skipped or absent pipelines from qualifying as successful. Verify the effective settings and pipeline selection in a test project before relying on the gate. [Protected branch settings](https://docs.gitlab.com/user/project/repository/branches/protected/) · [Merged-results pipelines](https://docs.gitlab.com/ci/pipelines/merged_results_pipelines/) · [Merge checks](https://docs.gitlab.com/user/project/merge_requests/auto_merge/#require-a-successful-pipeline-for-merge)
2. Use an isolated Linux x86-64 Docker executor with fresh jobs. Keep it unprivileged and avoid host mounts, Docker sockets, database credentials, or shared caches. Use a runner available to the intended merge request jobs. GitLab's protected runner and variable access requires both branches to be protected, along with the documented project and user checks. This job needs only its built-in job token for the Git fetch. [GitLab merge request resources](https://docs.gitlab.com/ci/pipelines/merge_request_pipelines/#control-access-to-protected-variables-and-runners)
3. Stage an approved Linux x86-64 executable and the reviewed gate script into the image described below. Copy [protected.yml](../examples/gitlab/protected.yml) to the organization-owned CI configuration repository. Replace the example image, binary checksum, numeric GitLab project ID, project path, protected target branch, and Guard input paths as needed. Pin the image by its Linux x86-64 digest. The runner must be able to pull it without giving the job a long-lived registry credential.
4. Add [policy.yml](../examples/gitlab/policy.yml) to the linked security policy project at `.gitlab/security-policies/policy.yml`. Replace the project scope ID and the CI project, file, and full commit in its include. Keep the override strategy and prohibit user variable overrides. The example replaces the project's pipeline config with the organization-owned job. Add any other required jobs through the same trusted configuration. Do not include the candidate's CI files in the override. Use the GitLab policy workflow to apply the change and confirm the policy is active. [Pipeline execution policies](https://docs.gitlab.com/user/application_security/policies/pipeline_execution_policies/) · [Security policy projects](https://docs.gitlab.com/user/application_security/policies/enforcement/security_policy_projects/)
5. Commit `.dmg/dmg.toml` and `.dmg/policy.toml` to the protected target. The [GitHub examples](../examples/github) also work for the GitLab job. Keep the migration roots, adapter, and policy review under branch protections. A candidate change to either TOML file takes effect after it lands through that review process. A workflow requiring rehearsal or approval evidence will report incomplete when that evidence is required, because this job supplies Git-backed static analysis only. [Policy evidence](policy-evidence.md)

The example uses maintainer-only artifact downloads, seven-day retention, no dependencies or caches, and `GIT_STRATEGY` set to `empty`. Confirm the effective artifact visibility and job-token access settings for your project. GitLab can retain latest successful artifacts beyond `expire_in` by default. Optional Code Quality findings may appear in the merge request according to GitLab's visibility rules. [Job artifacts](https://docs.gitlab.com/ci/jobs/job_artifacts/) · [Empty Git strategy](https://docs.gitlab.com/ci/runners/configure_runners/#git-strategy)

## Prepare the pinned image

Use the reviewed dmg binary, its independently recorded SHA-256, and the reviewed repository revision containing [the integration](../integrations/gitlab). Build with a minimal context containing only the binary and the tracked gate script. The [Containerfile](../integrations/gitlab/Containerfile) pins the Linux x86-64 Ubuntu 24.04 SCM base used by the CLI image. It uses Ubuntu-packaged Python 3.12 and Git, verifies the executable at build time, and exposes Python at `/usr/local/bin/python3` for the job templates. The job independently checks the configured binary pin before execution.

```sh
context="$(mktemp -d)"
install -Dm555 /path/to/verified/dmg "$context/dmg"
install -Dm444 integrations/gitlab/gate.py "$context/integrations/gitlab/gate.py"
docker build --platform linux/amd64 \
  --build-arg "DMG_BINARY_SHA256=$DMG_BINARY_SHA256" \
  --file integrations/gitlab/Containerfile \
  --tag dmg-gitlab-review "$context"
```

The build produces a local review image. Use an approved registry and the reviewed image digest when the release is authorized. Keep the binary checksum, image digest, and protected job configuration pinned together.

## Revision and report contract

The job checks the configured GitLab HTTPS origin, target project ID and path, pipeline source, merge request identity, and protected target branch. The fetch uses full SHA-1 `CI_COMMIT_SHA` and the two current branch refs into a private bare repository. It compares both branch tips with the pipeline SHAs and requires the merged result to have exactly the target SHA as its first parent and the source SHA as its second. GitLab documents exact-SHA fetches for merge request pipelines, with availability depending on the server retaining the commit. If a branch has moved, rerun the pipeline for the current revision. Detached merge requests, merge trains, missing objects, and non-matching parent layouts yield incomplete. The implementation targets an HTTPS origin without a path prefix and the ordinary SHA-1 repository format. [Job refspecs](https://docs.gitlab.com/ci/jobs/job_execution/#ref-specs-for-runners) · [Merge request variables](https://docs.gitlab.com/ci/variables/predefined_variables/#predefined-variables-for-merge-request-pipelines) · [Merge parent order](https://gitlab.com/gitlab-org/gitlab/-/blob/master/app/services/merge_requests/merge_to_ref_service.rb)

The job's built-in token is confined to the repository-fetch process. Git disables redirects, global config, submodule fetching, and lazy fetches. Guard and its report renderers receive a minimal environment with a private home and local Git objects. Candidate files are read as Git blobs. The job never checks out the candidate tree or runs its scripts. Keep GitLab's job-token allowlist limited and use a standard trusted TLS chain for the pinned origin. [GitLab job tokens](https://docs.gitlab.com/ci/jobs/ci_job_token/)

| File | Contents |
| --- | --- |
| `dmg-reports/report.json` | Verified Guard JSON when analysis produced a report bound to the revisions and policy |
| `dmg-reports/report.md` | Bounded Markdown for review, or a diagnostic when analysis or rendering fails |
| `dmg-reports/provenance.json` | Pipeline, project, revisions, available input and report digests, coverage, raw CLI exit, and gate exit |
| `dmg-reports/gl-code-quality-report.json` | Optional GitLab Code Quality output |

The job uses exit 0 for pass, 1 for denial, and 3 for incomplete. The provenance records the original CLI status even if the protected project maps those codes. A setup error can prevent Guard from producing JSON. In that case the job retains its Markdown diagnostic and provenance with no report digest.

To enable Code Quality, add `DMG_CODE_QUALITY` with the string value `true` to the reviewed job variables, then add the report under `artifacts.reports.codequality` and include its path under `artifacts.paths` if it should also be downloadable in the ordinary archive. The Code Quality artifact represents migration findings. Use the gate exit and its verified report for the actual decision. [Code Quality reports](https://docs.gitlab.com/ci/testing/code_quality/#code-quality-report-format) · [Guard report formats](report-formats.md)

## Forks

Keep parent-project execution of fork pipelines disabled for this profile. Review GitLab's settings and any existing rerunnable fork pipelines. The default fork job runs in the source project with its own standard job token and a public or otherwise accessible pinned image. [fork-advisory.yml](../examples/gitlab/fork-advisory.yml) reads the fork's committed config and compares its diff-base commit with its detached source commit. It runs `dmg scan` with no policy or upstream credentials. Missing Git objects result in an incomplete advisory report.

A fork owns its pipeline and can change the job or its configuration. Require a separately reviewed same-project branch to use this profile for external contributions. If you allow direct fork merges, require a separate trusted status check bound to the exact revision and verified in the target project. Establish that requirement before allowing fork merges. [Fork pipeline behavior](https://docs.gitlab.com/ci/pipelines/merge_request_pipelines/#use-with-forked-projects) · [Required status checks](https://docs.gitlab.com/user/project/merge_requests/status_checks/)

## Verify locally and in GitLab

```sh
python3 -B integrations/gitlab/tests/replay.py --binary /path/to/dmg
```

The replay uses disposable synthetic Git commits with the real CLI. It covers pass, denial, incomplete coverage, custom exit-code mapping, candidate policy edits, stale branch tips, missing commits, unexpected parents, mismatched report provenance, artifact write errors, token isolation, and fork advisory behavior. The CI files and security policy also validate against GitLab's published schemas. [CI schema](https://gitlab.com/gitlab-org/gitlab/-/blob/master/app/assets/javascripts/editor/schema/ci.json) · [Security policy schema](https://gitlab.com/gitlab-org/gitlab/-/blob/master/ee/app/validators/json_schemas/security_orchestration_policy.json)

Hosted validation is pending. In a test project verify that the active policy uses the pinned job even with a modified candidate CI file, that GitLab supplies the expected commit and parent metadata, and that passing, denied, and incomplete jobs affect the merge button as intended. Verify the fork boundary and artifact visibility on the configured GitLab version and runner before enabling enforcement.
