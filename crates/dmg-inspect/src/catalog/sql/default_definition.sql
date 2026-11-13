
WITH value AS MATERIALIZED (
    SELECT pg_catalog.pg_get_expr(d.adbin, d.adrelid, false) AS definition
    FROM pg_catalog.pg_attrdef d
    WHERE d.oid = $1::pg_catalog.oid
)
SELECT CASE WHEN pg_catalog.octet_length(definition) <= $2::pg_catalog.int4 THEN definition END,
       pg_catalog.octet_length(definition) > $2
FROM value
