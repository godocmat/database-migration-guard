# Database Migration Guard security review

Reviewed on 19 November 2026 against public `main` at `328d49cbc7fb52811a5b664612dd625246cacda6`, including native-release changes merged during the review at `95f825c5d34524e2e68062155fe01cf9acea5603`. The review found two medium-severity code issues and four low-severity hardening gaps. No critical or high-severity findings were confirmed within the review scope.

## Medium

### 1. Git discovery could mistake a blocked migration root for an empty root

Git can return an empty result when a configured nested path has a file, symlink, or submodule in one of its parent components. The previous check accepted that as an empty inventory. A check could therefore pass without reviewing the migrations if the trusted base had no entries in the root, or if the policy allowed their removal and the deployment runner resolved the path differently.

Git discovery now checks the closest existing path component. A non-tree entry makes coverage incomplete and leaves the integrity comparison unresolved. The regression test uses all three non-tree Git modes and confirms that a genuinely absent root remains valid. [Git root validation](../crates/dmg-discovery/src/git.rs#L99) · [Discovery regression](../crates/dmg-discovery/tests/discovery.rs#L505)

### 2. Parser limits did not cap total work across an analysis

The parser bounded each segment, while the overall plan could retain normalized statements and related analysis state across many segments and files. Byte limits alone permitted enough small statements to cause excessive memory use or an operational failure.

Each analysis now shares a budget of 1,500,000 tokenizer tokens and 65,536 statements. Guard reports incomplete coverage at the segment that would exceed the limit and retains earlier results. The regression test covers independent token and statement exhaustion across multiple files. [Parser budget](../crates/dmg-postgres/src/parse.rs#L27) · [Parser regression](../crates/dmg-postgres/src/parse.rs#L642)

## Low

### 3. GitHub pilot setup lacked a per-file fetch limit

The GitHub pilot fetched the commit into a bare repository before the Rust scanner applied its limits. An unusually large Git object pack, including content outside the migration roots, could consume runner disk during that step.

The pilot now gives preparation and its children a 512 MiB per-file limit and respects stricter runner limits. The action also runs Python in isolated mode for both stages. [Preparation limit](../integrations/github/gate.py#L162) · [Composite action](../integrations/github/action.yml#L28)

### 4. GitHub build results were optional for the default branch

The protected branch allowed updates without the three workspace jobs passing. A merge could therefore introduce a failed build or security regression. The branch requires the existing Linux, Windows, and macOS checks from GitHub Actions, with the candidate up to date with `main`. The Linux job also replays both CI gates. [Build checks](../.github/workflows/build.yml#L11) · [Gate replays](../.github/workflows/build.yml#L79)

### 5. GitHub did not have private vulnerability reporting or dependency alerts enabled

GitHub now accepts private vulnerability reports and sends Dependabot alerts for supported dependencies. The repository documents how to report a vulnerability and deploy Guard with protected inputs. [Security policy](../SECURITY.md) · [Repository security](https://github.com/godocmat/database-migration-guard/security)

### 6. Download instructions ran the executable before checking its provenance

The original install order verified a checksum from the same release before running the download, then described the separate provenance check. The install guide now verifies the signed checksum list for the approved workflow and source commit before extraction and execution. [Download verification](../README.md#download-v010)

## Checks and scope

- Gitleaks 8.30.1 scanned the public Git history. The one alert was verified against the SHA-256 of a tracked synthetic SQL fixture. GitHub had no open secret-scanning alerts, and secret scanning and push protection were enabled. [Fixture checksums](../fixtures/dmg-02/sha256.json#L39)
- Cargo Audit 0.22.2 found zero advisories or informational warnings in the workspace and parser research lockfiles using RustSec database revision `7133866a6b44d6d3e1994ae6a48a296553c3c3da`. PyPI returned no active advisories for the five pinned release-validation packages. [RustSec database](https://github.com/RustSec/advisory-db/tree/7133866a6b44d6d3e1994ae6a48a296553c3c3da) · [Python package pins](../scripts/release-ci-requirements.txt)
- The source review covered Git and worktree discovery, SQL parsing, policy and evidence, PostgreSQL inspection, the rehearsal worker, report rendering, CI gates, and release staging. It checked the inspection TLS and read-only session controls, restricted query projections, and escaping for review output. The security-best-practices skill has no Rust or standalone Python CLI reference. [Inspection transport](../crates/dmg-inspect/src/transport.rs#L158) · [History reader](../crates/dmg-inspect/src/history/reader.rs#L79) · [Annotation rendering](../crates/dmg-report/src/github.rs#L105) · [Release verification](../scripts/stage-release.py#L216)
- The release records cover separately identified container images. Image rebuilds, deployment to a live database, and enforcement in downstream pilot repositories were outside this review. [Image advisory record](research/image-advisory-acceptance.md) · [Release acceptance](release.md#acceptance-matrix)
