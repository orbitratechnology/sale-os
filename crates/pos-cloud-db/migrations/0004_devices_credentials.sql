-- 0004 (spec C0002): devices and their refresh tokens (E16-T04, ADR-013), with their
-- row-level security, and the read-only policies that let an owner see across the
-- businesses they belong to. Append-only once merged to main.
CREATE TABLE devices (
  business_id       uuid NOT NULL,
  device_id         uuid NOT NULL,
  register_id       uuid NOT NULL,
  installation_id   uuid NOT NULL,
  app_version       text NOT NULL,
  os_info           text,
  status            text NOT NULL DEFAULT 'active' CHECK (status IN ('active','revoked')),
  registered_by     uuid NOT NULL REFERENCES accounts(account_id),
  registered_at     timestamptz NOT NULL DEFAULT now(),
  last_seen_at      timestamptz,
  revoked_at        timestamptz,
  PRIMARY KEY (business_id, device_id),
  FOREIGN KEY (business_id, register_id) REFERENCES registers(business_id, register_id)
);
CREATE UNIQUE INDEX ux_active_device_per_register ON devices(business_id, register_id) WHERE status = 'active';

CREATE TABLE device_refresh_tokens (     -- rotation family (ADR-013)
  token_hash     bytea PRIMARY KEY,       -- SHA-256 of the opaque token; the token itself is never stored
  business_id    uuid NOT NULL,
  device_id      uuid NOT NULL,
  family_id      uuid NOT NULL,
  issued_at      timestamptz NOT NULL DEFAULT now(),
  expires_at     timestamptz NOT NULL,
  used_at        timestamptz,            -- reuse of a used token revokes the family and the device
  FOREIGN KEY (business_id, device_id) REFERENCES devices(business_id, device_id)
);
CREATE INDEX ix_refresh_family ON device_refresh_tokens(business_id, family_id);

GRANT SELECT, INSERT, UPDATE ON devices, device_refresh_tokens TO pos_app;

-- Tenant isolation, as for every tenant table (0003).
DO $$
DECLARE t text;
BEGIN
  FOREACH t IN ARRAY ARRAY['devices', 'device_refresh_tokens'] LOOP
    EXECUTE format('ALTER TABLE %I ENABLE ROW LEVEL SECURITY', t);
    EXECUTE format('ALTER TABLE %I FORCE ROW LEVEL SECURITY', t);
    EXECUTE format(
      'CREATE POLICY tenant_isolation ON %I
         USING      (business_id = nullif(current_setting(''app.business_id'', true), '''')::uuid)
         WITH CHECK (business_id = nullif(current_setting(''app.business_id'', true), '''')::uuid)', t);
  END LOOP;
END $$;

-- An owner (`app.account_id`, set only from a verified user token) may READ the businesses
-- they belong to, and those businesses' devices. Permissive policies are OR-ed with
-- tenant_isolation; they add no write access. Without the setting they match nothing.
CREATE POLICY account_membership ON business_members FOR SELECT
  USING (account_id = nullif(current_setting('app.account_id', true), '')::uuid);
CREATE POLICY account_businesses ON businesses FOR SELECT
  USING (EXISTS (SELECT 1 FROM business_members m
                  WHERE m.business_id = businesses.business_id
                    AND m.account_id = nullif(current_setting('app.account_id', true), '')::uuid));
CREATE POLICY account_devices ON devices FOR SELECT
  USING (EXISTS (SELECT 1 FROM business_members m
                  WHERE m.business_id = devices.business_id
                    AND m.account_id = nullif(current_setting('app.account_id', true), '')::uuid));
CREATE POLICY account_registers ON registers FOR SELECT
  USING (EXISTS (SELECT 1 FROM business_members m
                  WHERE m.business_id = registers.business_id
                    AND m.account_id = nullif(current_setting('app.account_id', true), '')::uuid));

-- A refresh request knows only its token: it may read that one row, by hash.
CREATE POLICY refresh_lookup ON device_refresh_tokens FOR SELECT
  USING (token_hash = decode(nullif(current_setting('app.refresh_token_hash', true), ''), 'hex'));
