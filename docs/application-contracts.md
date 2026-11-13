# Application schema contracts

An application contract describes the structural PostgreSQL dependencies of one deployed application version. Keep its JSON in the trusted configuration source for the reviewed revision. Evaluate every application version that must continue working during the rollout. The same contract is evaluated against both the current and target schema. The evaluator accepts structural snapshots and inspection artifacts through the schema diff input type. [Schema comparison](diff.md) · [Inspection evidence](inspection-evidence.md)

~~~json
{
  "schema_version": 1,
  "application": "orders-api",
  "version": "2.4.1",
  "critical": true,
  "tables": [
    {
      "table": { "schema": "sales", "name": "Orders" },
      "kind": "ordinary",
      "columns": [
        {
          "name": "id",
          "access": "read",
          "data_type": {
            "name": { "schema": "pg_catalog", "name": "int8" },
            "type_modifier": -1,
            "array_dimensions": 0
          },
          "nullability": "not_null"
        },
        {
          "name": "note",
          "access": "read_write",
          "data_type": {
            "name": { "schema": "pg_catalog", "name": "text" },
            "type_modifier": -1,
            "array_dimensions": 0
          },
          "nullability": "nullable"
        }
      ]
    }
  ]
}
~~~

Use exact PostgreSQL catalog names. The example requires the case-sensitive name `sales."Orders"`. Omitting a table's `kind` accepts an ordinary or partitioned table. Omitting `columns` checks only the table. `data_type` compares the catalog type name, raw type modifier, and declared array dimensions. Copy those fields from the trusted snapshot. An array type name can be `pg_catalog._int4`. PostgreSQL treats the declared array dimensions as metadata, so a match establishes equality of the declaration. The named type's implementation and its accepted values remain outside this check. [Column catalog fields](https://www.postgresql.org/docs/18/catalog-pg-attribute.html) · [PostgreSQL arrays](https://www.postgresql.org/docs/18/arrays.html)

`read` declares a reference to the named column. `write` and `read_write` additionally require a column that structurally permits assignment of an explicit value without an identity override. Generated columns and `GENERATED ALWAYS` identity columns violate that requirement. Runtime permissions, triggers, partition routing, write success, and omission of other columns are outside the contract. [Generated columns](https://www.postgresql.org/docs/18/ddl-generated-columns.html) · [Identity columns](https://www.postgresql.org/docs/18/ddl-identity-columns.html)

`nullability` defaults to `any`. `nullable` requires the direct column `NOT NULL` flag to be absent. A CHECK or domain constraint can still reject nulls. `not_null` requires the direct flag and evidence that the constraint is enforced and validated. An inspection on PostgreSQL 15–17 supplies this through its column catalog flag. On PostgreSQL 18 and for snapshots without a known server version, the evaluator also requires an observed validated and enforced `NOT NULL` constraint for that exact column. A deferred validation, unavailable constraint row, or unavailable column association leaves this requirement unknown. Nullability describes the named relation's catalog declaration. Joins and wider inheritance or partition behavior are outside this check. [PostgreSQL 17 column catalog](https://www.postgresql.org/docs/17/catalog-pg-attribute.html) · [PostgreSQL 18 column catalog](https://www.postgresql.org/docs/18/catalog-pg-attribute.html) · [Deferred validation](https://www.postgresql.org/docs/18/sql-altertable.html)

## Outcomes

A table or column proven absent violates the matching requirement. An observation with denied scope, unavailable scope, or a relevant projection gap can leave its presence unknown. An observed type mismatch remains a violation when a separate column is unknown. Each side records its own status and coverage. A critical contract's proven target violation is explicitly marked, including when other facts are unknown. A change from satisfied to violated is a regression. A check violated on both sides is a preexisting violation. A change from violated to satisfied is resolved. An unknown on either side leaves that transition indeterminate.

Contract checks use the exact names required by the deployed application. A reviewed rename in the schema diff still leaves the old name absent from the target if the old name was removed. A satisfied assessment covers the declared structural facts. It carries the canonical contract SHA-256, source structural digests when available, and inspection observations when provided. The caller supplies the trusted artifacts and establishes their target, provenance, and freshness.

Decode an application contract with `ApplicationContractV1::from_json` and evaluate it with `dmg_core::application::evaluate_contract`. The decoder accepts up to 4 MiB of JSON, 256 qualified tables across up to 32 non-system schemas, and 256 columns per table and 4096 total. Names use the inspection contract's catalog identifier limits. Application and version labels start with an ASCII letter or digit and allow letters, digits, dots, underscores, hyphens, and plus signs up to 128 bytes. Duplicate tables and columns, unknown fields, and unsupported schema versions are rejected. The canonical contract sorts by qualified table and column name.
