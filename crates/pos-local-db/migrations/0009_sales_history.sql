-- 0009: sales history (E12-T03). Pages are newest first, keyed on
-- (business_date, occurred_at, sale_id), so a page is an index range scan whatever the
-- history size (NFR-sales-history: < 300 ms per page at 500k lines).
CREATE INDEX ix_sales_history ON sales(business_date, occurred_at, sale_id);
CREATE INDEX ix_sales_cashier_history ON sales(cashier_user_id, business_date, occurred_at, sale_id);
-- Returns made against a sale (sale detail, refunded totals in the list).
CREATE INDEX ix_returns_original_sale ON returns(original_sale_id);
