//! E17-T02: the outbox covers every syncable write. Triggers (installed before setup) log
//! each insert and update of a syncable table with the aggregate it belongs to, and the
//! outbox's high-water mark at that moment. `apply` writes a set's rows first and its outbox
//! events last, in one transaction, so a write's event must have a sequence above that mark
//! and not above the next transaction's.

use pos_local_db::SqliteStore;
use std::collections::BTreeSet;

/// Syncable tables: (table, aggregate type, aggregate id), as SQL over `NEW`.
pub const SYNCABLE: &[(&str, &str, &str)] = &[
    ("businesses", "'business'", "NEW.business_id"),
    ("locations", "'location'", "NEW.location_id"),
    ("registers", "'register'", "NEW.register_id"),
    ("business_settings", "'setting'", "NEW.key"),
    ("staff_users", "'staff_user'", "NEW.user_id"),
    ("tax_rates", "'tax_rate'", "NEW.tax_rate_id"),
    ("tax_categories", "'tax_category'", "NEW.tax_category_id"),
    ("tax_category_rates", "'tax_category'", "NEW.tax_category_id"),
    ("categories", "'category'", "NEW.category_id"),
    ("products", "'product'", "NEW.product_id"),
    ("product_variants", "'product'", "NEW.product_id"),
    (
        "product_barcodes",
        "'product'",
        "(SELECT product_id FROM product_variants WHERE variant_id = NEW.variant_id)",
    ),
    ("sales", "'sale'", "NEW.sale_id"),
    ("sale_lines", "'sale'", "NEW.sale_id"),
    ("tenders", "'sale'", "NEW.sale_id"),
    ("returns", "'return'", "NEW.return_id"),
    ("return_lines", "'return'", "NEW.return_id"),
    ("refunds", "'return'", "NEW.return_id"),
    ("inventory_documents", "'inventory_document'", "NEW.document_id"),
    ("stock_movements", "NEW.source_type", "NEW.source_id"),
    ("cash_sessions", "'cash_session'", "NEW.cash_session_id"),
];

/// Tables that never reach the cloud, and why.
pub const LOCAL_ONLY: &[(&str, &str)] = &[
    ("device_identity", "this installation"),
    (
        "register_counters",
        "number allocation (numbers travel inside the facts)",
    ),
    (
        "audit_events",
        "the local log; standalone entries travel as audit.recorded",
    ),
    ("outbox_events", "the outbox itself"),
    ("sync_state", "sync bookkeeping"),
    ("staff_login_throttle", "device-local throttle"),
    ("staff_sessions", "device-local sessions"),
    ("units", "seed data"),
    ("product_search", "search index (rebuilt from products)"),
    ("carts", "drafts (ADR-009)"),
    ("cart_lines", "drafts (ADR-009)"),
    ("scan_events_seen", "scan de-duplication window"),
    ("stock_balances", "projection of stock_movements"),
    (
        "tender_status_events",
        "local history; tender.status_changed waits for E10-T04",
    ),
    ("receipts", "rendered from the sale or return, which travel"),
    ("print_jobs", "this printer"),
    ("print_job_events", "this printer"),
    ("import_jobs", "import staging (the products travel)"),
    ("import_rows", "import staging"),
    ("sync_conflicts", "sync bookkeeping"),
    ("inbound_changes", "sync bookkeeping"),
];

/// Aggregate types that may appear in the outbox.
const SYNCED_AGGREGATES: &[&str] = &[
    "business",
    "location",
    "register",
    "setting",
    "staff_user",
    "tax_rate",
    "tax_category",
    "category",
    "product",
    "sale",
    "return",
    "inventory_document",
    "cash_session",
    "audit",
];

/// A captured write: table, operation, aggregate type and id, outbox mark.
type Write = (String, String, Option<String>, Option<String>, i64);

/// SQL creating the capture log and its triggers; run before anything is written.
pub fn triggers_sql() -> String {
    let mut sql = String::from(
        "CREATE TABLE _sync_coverage (
           id INTEGER PRIMARY KEY, tbl TEXT NOT NULL, op TEXT NOT NULL,
           aggregate_type TEXT, aggregate_id TEXT, outbox_mark INTEGER NOT NULL);\n",
    );
    for (table, agg_type, agg_id) in SYNCABLE {
        for op in ["INSERT", "UPDATE"] {
            sql.push_str(&format!(
                "CREATE TRIGGER _cov_{table}_{op} AFTER {op} ON {table} BEGIN
                   INSERT INTO _sync_coverage (tbl, op, aggregate_type, aggregate_id, outbox_mark)
                   VALUES ('{table}', '{op}', {agg_type}, {agg_id},
                           (SELECT coalesce(max(local_sequence), 0) FROM outbox_events));
                 END;\n"
            ));
        }
    }
    sql
}

/// Checks the run: every syncable write has its event in the same transaction, local-only
/// tables emit nothing, every table is classified. Returns the syncable tables written.
pub async fn check(store: &SqliteStore) -> BTreeSet<String> {
    let r = store.db().readers();
    // Every table is classified (a new one must be added to one list or the other).
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
           AND name NOT LIKE '\\_%' ESCAPE '\\' AND name NOT LIKE 'product_search_%'",
    )
    .fetch_all(r)
    .await
    .unwrap();
    let known: BTreeSet<&str> = SYNCABLE
        .iter()
        .map(|(t, ..)| *t)
        .chain(LOCAL_ONLY.iter().map(|(t, _)| *t))
        .collect();
    let unclassified: Vec<&String> = tables.iter().filter(|t| !known.contains(t.as_str())).collect();
    assert!(
        unclassified.is_empty(),
        "classify these tables as syncable or local-only: {unclassified:?}"
    );

    let writes: Vec<Write> = sqlx::query_as(
        "SELECT tbl, op, aggregate_type, aggregate_id, outbox_mark FROM _sync_coverage ORDER BY id",
    )
    .fetch_all(r)
    .await
    .unwrap();
    assert!(!writes.is_empty());
    let events: Vec<(i64, String, String)> = sqlx::query_as(
        "SELECT local_sequence, aggregate_type, aggregate_id FROM outbox_events ORDER BY local_sequence",
    )
    .fetch_all(r)
    .await
    .unwrap();
    // Transaction boundaries, from the distinct outbox marks.
    let marks: Vec<i64> = {
        let mut m: Vec<i64> = writes.iter().map(|w| w.4).collect();
        m.sort_unstable();
        m.dedup();
        m
    };
    let mut missing = Vec::new();
    let mut touched = BTreeSet::new();
    for (tbl, op, agg_type, agg_id, mark) in &writes {
        touched.insert(tbl.clone());
        let (Some(agg_type), Some(agg_id)) = (agg_type, agg_id) else {
            missing.push(format!("{op} {tbl}: no aggregate"));
            continue;
        };
        let next = marks.iter().find(|m| *m > mark).copied().unwrap_or(i64::MAX);
        let found = events
            .iter()
            .any(|(seq, t, id)| seq > mark && *seq <= next && t == agg_type && id == agg_id);
        if !found {
            missing.push(format!("{op} {tbl} ({agg_type} {agg_id}) after outbox #{mark}"));
        }
    }
    assert!(
        missing.is_empty(),
        "syncable writes without an outbox event in their transaction:\n{}",
        missing.join("\n")
    );
    // Nothing local-only travels.
    let types: Vec<String> = sqlx::query_scalar("SELECT DISTINCT aggregate_type FROM outbox_events")
        .fetch_all(r)
        .await
        .unwrap();
    for t in types {
        assert!(
            SYNCED_AGGREGATES.contains(&t.as_str()),
            "outbox event for local-only aggregate {t}"
        );
    }
    // An audit row that names its outbox event names a real one.
    let dangling: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events a WHERE a.outbox_event_id IS NOT NULL
           AND NOT EXISTS (SELECT 1 FROM outbox_events o WHERE o.event_id = a.outbox_event_id)",
    )
    .fetch_one(r)
    .await
    .unwrap();
    assert_eq!(dangling, 0);
    touched
}
