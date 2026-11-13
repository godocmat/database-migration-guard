# Changelog

## Unreleased

- Review plain SQL, SQLx, Flyway, Goose, and Supabase migrations for PostgreSQL 15–18 using the documented runner profiles.
- Collect scoped catalog and native history over verified TLS. Import pinned evidence into offline checks.
- Compare expected, current, and proposed schemas. Review explicit rename mappings and check deployed application contracts.
- Plan revision-bound rehearsals and execute them on supported disposable Linux x86-64 Docker workers.
- Evaluate trusted baselines, audited approvals, and required rehearsal evidence. Render saved reports as HTML, Markdown, SARIF, or CI annotations and Code Quality as applicable.
- Prepare protected GitHub and GitLab checks and local release candidate packages with checksums and a per-target CycloneDX SBOM.

Ruleset 2 reports PG002 compatibility risk for table and column removal alongside the PG001 data-loss finding. Protected evidence bundles must pin the new ruleset, and each blocking finding needs its own matching approval when the policy requires approval. [Rules](docs/rules.md) · [Policy evidence](docs/policy-evidence.md)

Rehearsal reports can include `cleanup_worker` when Docker removal could not be confirmed. Older strict report decoders need an update to read a report that includes this field. [Worker recovery](docs/research/simulation-worker.md#postgresql-connection-and-teardown)

The Linux x86-64 CLI and GitLab images use the pinned Wolfi runtime with Git HTTPS. GitLab also includes Bash and Python 3.15. Offline rebuilds require the retained signed package snapshot. [Container build inputs](docs/release.md#retain-the-container-build-inputs)

See the [release acceptance matrix](docs/release.md#acceptance-matrix) for the tested profiles and remaining release checks.
