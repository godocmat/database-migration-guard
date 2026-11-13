# Catalog context for offline review

`dmg scan` and `dmg check` can use a saved `dmg inspect` catalog artifact to explain risks to existing relations. Collect the artifact from the authorized target in a protected context. Keep its exact-byte SHA-256, expected opaque target ID, and maximum age with the trusted job configuration. [Catalog collection](inspect.md#catalog-evidence)

```sh
dmg check --repo . --config /protected/dmg.toml --policy /protected/policy.toml \
  --base "$BASE_SHA" --head "$HEAD_SHA" --format json \
  --catalog-evidence /protected/catalog.json \
  --catalog-evidence-sha256 "$CATALOG_SHA256" \
  --catalog-target-id orders-staging --catalog-max-age-seconds 600
```

The four catalog flags are optional as a group. The importer reads up to 32 MiB from a regular file, hashes the original bytes, and validates version 1 catalog JSON. It checks that the observation matches the supplied target ID and project PostgreSQL major. The caller's clock checks freshness against the start time of the observation. Future observations and observations older than the specified age are rejected. A supplied artifact that fails these checks makes the analysis incomplete. The report includes a catalog-evidence diagnostic and retains any static findings.

Catalog context and native history have independent pins. Both can be supplied to one command when their target IDs agree. A configured policy target also has to agree. Catalog metadata carries no applied-migration classification. The artifact pin and target identifier depend on a trusted publisher with access to the authorized environment.

## What a finding can show

An exact, schema-qualified relation target can carry the observed table kind and the presence of a named member or incoming dependency. A physical relation can also carry planner row and heap estimates and the optional observed total relation size. The size applies to that relation, including its indexes and TOAST for the total. It gives a scale for the predicted lock, validation, rewrite, or data-change impact. Planner values can differ from actual row counts and storage. A zero row estimate still leaves emptiness unknown. [PostgreSQL planner statistics](https://www.postgresql.org/docs/18/catalog-pg-class.html) · [PostgreSQL relation size functions](https://www.postgresql.org/docs/18/functions-admin.html#FUNCTIONS-ADMIN-DBSIZE)

Matching applies to supported findings at their original statement spans and in selected-forward order. An earlier selected statement can make a relation name, member, dependency, or size unsuitable for attribution. Unqualified targets are left to the static analyzer. The same applies to unresolved migration selection and incomplete parsed plans. The observations do not cover every dependent object or include evidence about current lock contention. They leave the static severity, confidence, fingerprint, lock prediction, and gate unchanged.

The optional JSON `catalog_context` records the artifact digest, observation, requested schema states, and distinct typed field and reason pairs from the artifact. A valid partial snapshot can enrich a finding with positive facts it actually observed. Missing or unavailable facts stay unknown. A partial snapshot by itself leaves the static gate unchanged. A supplied observation without a known server major or usable snapshot makes the analysis incomplete and keeps its typed catalog gaps in the report. Confirmed missing schemas can account for an empty scope without a snapshot. Review the protected original when the exact location of a projection gap matters. Guard omits deparsed expressions, unrelated object names, connection details, and application row data from catalog context in the report. [Inspection evidence format](inspection-evidence.md)
