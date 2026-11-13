
SELECT scoped.oid
FROM pg_catalog.unnest($1::pg_catalog.oid[]) AS scoped(oid)
WHERE EXISTS (
    SELECT FROM pg_catalog.pg_class c
    WHERE c.relnamespace = scoped.oid
      AND (c.relkind NOT IN ('r', 'p', 'i', 'I', 't') OR c.relrowsecurity OR c.relhasrules)
      AND (c.relkind <> 'S' OR NOT EXISTS (
          SELECT FROM pg_catalog.pg_depend d
          JOIN pg_catalog.pg_class owner ON owner.oid = d.refobjid
          WHERE d.classid = 'pg_catalog.pg_class'::pg_catalog.regclass
            AND d.refclassid = 'pg_catalog.pg_class'::pg_catalog.regclass
            AND d.objid = c.oid AND d.objsubid = 0
            AND d.refobjsubid > 0 AND d.deptype IN ('a', 'i')
            AND owner.relkind IN ('r', 'p') AND owner.relnamespace = scoped.oid
      ))
) OR EXISTS (
    SELECT FROM pg_catalog.pg_type t
    WHERE t.typnamespace = scoped.oid
      AND (t.typtype IN ('d', 'e', 'r', 'm')
           OR (t.typtype = 'b' AND t.typelem = 0))
) OR EXISTS (
    SELECT FROM pg_catalog.pg_proc p
    WHERE p.pronamespace = scoped.oid
) OR EXISTS (
    SELECT FROM pg_catalog.pg_class c
    JOIN pg_catalog.pg_trigger t ON t.tgrelid = c.oid
    WHERE c.relnamespace = scoped.oid AND NOT t.tgisinternal
)
ORDER BY scoped.oid
