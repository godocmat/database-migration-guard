# GitHub Actions pilot

Use the [workflow template](../examples/github/dmg-required.yml) from an organization-controlled workflow source repository. The composite action and the Linux x86-64 executable each have their own immutable pin. The template targets GitHub.com pull requests on the protected branch. It uses a GitHub-hosted Ubuntu 24.04 runner, read-only repository contents, and the workflow's built-in token during the Git fetch. The check and render processes run without that token, user secrets, database credentials, or a checkout of contributor files. [GitHub workflow rules](https://docs.github.com/en/enterprise-cloud@latest/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/available-rules-for-rulesets#require-workflows-to-pass-before-merging) · [GitHub token permissions](https://docs.github.com/en/actions/tutorials/authenticate-with-github_token)

## Stage and verify a local binary

From a clean checkout of the reviewed build revision, install its pinned toolchain and the [native parser prerequisites](../CONTRIBUTING.md#native-postgresql-parser). Use the native compiler for the package target and build `dmg-cli` with its checked-in lockfile. For a Linux x86-64 pilot, run this on the Ubuntu 24.04 baseline.

```sh
DOCS_RS=1 cargo build --locked --release -p dmg-cli
python3 scripts/stage-pilot.py \
  --target x86_64-unknown-linux-gnu \
  --binary target/release/dmg \
  --output /path/to/new-pilot-package
```

The staging script requires Python 3.11 or newer and a new output directory. For a native Windows package use `x86_64-pc-windows-msvc` and `target/release/dmg.exe`. For a verified native macOS arm64 build use `aarch64-apple-darwin`. The script copies the executable, verifies that it runs and reports a version, and writes `package.json` and `SHA256SUMS`. `package.json` records the supplied build target, the source checkout's exact commit, and the lockfile, toolchain, and executable digests. Use the executable built from that checkout and verify the result on a clean host. The metadata describes the staging inputs and is unsigned.

```sh
cd /path/to/new-pilot-package
sha256sum -c SHA256SUMS
./dmg-0.1.0-x86_64-unknown-linux-gnu --version
```

The pilot needs an approved HTTPS download that the runner can reach without an account credential. Stage the raw executable under the filename in the checksum list. Retain the reviewed metadata and pin the expected digest in the protected workflow independently of the download. An organization can use an existing approved artifact host. The local staging command creates files only. Release publication is a separate step. [GitHub action pinning](https://docs.github.com/en/actions/reference/security/secure-use#using-third-party-actions)

## Adopt the protected workflow

1. Add [the example configuration](../examples/github/dmg.toml) and [the example policy](../examples/github/policy.toml) to the pilot repository's protected base branch as `.dmg/dmg.toml` and `.dmg/policy.toml`. Set the declared PostgreSQL major, migration roots, adapter, and transaction mode to match the repository. The example blocks HIGH and CRITICAL findings and changes to established migration files.
2. Copy the [workflow template](../examples/github/dmg-required.yml) into `.github/workflows/` in the organization's workflow source repository. Replace the action repository, full commit SHA, asset URL, and executable SHA-256 in the template. The action repository must be accessible to the pilot runner through the organization's Actions settings. Restrict changes to the workflow source and pilot repository's base config and policy to the responsible maintainers. Require their review on policy changes before those changes become base policy.
3. Where supported, configure an organization or enterprise branch ruleset to **Require workflows to pass before merging** for the chosen pilot repositories and branches. Select the workflow in the organization-controlled source repository, protect its source ref, and activate the ruleset after the real PR checks below. Require pull requests, keep the branch up to date with its protected base, and scope bypass permissions according to the organization's approval model. Re-evaluate the check after a base or candidate revision changes. [Configuring required workflows](https://docs.github.com/en/code-security/how-tos/secure-at-scale/configure-organization-security/configure-specific-tools/enforce-dependency-review) · [Ruleset troubleshooting](https://docs.github.com/en/enterprise-cloud@latest/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/troubleshooting-rules)

Hosted enforcement depends on that ruleset. A copy of the workflow in a pilot repository can provide advisory PR feedback. A job name alone cannot protect the workflow definition. This template uses the `pull_request` event and requires the corresponding test merge. Validate a separate `merge_group` integration before using a merge queue on the same protected branch.

## Revisions, policy and evidence

The action fetches the GitHub test-merge ref into a new bare repository at depth two. Setup caps each file Git can write at 512 MiB, including a pack containing objects outside the migration roots. It verifies the test-merge SHA from the workflow context and the event's base and candidate head SHAs as its first and second parents. It compares the protected base tree with the test-merge tree, which includes migrations already present on the base branch. The check fails if setup cannot retrieve the exact merge within its fetch limits. [Pull request revisions](https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows#pull_request) · [Git fetch](https://git-scm.com/docs/git-fetch)

Configuration and policy are read as ordinary Git blobs from the verified base SHA. Candidate edits to either file take effect after they become reviewed base content. The pilot stages the HTTPS executable, checks the pinned SHA-256 before execution, and runs Guard with the downloaded Git objects and protected inputs. The scanner reads those local bytes, so the analysis needs neither database nor network access. The hosted workflow needs network access for setup and artifact upload.

The required job passes only with CLI exit 0 and a verified `pass` decision on the expected revision, policy digest, and complete coverage. It preserves a nonzero portable check exit. The default configuration uses exit 1 for denial, 2 for invalid configuration, and 3 for incomplete or operational failure. A malformed or missing successful report also fails the job. Rendering adds a Markdown job summary and escaped, bounded workflow annotations. Rendering errors preserve the original decision and leave the validated JSON and provenance available.

The action uploads `provenance.json` and, when available, `report.json` and `report.md` for seven days. Provenance binds the event base, PR head and test merge commits, config, policy and executable SHA-256 digests, the check exit, gate exit, and the verified JSON report digest. The artifact contains analysis output with migration paths and object names. Keep it within the repository's approved workflow artifact access scope. The pinned upload action uses Node 24. [Job summaries and annotations](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-commands) · [Artifact retention](https://github.com/actions/upload-artifact/tree/cf430e030ddbb5b0abf93d22962f4752f3646cd9#retention-period) · [GitHub Actions runtime](https://github.blog/changelog/2026-09-23-node-20-is-no-longer-available-in-github-actions/)

## Verification

Run the local replay with a binary built from a revision containing `dmg check` and `dmg report`.

```sh
python3 -B integrations/github/tests/replay.py --binary /path/to/dmg
actionlint examples/github/dmg-required.yml
```

The replay builds synthetic Git refs, fetches them into fresh bare repositories, checks exact merge ancestry, and exercises a destructive migration, its additive correction, and a malformed migration. It asserts exits 1, 0 and 3, respectively, plus exact base policy selection, JSON and Markdown retention, and digest-bound provenance. The staged Windows x86-64 and Ubuntu 24.04 x86-64 releases from `c4974cd` passed checksum and version checks, the replay, and all 130 public CLI cases. The Linux replay ran with network access disabled. The template passed actionlint 1.7.12. [Package digests and host details](../PROGRESS.md#dmg-08-local-pilot-packages)

For hosted acceptance, run a same-repository PR and a fork PR on the designated pilot branch. Confirm that the destructive migration displays its source finding and blocks merge, the corrected revision passes, and an incomplete revision stays blocked. Include proposed config and policy edits in the candidate and confirm the job records the protected base hashes. Confirm the required workflow and artifact are attached to the exact latest test merge after a new base or candidate revision. The local replay exercises the action's analysis, revision and artifact inputs. GitHub's live ruleset, HTTPS setup and fork execution remain to be verified in a configured pilot repository.
