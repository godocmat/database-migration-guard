
SELECT CASE WHEN c.relnamespace = ANY($2::pg_catalog.oid[]) THEN c.oid END AS child_oid,
       CASE WHEN p.relnamespace = ANY($2::pg_catalog.oid[]) THEN p.oid END AS parent_oid,
       c.relispartition, i.inhdetachpending
FROM pg_catalog.pg_inherits i
JOIN pg_catalog.pg_class c ON c.oid = i.inhrelid
JOIN pg_catalog.pg_class p ON p.oid = i.inhparent
WHERE (i.inhrelid = ANY($1::pg_catalog.oid[]) OR i.inhparent = ANY($1::pg_catalog.oid[]))
  AND c.relkind IN ('r', 'p', 'f')
ORDER BY i.inhrelid, i.inhparent
LIMIT $3
