# PostgreSQL schema comparison

Use the version 1 comparison in `dmg_core::diff` with a saved structural snapshot or a scoped inspection artifact. `compare_schemas` compares expected to current for drift and current to target for proposed changes. Each comparison includes the source structural digests, any inspection observations, the requested schema states, coverage, and deterministic typed changes. [Inspection evidence and structural digest](inspection-evidence.md)

## Coverage and changes

- A visible schema with an empty, complete table inventory establishes absence of supported tables. An inspection can also confirm that a requested schema is missing. Denied, unavailable, and unrequested schemas leave absence unconfirmed. A bare snapshot lists visible schemas, so a name outside that list has unknown state.
- A truncated relation inventory can leave some tables unobserved while preserving the column, index, and constraint projections for visible tables. Each child projection has its own coverage. A missing member requires sufficient coverage on its owner.
- Available raw fields are compared even when another field is unavailable. Unreadable definitions and masked foreign-key targets produce gaps. A constraint projection can preserve validation and enforcement flags while leaving column identity unknown. A matching structural digest establishes equal payloads with their recorded field states. Check coverage to determine whether there were comparable observations.
- `same_requested_scope` describes the set of requested schemas. `coverage` also accounts for unavailable snapshots, unobserved schema or object identities, and structural gaps. History and size estimates do not affect structural comparison.
- A table addition or removal represents the table and its owned columns, indexes, and constraints. The report compares members individually for tables with a matching or reviewed identity. Partition and dependency edges are reported separately.

Changes identify exact schema and object names. An index has a schema-qualified name and an owning table. Modified objects carry changed field identifiers, with actual definition text retained in the source snapshots. Definitions compare as recorded catalog text. Cross-major or deparser differences can produce lexical changes. Check the source fields when deciding whether a definition change alters behavior. A matching type reference establishes the recorded name and modifiers. Independent type definitions, partition bounds, and broader dependencies can remain outside the supported projection.

## Reviewed identity

Supply a mapping from an observed old object to its observed new name for the specific comparison. The mapping requires distinct names, the same schema and supported kind, and verified absence of each mapped name on the opposite side. Members retain their owning table, which can itself have a reviewed rename. Changes in a renamed object's supported attributes remain visible. The diff uses exact identity for unmapped objects, so two matching shapes under different names produce independent additions and removals when their scope proves both. Preserve the reviewed mapping with the protected input.
