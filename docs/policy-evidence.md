# Policy evidence JSON

`dmg check` accepts version 1 JSON through `--policy-evidence FILE` with a lowercase SHA-256 of the exact bytes from a protected source. Author the bundle with trusted workflow code or a protected reviewer. The policy, project configuration, evidence bundle, expected binding, and report pin need independent authorization. The bundle is limited to 8 MiB and the rehearsal report to 64 MiB.

## Bundle

This example represents the minimum empty bundle. The commit IDs and hashes are examples.

```json
{
  "schema_version": 1,
  "subject": {
    "repository_id": "example-repo",
    "target_id": "production",
    "base_commit": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    "head_commit": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    "policy_digest": "1111111111111111111111111111111111111111111111111111111111111111",
    "project_config_digest": "2222222222222222222222222222222222222222222222222222222222222222",
    "ruleset_version": "1"
  }
}
```

The subject applies to every record in the bundle. Digests bind the raw TOML bytes, including whitespace. Use the ruleset version from the matching analysis report. Both revision IDs must be full Git commit IDs. Repository, target, and approver IDs are opaque tokens of up to 128 characters. Use letters, digits, dots, dashes, or underscores and begin with a letter or digit.

Optional fields at the bundle root are `baseline`, `approvals`, and `rehearsal`. Unknown fields, duplicate selectors within a record list, and more than 10,000 baseline selectors or approvals make the bundle invalid.

### Finding selector

Each selector identifies one exact finding. The check report has the source values in `findings` and `migrations.inventory`. Use the migration file digest from the matching forward file. Copy the finding's `migration`, `rule_id`, `object`, and `fingerprint` and use `span.path` and `span.bytes`. An object value may be `null` when the finding has no affected object.

```json
{
  "rule_id": "PG006",
  "migration": { "adapter": "plain", "version": 2 },
  "path": "migrations/2_update.sql",
  "migration_digest": "3333333333333333333333333333333333333333333333333333333333333333",
  "statement_bytes": { "start": 0, "end": 29 },
  "finding_fingerprint": "4444444444444444444444444444444444444444444444444444444444444444",
  "object": { "kind": "relation", "schema": "public", "name": "items", "member": null }
}
```

The byte interval is zero-based and half-open. A source edit, relocation, changed object, or ambiguous match prevents the selector from applying. Native migration IDs use the same representation as the analysis report.

### Baseline and approvals

The `baseline` object has `reference_commit` and `findings`. Use a separately reviewed reference commit and exact selectors derived from the findings approved for that reference. The reference may be a reviewed branch distinct from the current Git base. Make the reference commit and its blobs available to the checker. The checker independently reproduces the selected reference findings from Git using the same project configuration and ruleset, then compares each verified entry with the exact current finding and original file bytes. An unavailable reference, incomplete reference analysis, or selector that cannot be reproduced makes the check incomplete. A baseline entry can waive only a noncritical finding's severity threshold.

Each element of the `approvals` array has `finding` (the selector above), `approver`, `justification`, `issued_at_unix_seconds`, and `expires_at_unix_seconds`. Justification is a nonempty single-line string of at most 1,024 bytes. The approval becomes effective at issuance and expires at the specified second. If rehearsal is required for its rule, include `rehearsal_report_sha256` with the exact report pin. The referenced rehearsal must have succeeded and completed by the time the approval was issued.

### Rehearsal

The `rehearsal` object has `expected_binding` and `report_sha256`. The expected binding has the same schema as the simulation report's `binding`. Construct the expected binding from the protected run inputs. Include the repository and target IDs, Git base and candidate, original config and policy SHA-256 values, adapter and runner profile, exact inventory and execution plan digests, PostgreSQL major and observed server version, both approved image digests, provisioned schema scope, search path order, and rehearsal mode with any baseline and seed digests. `report_sha256` is the SHA-256 of the exact output bytes received from the trusted disposable worker.

## Audit

The check report's optional `policy_audit` records the authenticated bundle digest, repository and target IDs, reference commit, applied baseline matches and approvals, and the verified rehearsal binding and report digests and outcome. Applied approvals record the approver ID, timestamps, and a SHA-256 of the justification. Keep the original justification in the protected record and retain it with the audit. Use the decision status and exit code for the check outcome.
