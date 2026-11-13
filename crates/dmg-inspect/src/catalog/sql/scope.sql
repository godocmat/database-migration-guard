
SELECT requested.schema_name,
       n.oid,
       CASE WHEN n.oid IS NOT NULL THEN pg_catalog.has_schema_privilege(n.oid, 'USAGE') END
FROM pg_catalog.unnest($1::pg_catalog.text[]) AS requested(schema_name)
LEFT JOIN pg_catalog.pg_namespace n
  ON n.nspname::pg_catalog.text COLLATE pg_catalog."C" = requested.schema_name
ORDER BY requested.schema_name COLLATE pg_catalog."C"
