-- 0011 (E19-T10, PRD §13): deleting a cloud business. The owner's request marks it `deleted`
-- with a due date and revokes its tills; until then it can be cancelled. When due, a job
-- removes every row of that business, in every table that has `business_id`.
-- Append-only once merged to main.

ALTER TABLE businesses ADD COLUMN deletion_requested_at timestamptz;
ALTER TABLE businesses ADD COLUMN deletion_due_at timestamptz;

DO $$
BEGIN
  IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'pos_retention') THEN
    -- The purge job's role. Like pos_app it bypasses nothing: it sees and deletes only the
    -- business named in `app.purge_business_id`, through the policies below.
    CREATE ROLE pos_retention NOLOGIN NOSUPERUSER NOBYPASSRLS NOCREATEDB NOCREATEROLE;
  END IF;
END $$;

DO $$
DECLARE t text;
BEGIN
  FOR t IN
    SELECT c.table_name FROM information_schema.columns c
      JOIN information_schema.tables x USING (table_schema, table_name)
     WHERE c.table_schema = current_schema() AND c.column_name = 'business_id'
       AND x.table_type = 'BASE TABLE'
  LOOP
    EXECUTE format('GRANT SELECT, DELETE ON %I TO pos_retention', t);
    EXECUTE format(
      'CREATE POLICY retention_purge ON %I FOR ALL TO pos_retention
         USING (business_id = nullif(current_setting(''app.purge_business_id'', true), '''')::uuid)', t);
  END LOOP;
END $$;

-- Which businesses are due (the job's only cross-tenant read: ids and dates).
CREATE POLICY retention_due ON businesses FOR SELECT TO pos_retention
  USING (status = 'deleted' AND deletion_due_at IS NOT NULL);
