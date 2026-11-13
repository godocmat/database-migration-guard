# Supabase CLI 3.0.0 migration parity

The [native probe](native_probe.py) exercises `supabase migration new`, `migration up`, and `migration list` against a disposable PostgreSQL cluster. Each case gets a new database and project directory. The probe uses only its generated credentials, temporary home, and loopback address. The CLI's `--remote=<url>` flag selects that explicit PostgreSQL address. This uses the native migration path for a custom database without a linked Supabase project or a hosted account. [Supabase CLI 3.0.0 release](https://github.com/supabase/cli/releases/tag/v3.0.0) · [CLI v3 targeting](https://supabase.com/changelog/supabase-cli-v3)

## Discovery and history

- The CLI generates UTC 14-digit timestamp filenames under `supabase/migrations`. The loader also accepts variable-width decimal prefixes and compares them as exact text. It ran `01`, `1`, `10`, and `2` in that order and stored all four distinct versions. It applied the file ending `.down.sql` as a forward migration. Uppercase `.SQL`, an unversioned file, and a file in a child directory were skipped. [Filename generation](https://github.com/supabase/cli/blob/v3.0.0/apps/cli/src/command-internal/migration-file.ts) · [File selection](https://github.com/supabase/cli/blob/v3.0.0/apps/cli/src/command-internal/migration-list.ts) · [Version comparison](https://github.com/supabase/cli/blob/v3.0.0/apps/cli/src/command-internal/migration-timestamp.format.ts)
- The first directory entry in UTF-8 filename order receives a special legacy check. A first `20211208000000_init.sql` was skipped and `20211209000000_init.sql` was applied. With an earlier ignored `!readme`, both migrations applied. A scanner that cannot observe all entries needs to report this legacy scope as incomplete. [Legacy file selection](https://github.com/supabase/cli/blob/v3.0.0/apps/cli/src/command-internal/migration-list.ts)
- `supabase_migrations.schema_migrations` held the exact version, migration name, and split statement text. Editing the first applied file changed its SHA-256. A second apply selected zero files and preserved the recorded statements. Keep original-byte integrity tied to trusted Git or a Guard manifest. With two files sharing `01`, the first committed. The second failed with `23505` on its history insert and rolled back its ALTER. [Native history](https://github.com/supabase/cli/blob/v3.0.0/apps/cli/src/command-internal/migration-history.ts) · [Pending migrations](https://github.com/supabase/cli/blob/v3.0.0/apps/cli/src/command-internal/migration-pending.ts)

## Execution behavior

The CLI normally sends ordinary statements in an extended-query batch and adds the history insert to the final batch. Recognized standalone operations split that batch. The exact first line `-- pg-delta: transaction=false` or an authored top-level transaction boundary switches the whole file to sequential delivery. The explicit transaction commands then control which statements share a transaction. [Migration application](https://github.com/supabase/cli/blob/v3.0.0/apps/cli/src/command-internal/migration-apply.ts) · [Directive parsing](https://github.com/supabase/cli/blob/v3.0.0/apps/cli/src/command-internal/migration-file.ts) · [PostgreSQL pipelining](https://www.postgresql.org/docs/18/protocol-flow.html#PROTOCOL-FLOW-PIPELINING)

Both servers produced the same outcomes below. Failed cases left only the baseline version in history.

| Candidate | CLI exit and server SQLSTATE | Durable state |
| --- | --- | --- |
| Default ALTER followed by division by zero | 1, `22012` | Earlier ALTER rolled back. |
| Exact directive with LF or with UTF-8 BOM and CRLF | 1, `22012` | Earlier ALTER committed. |
| Directive preceded by a blank line or followed by a trailing space | 1, `22012` | Earlier ALTER rolled back. |
| ALTER, concurrent unique index with a leading block comment and a newline after `CREATE`, then another ALTER and division by zero | 1, `22012` | First ALTER and valid index committed. Second ALTER rolled back. |
| ALTER, `VACUUM (ANALYZE)`, second ALTER and division by zero | 1, `22012` | First ALTER committed. Second ALTER rolled back. |
| ALTER followed by concurrent index with a comment between `CREATE` and `INDEX` | 1, `25001` | Earlier ALTER rolled back. The internal comment prevented recognition as a standalone operation. |
| The same unrecognized index spelling after `SELECT 1` | 1, `25001` | The index was absent. |
| The same unrecognized index spelling first, after a comment-only token | 0 | A valid index and the candidate history row committed. |
| ALTER before `BEGIN`, then another ALTER and a concurrent unique index inside the block | 1, `25001` | First ALTER committed. Second ALTER rolled back. |
| ALTER and `SAVEPOINT` in the default batch, with a later `ROLLBACK TO SAVEPOINT` | 1, `25P01` | Earlier ALTER rolled back. The implicit batch did not establish an explicit transaction block for the savepoint. |
| ALTER, `COMMIT` with an adjacent or spaced block comment, then division by zero | 1, `22012` | Earlier ALTER committed in both cases. The native matcher classifies the two spellings differently. |
| `BEGIN` followed by an ALTER and EOF, with and without the exact directive | 0 | The ALTER and the candidate history row both rolled back when the session closed. |

For automatic splitting, classification needs the source text. The CLI executes different spellings of the same PostgreSQL operation in different transaction contexts. PostgreSQL can identify an extended-query pipeline once the first command has executed without a Sync. Guard preserves original file bytes and distinguishes extended-query batches from explicit file transactions. These probes exercise SQL migrations only. Managed Supabase schemas, extensions, hosted deployments, and older CLI versions require separate evidence. [PostgreSQL extended-query pipelining](https://www.postgresql.org/docs/18/protocol-flow.html#PROTOCOL-FLOW-PIPELINING)

## Reproduction

Run as a regular user on Linux with Python 3.11 or newer and the matching PostgreSQL binaries. The probe asserts ordered apply paths, raw text versions, SQLSTATE, surviving schema changes, valid index state, and history. Run it once per server. It starts and stops its own cluster and emits JSON Lines after teardown.

~~~sh
python3 -B fixtures/dmg-09/supabase/native_probe.py \
  --supabase /path/to/supabase --pg-bin /path/to/postgresql-17.12/bin
python3 -B fixtures/dmg-09/supabase/native_probe.py \
  --supabase /path/to/supabase --pg-bin /path/to/postgresql-18.7/bin
~~~

The [PostgreSQL 17.12 observations](native-17.12.jsonl) and [PostgreSQL 18.7 observations](native-18.7.jsonl) were recorded on Ubuntu 24.04 x86-64 in a WSL2 network namespace with loopback only. The pinned release archive `supabase_3.0.0_linux_amd64.tar.gz` has SHA-256 `eeb61edcd59a4735fcb5e58596574f25d03d420efca779f43ddb307e1cc5c754`. Each output header records its executable digests. [Release assets](https://github.com/supabase/cli/releases/tag/v3.0.0)
