# Security policy

Report suspected vulnerabilities privately through GitHub. Include the Guard version or commit, affected command or integration, operating system, and PostgreSQL major if relevant. Share a minimal description of the impact and omit credentials, production data, and confidential migration SQL. [Report a vulnerability](https://github.com/godocmat/database-migration-guard/security/advisories/new)

## Use Guard in a protected workflow

- Keep the scanner, CI workflow, migration roots, policy, and evidence pins under maintainer control. In a protected check, supply exact Git revisions and treat incomplete coverage as a failed check. [GitHub Actions](docs/github-pilot.md) · [GitLab CI](docs/gitlab-ci.md) · [Policy evidence](docs/policy-evidence.md)
- Keep inspection passwords, CA material, and saved catalog evidence in protected storage. Use a dedicated PostgreSQL role with only the grants needed for the selected inspection. [Inspection setup](docs/inspect.md)
- Rehearse only on the documented disposable worker with synthetic or sanitized inputs. [Rehearsal](docs/simulation.md)
- Verify the approved source revision and artifact digests before installing or pinning a release. [Release verification](docs/release.md#attest-and-verify-approved-artifacts)

The repository security review records findings, fixes, and verification scope. [Security review](docs/security_best_practices_report.md)
