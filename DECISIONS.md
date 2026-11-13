# Decisions

## DMG-03 foundation

The workspace pins Rust 1.100.0 and uses edition 2024 with Cargo resolver 3. Keep the parser spike as a separate workspace. The library's initial dependency graph covers serialization, typed errors, and SHA-256 hashing. [Cargo workspace reference](https://doc.rust-lang.org/cargo/reference/workspaces.html)

- A source span uses a repository-relative UTF-8 path with slash separators and zero-based, half-open offsets into the original file. Display lines and Unicode scalar columns start at one. LF ends a line. The two CRLF bytes map to the same end-of-line position. An empty range can mark EOF. The mapping rejects offsets outside the input or inside a UTF-8 scalar.
- A migration identity is the adapter and its numeric version. Up and Down files share the identity and carry a separate direction. Head inventory follows adapter order. Forward selection follows execution order. A missing selection records unresolved selection, and an empty list records a resolved selection with zero forward inputs.
- Integrity rows describe the original-byte SHA-256 and repository path on either side of an identity and direction. A byte-identical path change at the same identity retains `unchanged` status with both paths available. Git integrity is associated with an explicit base and head. Applied state stays unknown until authorized history is available.
- Execution metadata records the runner's transaction wrapper and statement delivery. SQLx's wrapper opt-out retains single-query delivery. The parser and analyzer use this information to determine the effective transaction context.
- Coverage gaps carry a typed reason and repository, migration, path, or source-span scope. Report construction combines discovery and analysis gaps while preserving existing findings.

## Policy and report v1

Policy TOML requires `schema_version = 1`. The v1 shape rejects unknown settings. Rule IDs use the form `PG001` through `PG999`, and rule registry membership is checked by the evaluator when the ruleset is available. The initial policy shape accepts a severity threshold, optional Git evidence requirement, integrity outcomes, and per-rule severity.

```toml
schema_version = 1
deny_at = "high"

[integrity]
require_git_base = true
deny_modified = true
deny_missing = true

[rules.PG005]
severity = "medium"
```

Report JSON requires `schema_version = 1` and accepts additive fields. Its top-level fields identify the tool, ruleset, PostgreSQL target, migration plan, coverage, severity counts, findings, and optional policy decision. Creation sorts findings by original source span with deterministic tie-breakers and derives severity counts from the same findings. Adapter order remains part of the migration plan. Consumers should interpret an absent applied-history record as unknown and a null policy decision as a report without a decision. Future major versions need explicit migration guidance and compatibility handling.
