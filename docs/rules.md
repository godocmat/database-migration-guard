# PostgreSQL risk rules

Guard evaluates rules over the ordered forward migration inventory. For a Git comparison, earlier unchanged migrations establish available schema context and the selected candidate paths supply findings. If selection is incomplete, Guard can still report findings for available forward sources. Each finding includes an original source span, affected object, severity, confidence, mitigation, and evidence.

| Rule | Supported default behavior |
| --- | --- |
| PG001 | Critical for table drops, column drops, and truncation where existing data may be lost. Proven private, newly created empty tables can lower the impact. |
| PG002 | High for an established rename that changes an existing object identity. Other supported compatibility signals can be Medium when application dependencies are unavailable. |
| PG003 | High for a likely rewrite on an existing relation. A nullable add or a supported nonvolatile default avoids the rewrite signal. Type changes use known type facts and the presence of a `USING` expression. |
| PG004 | Medium for a likely constraint scan at unknown table size. A retained, validated constraint can prove that a separate `SET NOT NULL` scan is unnecessary. The proof tracks transaction rollback and savepoints. |
| PG005 | High for a regular index build on an existing or unresolved relation. A table created in the same verified transaction can avoid the existing-relation finding. A supported index drop can produce a Medium advisory. |
| PG006 | High for an `UPDATE` or `DELETE` with an absent or provably true predicate. Additional source relations reduce confidence about actual row impact. |
| PG007 | High for an incompatible transaction block, a proven reference before a later table creation, or a supported writer-blocking lock held into a broad write on that relation. The runner wrapper, statement delivery, explicit controls, and savepoints determine the available transaction evidence. |

Reports can also carry predicted relation locks for passing or risky DDL. For example, a constant-default column add can avoid a rewrite while requesting `ACCESS EXCLUSIVE`. The prediction does not measure lock duration, production workload, or migration runtime. [PostgreSQL table locks](https://www.postgresql.org/docs/18/explicit-locking.html) · [PostgreSQL ALTER TABLE](https://www.postgresql.org/docs/18/sql-altertable.html) · [PostgreSQL CREATE INDEX](https://www.postgresql.org/docs/18/sql-createindex.html)

The rules use [supported parser operations](postgresql-support.md) and statically established schema and runner facts. An optional pinned [catalog observation](catalog-context.md) adds scoped relation facts and labeled size estimates to relevant findings. Use [application contracts](application-contracts.md) with `dmg diff` to evaluate declared dependencies. The scanner preserves incomplete coverage and existing findings when a migration contains an unsupported statement or unresolved condition.

Run the focused rules and the full public development corpus from the workspace root. The corpus command requires Git and Python 3.

```sh
cargo test --locked -p dmg-rules
cargo test --locked -p dmg-rules --test corpus -- --ignored --nocapture
```

The corpus exercises 130 adapter and PostgreSQL-major combinations, original spans, selected findings, known absences, and predicted relation locks. It checks the default gate inputs from discovery, parsing, and rules. The CLI checks the policy and maps the actual command outcome to an exit code. [Fixture evidence](../fixtures/dmg-02/evidence/README.md)
