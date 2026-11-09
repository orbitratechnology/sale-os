//! E16-T02: tenancy and row-level security against a real PostgreSQL. Runs when
//! `TEST_DATABASE_URL` is set (CI provides a service container); skipped otherwise.

use pos_cloud_db::repo::{self, NewBusiness};
use pos_cloud_db::{Principal, TenantTx};
use pos_domain::{BusinessId, LocationId, RegisterId, UuidV7Generator};
use sqlx::PgPool;

async fn pool() -> Option<PgPool> {
    let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return None;
    };
    let pool = pos_cloud_db::connect_lazy(&url).unwrap();
    pos_cloud_db::migrate(&pool).await.unwrap();
    Some(pool)
}

/// A business with one location and one register, made through its own TenantTx.
async fn business(pool: &PgPool, name: &str) -> (Principal, RegisterId) {
    let ids = UuidV7Generator;
    let principal = Principal {
        business_id: BusinessId::generate(&ids),
    };
    let mut tx = TenantTx::begin(pool, &principal).await.unwrap();
    repo::insert_business(
        &mut tx,
        &NewBusiness {
            business_id: principal.business_id,
            name: name.into(),
            country_code: "GB".into(),
            timezone: "Europe/London".into(),
            currency_code: "GBP".into(),
            currency_exponent: 2,
            prices_include_tax: true,
        },
    )
    .await
    .unwrap();
    let location = LocationId::generate(&ids);
    repo::insert_location(&mut tx, location, "Main").await.unwrap();
    let register = RegisterId::generate(&ids);
    repo::insert_register(&mut tx, register, location, "Till 1", "R01")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    (principal, register)
}

#[tokio::test]
async fn every_tenant_table_has_forced_rls_and_a_policy() {
    let Some(pool) = pool().await else { return };
    // Every table with a business_id column, and whether RLS is on, forced, and has a policy.
    let rows: Vec<(String, bool, bool, i64)> = sqlx::query_as(
        "SELECT c.relname::text, c.relrowsecurity, c.relforcerowsecurity,
                (SELECT count(*) FROM pg_policy p WHERE p.polrelid = c.oid)
           FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
          WHERE n.nspname = 'public' AND c.relkind = 'r'
            AND EXISTS (SELECT 1 FROM pg_attribute a
                         WHERE a.attrelid = c.oid AND a.attname = 'business_id' AND NOT a.attisdropped)
          ORDER BY 1",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(rows.len() >= 4, "{rows:?}");
    let unprotected: Vec<&String> = rows
        .iter()
        .filter(|(_, enabled, forced, policies)| !(*enabled && *forced && *policies > 0))
        .map(|(t, ..)| t)
        .collect();
    assert!(
        unprotected.is_empty(),
        "tables with business_id but without forced RLS and a policy: {unprotected:?}"
    );
}

#[tokio::test]
async fn pos_app_without_a_business_sees_nothing() {
    let Some(pool) = pool().await else { return };
    business(&pool, "Somebody's shop").await;
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SET LOCAL ROLE pos_app")
        .execute(&mut *tx)
        .await
        .unwrap();
    for table in ["businesses", "business_members", "locations", "registers"] {
        let n: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(n, 0, "{table} is readable without app.business_id");
    }
    // pos_app cannot switch row security off for itself either.
    let bypass: bool = sqlx::query_scalar("SELECT rolbypassrls FROM pg_roles WHERE rolname = 'pos_app'")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert!(!bypass);
}

#[tokio::test]
async fn a_business_sees_only_its_own_rows_and_cannot_write_anothers() {
    let Some(pool) = pool().await else { return };
    let (a, a_register) = business(&pool, "Shop A").await;
    let (b, b_register) = business(&pool, "Shop B").await;

    let mut tx = TenantTx::begin(&pool, &a).await.unwrap();
    assert_eq!(repo::business(&mut tx).await.unwrap().unwrap().name, "Shop A");
    assert!(repo::register(&mut tx, a_register).await.unwrap().is_some());
    // B's register, asked for by id from A's transaction: not found.
    assert_eq!(repo::register(&mut tx, b_register).await.unwrap(), None);
    let all = repo::registers(&mut tx).await.unwrap();
    assert_eq!(
        all.iter().map(|r| r.register_id).collect::<Vec<_>>(),
        vec![*a_register.as_uuid()]
    );
    // Even raw SQL inside A's transaction cannot see B.
    let seen: i64 = sqlx::query_scalar("SELECT count(*) FROM businesses WHERE business_id = $1")
        .bind(*b.business_id.as_uuid())
        .fetch_one(tx.test_conn())
        .await
        .unwrap();
    assert_eq!(seen, 0);
    tx.rollback().await.unwrap();

    // Writing a row for B from A's transaction is refused by the policy's WITH CHECK.
    let mut tx = TenantTx::begin(&pool, &a).await.unwrap();
    let e = sqlx::query(
        "INSERT INTO locations (business_id, location_id, name, status, version, updated_at)
         VALUES ($1, gen_random_uuid(), 'Sneaky', 'active', 1, now())",
    )
    .bind(*b.business_id.as_uuid())
    .execute(tx.test_conn())
    .await
    .unwrap_err();
    assert!(e.to_string().contains("row-level security"), "{e}");
    tx.rollback().await.unwrap();

    // And A cannot update B's rows: nothing matches.
    let mut tx = TenantTx::begin(&pool, &a).await.unwrap();
    let changed = sqlx::query("UPDATE registers SET name = 'Hijacked' WHERE register_id = $1")
        .bind(*b_register.as_uuid())
        .execute(tx.test_conn())
        .await
        .unwrap()
        .rows_affected();
    assert_eq!(changed, 0);
    tx.commit().await.unwrap();
    let mut tx = TenantTx::begin(&pool, &b).await.unwrap();
    assert_eq!(
        repo::register(&mut tx, b_register).await.unwrap().unwrap().name,
        "Till 1"
    );
}

#[tokio::test]
async fn a_reused_connection_still_fails_closed() {
    let Some(pool) = pool().await else { return };
    let (a, _) = business(&pool, "Shop on a reused connection").await;
    // One connection: a tenant transaction, then a transaction with no business set. The
    // setting reads back as '' afterwards, which must mean "no rows", not an error.
    let mut conn = pool.acquire().await.unwrap();
    {
        use sqlx::Connection;
        let mut tx = conn.begin().await.unwrap();
        sqlx::query("SET LOCAL ROLE pos_app")
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query("SELECT set_config('app.business_id', $1, true)")
            .bind(a.business_id.to_string())
            .execute(&mut *tx)
            .await
            .unwrap();
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM businesses")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(n, 1);
        tx.commit().await.unwrap();
        let mut tx = conn.begin().await.unwrap();
        sqlx::query("SET LOCAL ROLE pos_app")
            .execute(&mut *tx)
            .await
            .unwrap();
        for table in ["businesses", "registers", "devices", "device_refresh_tokens"] {
            let n: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
                .fetch_one(&mut *tx)
                .await
                .unwrap();
            assert_eq!(n, 0, "{table}");
        }
    }
}
