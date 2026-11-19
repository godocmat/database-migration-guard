# Changelog

## 0.1.1

- Mark Git discovery incomplete when a file, symlink, or submodule blocks a configured migration root.
- Bound parser work across the full migration set and retain earlier findings when a scan exceeds the limit.
- Limit each file written during GitHub pilot setup to 512 MiB and run the pilot's Python steps in isolated mode.

Prebuilt downloads cover Linux and Windows x86-64 and macOS on Apple silicon. GitHub build attestations cover each archive and the Linux and Windows checksum list. [GitHub release](https://github.com/godocmat/database-migration-guard/releases/tag/v0.1.1) · [Security review](docs/security_best_practices_report.md)

## 0.1.0

The first prebuilt downloads are for Linux x86-64 and Windows x86-64. Each archive includes the CLI, checksums, a CycloneDX SBOM, and the license and third-party notices. [GitHub release](https://github.com/godocmat/database-migration-guard/releases/tag/v0.1.0)

- Review plain SQL, SQLx, Flyway, Goose, and Supabase migrations for PostgreSQL 15–18 using the documented runner profiles.
- Collect scoped catalog and native history over verified TLS. Import pinned evidence into offline checks.
- Compare expected, current, and proposed schemas. Review explicit rename mappings and check deployed application contracts.
- Plan revision-bound rehearsals and execute them on supported disposable Linux x86-64 Docker workers.
- Evaluate trusted baselines, audited approvals, and required rehearsal evidence. Render saved reports as HTML, Markdown, SARIF, or CI annotations and Code Quality as applicable.
- Set up checks using the GitHub and GitLab integration examples. Enforcement on hosted merges requires a configured pilot and validation in the target repository. [GitHub pilot](docs/github-pilot.md) · [GitLab setup](docs/gitlab-ci.md)

Ruleset 2 reports PG002 compatibility risk for table and column removal alongside the PG001 data-loss finding. Protected evidence bundles must pin the ruleset, and each blocking finding needs its own matching approval when the policy requires approval. PG004 flags a likely scan when adding a `NOT NULL` column with a literal `NULL` default, including supported casts of that literal. [Rules](docs/rules.md) · [Policy evidence](docs/policy-evidence.md)

Rehearsal reports can include `cleanup_worker` when Docker removal could not be confirmed. Older strict report decoders need an update to read a report that includes this field. [Worker recovery](docs/research/simulation-worker.md#postgresql-connection-and-teardown)

### Verification scope and limits

The public development corpus covers 130 plain SQL and SQLx combinations on PostgreSQL 17 and 18. Separate native fixtures cover supported forms in other adapters and PostgreSQL majors. [Public corpus](fixtures/dmg-02/README.md) · [Supported SQL](docs/postgresql-support.md)

An earlier independent plain SQL and SQLx holdout on PostgreSQL 17 and 18 matched all 100 gate and coverage outcomes and all 16 required destructive findings. It matched 56 of 64 required findings exactly. Four PG003 findings used relation-level identities where the labels expected columns, and four PG007 findings used wider statement spans. Six of ten diagnostics matched category and path and two matched exact spans. The holdout also recorded four extra Medium PG004 findings. A policy that denies Medium findings or requires PG004 approval can make conservative scan warnings affect the gate. [Holdout results](PROGRESS.md#dmg-15-frozen-holdout-rescore) · [Diagnostic precision](docs/postgresql-support.md)

The CLI and GitLab image recipes use the pinned Wolfi runtime with Git HTTPS. GitLab also includes Bash and Python 3.15. Offline rebuilds require the retained signed package snapshot. Container distribution has a separate acceptance process. [Container build inputs](docs/release.md#retain-the-container-build-inputs) · [Release acceptance](docs/release.md#acceptance-matrix)
