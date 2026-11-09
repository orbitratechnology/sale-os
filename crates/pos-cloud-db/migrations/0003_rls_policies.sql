-- 0003 (spec C0007, the pattern): row-level security on every tenant table (E16-T02).
-- A missing `app.business_id` setting is NULL and matches no row: the system fails closed.
-- Every later migration that adds a table with `business_id` adds the same policy; a test
-- fails if one is missing. Append-only once merged to main.
DO $$
DECLARE t text;
BEGIN
  FOREACH t IN ARRAY ARRAY['businesses', 'business_members', 'locations', 'registers'] LOOP
    EXECUTE format('ALTER TABLE %I ENABLE ROW LEVEL SECURITY', t);
    EXECUTE format('ALTER TABLE %I FORCE ROW LEVEL SECURITY', t);
    EXECUTE format(
      'CREATE POLICY tenant_isolation ON %I
         USING      (business_id = current_setting(''app.business_id'', true)::uuid)
         WITH CHECK (business_id = current_setting(''app.business_id'', true)::uuid)', t);
  END LOOP;
END $$;
