-- 0019: the catalogue price each cart line was last shown at (E09-T02, POS-011). After a
-- restart or a resume the cart is priced against the current catalogue; a line whose price
-- differs from what the cashier last saw is highlighted. NULL for lines saved before 0019.
-- Append-only once merged to main.

ALTER TABLE cart_lines ADD COLUMN price_seen_minor INTEGER;
