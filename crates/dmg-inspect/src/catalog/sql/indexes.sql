
SELECT x.indrelid, x.indexrelid, i.relname::pg_catalog.text,
       am.amname::pg_catalog.text, x.indisunique, x.indisprimary, x.indisvalid,
       x.indisready, x.indnullsnotdistinct, x.indpred IS NOT NULL AS partial,
       x.indnkeyatts,
       CASE WHEN x.indnatts <= 32 THEN x.indkey::pg_catalog.int2[] END AS attributes
FROM pg_catalog.unnest($1::pg_catalog.oid[]) WITH ORDINALITY AS r(oid, position)
JOIN pg_catalog.pg_index x ON x.indrelid = r.oid
LEFT JOIN pg_catalog.pg_class i ON i.oid = x.indexrelid
LEFT JOIN pg_catalog.pg_am am ON am.oid = i.relam
ORDER BY r.position, i.relname COLLATE pg_catalog."C"
LIMIT $2
