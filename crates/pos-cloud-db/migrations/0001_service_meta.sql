-- 0001_service_meta: bootstrap table proving the migration pipeline (E16-T01).
-- Tenancy, identity and sync tables arrive from E16-T02 (docs/backlog/05-cloud-database.md).
CREATE TABLE IF NOT EXISTS service_meta (
  key         text PRIMARY KEY,
  value       text NOT NULL,
  updated_at  timestamptz NOT NULL DEFAULT now()
);
INSERT INTO service_meta (key, value) VALUES ('schema_family', 'sale-os-cloud') ON CONFLICT (key) DO NOTHING;
