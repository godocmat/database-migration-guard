
WITH value AS MATERIALIZED (
    SELECT pg_catalog.pg_get_indexdef($1::pg_catalog.oid, 0, false) AS definition
)
SELECT CASE WHEN pg_catalog.octet_length(definition) <= $2::pg_catalog.int4 THEN definition END,
       pg_catalog.octet_length(definition) > $2
FROM value
