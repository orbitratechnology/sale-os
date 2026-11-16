//! E17-T08: linking registers to the cloud, end to end over HTTP.

mod common;

use common::{cloud, register};
use pos_application::context::AppError;
use pos_application::link::LinkPath;
use pos_application::sync::{Next, SyncStore};
use pos_domain::{ErrorCode, HasErrorCode, Role, Timestamp};
use std::collections::BTreeMap;

/// A week offline at a busy shop: 7 days of sales on a register that was never linked.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn create_path_a_week_offline_links_and_the_cloud_books_match_the_register() {
    let Some(cloud) = cloud().await else { return };
    let (email, password) = cloud.owner().await;
    let r = register(&cloud, "1234").await;
    let items = [
        r.product("Cola", 120).await,
        r.product("Bread", 250).await,
        r.product("Gum", 75).await,
    ];
    r.go_live().await;
    let carts = r.carts();
    let day_ms = 86_400_000;
    for day in 0..7 {
        r.clock
            .set(Timestamp::from_millis(1_767_258_000_000 + day * day_ms));
        for i in 0..420 {
            r.sell(&carts, items[(i % 3) as usize], 1 + i % 3).await;
        }
    }
    let events = r.local("SELECT count(*) FROM outbox_events").await;
    assert!(events >= 2_950, "{events}");

    // The owner links: create the cloud business from this register.
    let owner = r.sign_in(&email, &password).await;
    let started = std::time::Instant::now();
    let linked = r.link.create(&r.owner, &owner, false).await.unwrap();
    assert_eq!(linked.path, LinkPath::Create);
    assert_eq!(r.sync().await, Next::Idle);
    eprintln!("{events} events linked and drained in {:?}", started.elapsed());
    // Every event applied.
    let state = r.store.push_state().await.unwrap();
    assert_eq!(state.last_ack_sequence, state.max_sequence);
    assert_eq!(
        r.local("SELECT count(*) FROM outbox_events WHERE status NOT IN ('applied', 'duplicate')")
            .await,
        0
    );
    // The cloud's books equal the register's, day by day.
    let local: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT business_date, count(*), sum(total_minor) FROM sales WHERE mode = 'live' GROUP BY business_date ORDER BY 1",
    )
    .fetch_all(r.store.db().readers())
    .await
    .unwrap();
    let cloud_rows: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT business_date::text, sum(sales_count)::bigint, sum(sales_total_minor)::bigint FROM daily_sales_rollup
          WHERE business_id = $1 GROUP BY business_date ORDER BY 1",
    )
    .bind(linked.business_id)
    .fetch_all(&cloud.pool)
    .await
    .unwrap();
    assert_eq!(local.len(), 7);
    assert_eq!(cloud_rows, local);
    // Linking is audited.
    assert_eq!(
        r.local("SELECT count(*) FROM audit_events WHERE action = 'sync.linked'")
            .await,
        1
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn join_path_a_second_register_receives_the_business_and_numbers_its_own_receipts() {
    let Some(cloud) = cloud().await else { return };
    let (email, password) = cloud.owner().await;
    let one = register(&cloud, "1234").await;
    let cola = one.product("Cola", 120).await;
    one.product("Bread", 250).await;
    let owner = one.sign_in(&email, &password).await;
    let linked = one.link.create(&one.owner, &owner, false).await.unwrap();
    one.sync().await;

    // Register 2 was set up on its own (its PIN differs) and has sold nothing yet.
    let two = register(&cloud, "9999").await;
    let (owner2, options) = two.link.start(&two.owner, &email, &password).await.unwrap();
    assert!(options.may_join);
    assert!(
        options
            .businesses
            .iter()
            .any(|b| b.business_id == linked.business_id)
    );
    let joined = two
        .link
        .join(&two.owner, &owner2, linked.business_id)
        .await
        .unwrap();
    assert_eq!(
        (joined.path, joined.register_code.as_str()),
        (LinkPath::Join, "R02")
    );
    // It now holds register 1's business: its catalogue, its settings and its staff.
    assert_eq!(two.local("SELECT count(*) FROM products").await, 2);
    assert_eq!(two.local("SELECT count(*) FROM businesses").await, 1);
    let business: String = sqlx::query_scalar("SELECT business_id FROM device_identity")
        .fetch_one(two.store.db().readers())
        .await
        .unwrap();
    assert_eq!(business, linked.business_id.to_string());
    assert_eq!(
        two.local("SELECT count(*) FROM business_settings").await,
        one.local("SELECT count(*) FROM business_settings").await
    );
    // Register 1's owner signs in on register 2 with register 1's PIN.
    let staff: (String, String) = sqlx::query_as("SELECT user_id, role FROM staff_users")
        .fetch_one(two.store.db().readers())
        .await
        .unwrap();
    assert_eq!(staff.1, "owner");
    let auth = pos_application::auth::AuthService {
        store: std::sync::Arc::new(two.store.clone()),
        staff: std::sync::Arc::new(two.store.clone()),
        hasher: two.hasher.clone(),
        approvals: std::sync::Arc::new(pos_application::approval::Approvals::default()),
    };
    let mut ctx = two.owner.clone();
    ctx.register = pos_application::context::RegisterContext {
        business_id: pos_domain::BusinessId::from_uuid(linked.business_id),
        location_id: sqlx::query_scalar::<_, String>("SELECT location_id FROM locations")
            .fetch_one(two.store.db().readers())
            .await
            .unwrap()
            .parse()
            .unwrap(),
        register_id: sqlx::query_scalar::<_, String>("SELECT register_id FROM device_identity")
            .fetch_one(two.store.db().readers())
            .await
            .unwrap()
            .parse()
            .unwrap(),
    };
    ctx.actor = None;
    let grant = auth
        .login(
            &ctx,
            staff.0.parse().unwrap(),
            pos_domain::staff::Pin::parse("1234").unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(grant.role, Role::Owner);
    // Its first sale is numbered for its own register code, and reaches the cloud.
    ctx.actor = Some(pos_application::context::Actor {
        user_id: staff.0.parse().unwrap(),
        role: Role::Owner,
        session_id: pos_domain::SessionId::generate(&pos_domain::UuidV7Generator),
    });
    let mut two = two;
    two.owner = ctx;
    two.go_live().await;
    let sale = two.sell(&two.carts(), cola, 1).await;
    assert_eq!(sale.receipt_number, "R02-000001");
    assert_eq!(two.sync().await, Next::Idle);
    let in_cloud: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sales WHERE business_id = $1 AND receipt_number = 'R02-000001'",
    )
    .bind(linked.business_id)
    .fetch_one(&cloud.pool)
    .await
    .unwrap();
    assert_eq!(in_cloud, 1);
    // And register 1 sees its stock move.
    one.sync().await;
    assert_eq!(
        one.local(&format!(
            "SELECT on_hand_milli FROM stock_balances WHERE variant_id = '{cola}'"
        ))
        .await,
        -1000
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_register_with_its_own_sales_or_products_cannot_join_and_only_the_owner_links() {
    let Some(cloud) = cloud().await else { return };
    let (email, password) = cloud.owner().await;
    let one = register(&cloud, "1234").await;
    let owner = one.sign_in(&email, &password).await;
    let linked = one.link.create(&one.owner, &owner, false).await.unwrap();
    let two = register(&cloud, "1234").await;
    two.product("Tea", 100).await;
    let (owner2, options) = two.link.start(&two.owner, &email, &password).await.unwrap();
    assert!(!options.may_join);
    let err = two
        .link
        .join(&two.owner, &owner2, linked.business_id)
        .await
        .unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::SyncJoinNotAllowed);
    assert_eq!(
        two.local("SELECT count(*) FROM products").await,
        1,
        "nothing was wiped"
    );
    // A cashier cannot link.
    let mut cashier = two.owner.clone();
    cashier.actor.as_mut().unwrap().role = Role::Cashier;
    let err = two.link.start(&cashier, &email, &password).await.unwrap_err();
    assert!(matches!(err, AppError::PermissionDenied(_)), "{err:?}");
    // A wrong password is refused without saying which part was wrong.
    let err = two
        .link
        .start(&two.owner, &email, &pos_domain::Secret::new("wrong".into()))
        .await
        .unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::AuthCredentialsInvalid);
}

/// Fails the first bootstrap call: the network drops mid-join.
struct Flaky {
    inner: std::sync::Arc<pos_sync_http::DeviceSession>,
    failed: std::sync::atomic::AtomicBool,
}

#[async_trait::async_trait]
impl pos_application::sync::SyncTransport for Flaky {
    async fn push(
        &self,
        r: pos_sync::PushRequest,
    ) -> Result<pos_sync::PushResponse, pos_application::sync::TransportError> {
        self.inner.push(r).await
    }
    async fn bootstrap(
        &self,
        page: Option<String>,
    ) -> Result<pos_sync::pull::BootstrapPage, pos_application::sync::TransportError> {
        if !self.failed.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return Err(pos_application::sync::TransportError::Offline);
        }
        self.inner.bootstrap(page).await
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_join_that_loses_the_network_resumes_without_registering_again_or_wiping_early() {
    let Some(cloud) = cloud().await else { return };
    let (email, password) = cloud.owner().await;
    let one = register(&cloud, "1234").await;
    one.product("Cola", 120).await;
    let owner = one.sign_in(&email, &password).await;
    let linked = one.link.create(&one.owner, &owner, false).await.unwrap();
    one.sync().await;
    let mut two = register(&cloud, "9999").await;
    two.link.transport = std::sync::Arc::new(Flaky {
        inner: two.session.clone(),
        failed: std::sync::atomic::AtomicBool::new(false),
    });
    let (owner2, _) = two.link.start(&two.owner, &email, &password).await.unwrap();
    let err = two
        .link
        .join(&two.owner, &owner2, linked.business_id)
        .await
        .unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::SyncOffline);
    // Still its own business, and the link is waiting to resume.
    assert_eq!(two.local("SELECT count(*) FROM products").await, 0);
    let state: String = sqlx::query_scalar("SELECT link_state FROM sync_state")
        .fetch_one(two.store.db().readers())
        .await
        .unwrap();
    assert_eq!(state, "linking");
    let joined = two
        .link
        .join(&two.owner, &owner2, linked.business_id)
        .await
        .unwrap();
    assert_eq!(joined.register_code, "R02");
    assert_eq!(two.local("SELECT count(*) FROM products").await, 1);
    // One device for that register: the retry did not register again.
    let devices: i64 = sqlx::query_scalar("SELECT count(*) FROM devices WHERE business_id = $1")
        .bind(linked.business_id)
        .fetch_one(&cloud.pool)
        .await
        .unwrap();
    assert_eq!(devices, 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_register_disconnected_by_the_owner_stops_syncing_and_keeps_selling() {
    let Some(cloud) = cloud().await else { return };
    let (email, password) = cloud.owner().await;
    let r = register(&cloud, "1234").await;
    let tea = r.product("Tea", 100).await;
    let owner = r.sign_in(&email, &password).await;
    r.link.create(&r.owner, &owner, false).await.unwrap();
    r.sync().await;
    // The owner revokes the device in the cloud.
    let device: String = sqlx::query_scalar("SELECT device_id FROM sync_state")
        .fetch_one(r.store.db().readers())
        .await
        .unwrap();
    sqlx::query("UPDATE devices SET status = 'revoked', revoked_at = now() WHERE device_id = $1::uuid")
        .bind(&device)
        .execute(&cloud.pool)
        .await
        .unwrap();
    r.go_live().await;
    r.sell(&r.carts(), tea, 1).await;
    let next = r.worker.push_once().await.unwrap().next;
    assert_eq!(next, Next::Stopped("SYNC_DEVICE_REVOKED"));
    // Local sales go on.
    r.sell(&r.carts(), tea, 1).await;
    let _ = BTreeMap::<u8, u8>::new();
}
