
SELECT c.oid, c.relnamespace, c.relname::pg_catalog.text, c.relkind, c.relispartition,
       c.reltuples, c.relpages
FROM pg_catalog.pg_namespace n
JOIN pg_catalog.pg_class c ON c.relnamespace = n.oid
WHERE n.oid = ANY($1::pg_catalog.oid[])
  AND c.relkind IN ('r', 'p')
ORDER BY n.nspname COLLATE pg_catalog."C", c.relname COLLATE pg_catalog."C"
LIMIT $2
