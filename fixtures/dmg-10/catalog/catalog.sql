PREPARE inspect_scope(text[]) AS
SELECT json_build_object(
    'schema', requested.name,
    'state', CASE
        WHEN n.oid IS NULL THEN 'missing'
        WHEN pg_catalog.has_schema_privilege(n.oid, 'USAGE') THEN 'visible'
        ELSE 'denied'
    END)
FROM pg_catalog.unnest($1) AS requested(name)
LEFT JOIN pg_catalog.pg_namespace n ON n.nspname::text = requested.name
ORDER BY requested.name COLLATE "C";

PREPARE inspect_relations(text[], integer) AS
WITH scope AS MATERIALIZED (
    SELECT n.oid, n.nspname
    FROM pg_catalog.pg_namespace n
    WHERE n.nspname::text = ANY($1)
      AND pg_catalog.has_schema_privilege(n.oid, 'USAGE')
), relations AS MATERIALIZED (
    SELECT c.oid, n.nspname, c.relname, c.relkind, c.relispartition, c.reltuples, c.relpages
    FROM scope n JOIN pg_catalog.pg_class c ON c.relnamespace = n.oid
    WHERE c.relkind IN ('r', 'p')
    ORDER BY n.nspname COLLATE "C", c.relname COLLATE "C"
    LIMIT $2
)
SELECT json_build_object(
    'schema', r.nspname, 'relation', r.relname, 'kind', r.relkind,
    'is_partition', r.relispartition,
    'estimated_rows', CASE WHEN r.relkind = 'r' AND r.reltuples >= 0 THEN r.reltuples END,
    'estimated_heap_bytes', CASE WHEN r.relkind = 'r' AND r.relpages >= 0
        THEN r.relpages::bigint * pg_catalog.current_setting('block_size')::bigint END)
FROM relations r
ORDER BY r.nspname COLLATE "C", r.relname COLLATE "C";

PREPARE inspect_columns(text[], integer, boolean) AS
SELECT json_build_object(
    'relation', r.relname, 'column', a.attname, 'position', a.attnum,
    'type', pg_catalog.format_type(a.atttypid, a.atttypmod),
    'not_null', a.attnotnull, 'has_default', d.adnum IS NOT NULL,
    'default_definition', CASE WHEN $3 THEN pg_catalog.pg_get_expr(d.adbin, d.adrelid, false) END)
FROM pg_catalog.pg_namespace n
JOIN pg_catalog.pg_class r ON r.relnamespace = n.oid
JOIN pg_catalog.pg_attribute a ON a.attrelid = r.oid
LEFT JOIN pg_catalog.pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
WHERE n.nspname::text = ANY($1) AND pg_catalog.has_schema_privilege(n.oid, 'USAGE')
  AND r.relkind IN ('r', 'p') AND a.attnum > 0 AND NOT a.attisdropped
ORDER BY n.nspname COLLATE "C", r.relname COLLATE "C", a.attnum
LIMIT $2;

PREPARE inspect_indexes(text[], integer, boolean) AS
SELECT json_build_object(
    'relation', r.relname, 'index', i.relname, 'valid', x.indisvalid, 'ready', x.indisready,
    'definition', CASE WHEN $3 THEN pg_catalog.pg_get_indexdef(i.oid, 0, false) END)
FROM pg_catalog.pg_namespace n
JOIN pg_catalog.pg_class r ON r.relnamespace = n.oid
JOIN pg_catalog.pg_index x ON x.indrelid = r.oid
JOIN pg_catalog.pg_class i ON i.oid = x.indexrelid
WHERE n.nspname::text = ANY($1) AND pg_catalog.has_schema_privilege(n.oid, 'USAGE')
  AND r.relkind IN ('r', 'p')
ORDER BY n.nspname COLLATE "C", r.relname COLLATE "C", i.relname COLLATE "C"
LIMIT $2;

PREPARE inspect_constraints(text[], integer, boolean) AS
SELECT json_build_object(
    'relation', r.relname, 'constraint', k.conname, 'kind', k.contype,
    'validated', k.convalidated,
    'definition', CASE WHEN $3 THEN pg_catalog.pg_get_constraintdef(k.oid, false) END)
FROM pg_catalog.pg_namespace n
JOIN pg_catalog.pg_class r ON r.relnamespace = n.oid
JOIN pg_catalog.pg_constraint k ON k.conrelid = r.oid
WHERE n.nspname::text = ANY($1) AND pg_catalog.has_schema_privilege(n.oid, 'USAGE')
  AND r.relkind IN ('r', 'p')
ORDER BY n.nspname COLLATE "C", r.relname COLLATE "C", k.conname COLLATE "C"
LIMIT $2;

PREPARE inspect_partitions(text[], integer) AS
SELECT json_build_object(
    'child', child.relname, 'parent', parent.relname, 'detach_pending', i.inhdetachpending)
FROM pg_catalog.pg_inherits i
JOIN pg_catalog.pg_class child ON child.oid = i.inhrelid
JOIN pg_catalog.pg_class parent ON parent.oid = i.inhparent
JOIN pg_catalog.pg_namespace cn ON cn.oid = child.relnamespace
JOIN pg_catalog.pg_namespace pn ON pn.oid = parent.relnamespace
WHERE cn.nspname::text = ANY($1) AND pn.nspname::text = ANY($1)
  AND pg_catalog.has_schema_privilege(cn.oid, 'USAGE')
  AND pg_catalog.has_schema_privilege(pn.oid, 'USAGE')
  AND child.relkind IN ('r', 'p') AND parent.relkind IN ('r', 'p')
ORDER BY cn.nspname COLLATE "C", child.relname COLLATE "C",
         pn.nspname COLLATE "C", parent.relname COLLATE "C"
LIMIT $2;
