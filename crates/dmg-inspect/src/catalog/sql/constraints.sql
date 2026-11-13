
SELECT k.conrelid, k.oid, k.conname::pg_catalog.text, k.contype, k.convalidated,
       @ENFORCED@ AS enforced, k.condeferrable, k.condeferred,
       CASE WHEN pg_catalog.cardinality(k.conkey) <= 128 THEN k.conkey END AS columns,
       CASE WHEN f.relnamespace = ANY($3::pg_catalog.oid[]) AND pg_catalog.cardinality(k.confkey) <= 128
            THEN k.confkey END AS target_columns,
       CASE WHEN f.relnamespace = ANY($3::pg_catalog.oid[]) THEN f.oid END AS target_oid,
       f.relnamespace = ANY($3::pg_catalog.oid[]) AS target_visible,
       @PERIOD@ AS period,
       COALESCE(pg_catalog.cardinality(k.conkey) > 128, false) AS columns_truncated,
       COALESCE(f.relnamespace = ANY($3::pg_catalog.oid[]) AND pg_catalog.cardinality(k.confkey) > 128, false)
           AS target_columns_truncated
FROM pg_catalog.unnest($1::pg_catalog.oid[]) WITH ORDINALITY AS r(oid, position)
JOIN pg_catalog.pg_constraint k ON k.conrelid = r.oid
LEFT JOIN pg_catalog.pg_class f ON f.oid = k.confrelid
ORDER BY r.position, k.conname COLLATE pg_catalog."C", k.oid
LIMIT $2
