# Supabase SQL migrations

Configure a single migration root for the Supabase CLI 3.0.0 adapter. For a standard project, use the following Guard settings.

~~~toml
roots = ["supabase/migrations"]

[adapter]
kind = "supabase"
~~~

Guard inventories immediate regular files named with an ASCII decimal prefix, an underscore, and a lowercase extension such as `20261113140000_create_accounts.sql`. The version is exact text, so `01`, `1`, `10`, and `2` are distinct identities in that order. A file ending in `.down.sql` is still forward SQL. Duplicate versions and pre-cutoff legacy init filenames make coverage incomplete. Native legacy selection can depend on the order of directory entries that otherwise do not match. [Supabase CLI 3.0.0 file selection](https://github.com/supabase/cli/blob/v3.0.0/apps/cli/src/command-internal/migration-list.ts) · [Native observations](../fixtures/dmg-09/supabase/README.md)

Git and pinned Guard manifests compare the original bytes, including the UTF-8 byte order mark and CRLF line endings. A manifest version for `01` uses `{"kind":"supabase","value":"01"}`. The native CLI records the version and split statement text and skips an already recorded version after an edit. Use trusted Guard integrity evidence to check edits. [Supabase CLI 3.0.0 history](https://github.com/supabase/cli/blob/v3.0.0/apps/cli/src/command-internal/migration-history.ts)

## Transaction coverage

The default is an implicit extended-query batch. The exact first line `-- pg-delta: transaction=false` switches to sequential execution, including when preceded by a UTF-8 byte order mark or terminated by CRLF. Guard preserves original source positions. A recognized top-level transaction boundary also selects sequential execution, and the explicit SQL commands control transaction scope. `SAVEPOINT` and `ROLLBACK TO SAVEPOINT` alone leave the default batch in effect. [Supabase CLI 3.0.0 file parser](https://github.com/supabase/cli/blob/v3.0.0/apps/cli/src/command-internal/migration-file.ts) · [Supabase CLI 3.0.0 executor](https://github.com/supabase/cli/blob/v3.0.0/apps/cli/src/command-internal/migration-apply.ts)

The default batch splits around concurrent index statements recognized by the runner's text matcher. Leading SQL comments and whitespace between keywords preserve the match. An internal comment between `CREATE` and `INDEX` leaves the statement in the batch. PostgreSQL rejected that spelling after a preceding executed statement and allowed it when it was first. Guard reports the proved transaction conflict after an earlier statement and marks the first-statement case incomplete. Adjacent comments on authored transaction boundaries and files that leave an explicit transaction open can also make transaction coverage incomplete. The inventory records the declared delivery. The parser carries the effective per-statement groups for risk analysis. [PostgreSQL extended query protocol](https://www.postgresql.org/docs/18/protocol-flow.html#PROTOCOL-FLOW-PIPELINING) · [Native observations](../fixtures/dmg-09/supabase/README.md)

Managed schemas, extension availability, seeds, hosted deployment, and other CLI versions are outside the verified adapter scope. SQL statements outside the normalized analysis subset continue to produce coverage diagnostics.
