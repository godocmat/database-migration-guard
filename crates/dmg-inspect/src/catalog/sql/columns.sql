
SELECT a.attrelid, a.attname::pg_catalog.text, a.attnum,
       n.nspname::pg_catalog.text, t.typname::pg_catalog.text, a.atttypmod, a.attndims::pg_catalog.int4,
       a.attnotnull, a.attidentity, a.attgenerated, a.atthasdef, d.oid AS definition_oid
FROM pg_catalog.unnest($1::pg_catalog.oid[]) WITH ORDINALITY AS r(oid, position)
JOIN pg_catalog.pg_attribute a ON a.attrelid = r.oid
LEFT JOIN pg_catalog.pg_type t ON t.oid = a.atttypid
LEFT JOIN pg_catalog.pg_namespace n ON n.oid = t.typnamespace
LEFT JOIN pg_catalog.pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
WHERE a.attnum > 0 AND NOT a.attisdropped
ORDER BY r.position, a.attnum
LIMIT $2
