-- 0017: parking carts (POS-012). A parked cart keeps its lines and is set aside, so the till
-- can serve the next customer; any cashier on the register can resume it. Carts stay local
-- and are never synced (ADR-009). Append-only once merged to main.

ALTER TABLE carts ADD COLUMN parked_label TEXT;     -- what the cashier typed ("blue coat"), if anything
ALTER TABLE carts ADD COLUMN parked_at INTEGER;
ALTER TABLE carts ADD COLUMN parked_by TEXT REFERENCES staff_users(user_id);
CREATE INDEX ix_carts_parked ON carts(register_id, parked_at) WHERE status = 'parked';
