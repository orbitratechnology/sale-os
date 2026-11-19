-- 0018: scale labels on cart lines (BAR-009). A line scanned from a weight or price label keeps
-- what the label set: its quantity cannot be edited, and a price label fixes the line's gross.
-- Append-only once merged to main.

ALTER TABLE cart_lines ADD COLUMN label_kind TEXT CHECK (label_kind IS NULL OR label_kind IN ('weight','price'));
ALTER TABLE cart_lines ADD COLUMN label_price_minor INTEGER CHECK (
  (label_kind = 'price' AND label_price_minor IS NOT NULL AND label_price_minor >= 0)
  OR (label_kind IS NOT 'price' AND label_price_minor IS NULL));
