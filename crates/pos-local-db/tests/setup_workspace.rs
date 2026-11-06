//! E04-T01 acceptance: first-run workspace creation, offline, in one transaction.

use pos_application::LocalStore;
use pos_application::auth::{Argon2PinHasher, AuthService, PinHasher};
use pos_application::context::{AppError, CommandContext};
use pos_application::queries::{SetupQueries, StaffQueries};
use pos_application::setup::{CreateWorkspace, NewTaxRate, SetupError, SetupService};
use pos_domain::staff::{Pin, StaffError};
use pos_domain::{Clock, ErrorCode, HasErrorCode, IdGenerator, RatePpm, Timestamp, UuidV7Generator};
use pos_local_db::{DbOptions, LocalDb, SqliteStore};
use pos_testkit::FixedClock;
use std::sync::Arc;
use tempfile::TempDir;

struct Env {
    _dir: TempDir,
    store: SqliteStore,
    setup: SetupService,
    hasher: Arc<dyn PinHasher>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
}

async fn env() -> Env {
    let dir = TempDir::new().unwrap();
    let store = SqliteStore::new(LocalDb::open(&DbOptions::in_dir(dir.path())).await.unwrap());
    let hasher: Arc<dyn PinHasher> = Arc::new(Argon2PinHasher::with_params(256, 1, 1));
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::new(Timestamp::from_millis(1_767_225_600_000)));
    let ids: Arc<dyn IdGenerator> = Arc::new(UuidV7Generator);
    let setup = SetupService {
        store: Arc::new(store.clone()),
        setup: Arc::new(store.clone()),
        hasher: hasher.clone(),
        clock: clock.clone(),
        ids: ids.clone(),
    };
    Env {
        _dir: dir,
        store,
        setup,
        hasher,
        clock,
        ids,
    }
}

fn input() -> CreateWorkspace {
    CreateWorkspace {
        business_name: " Corner Shop ".into(),
        legal_name: Some("".into()),
        address: serde_json::json!({ "line1": "1 High St", "city": "Leeds" }),
        phone: None,
        email: None,
        country_code: "gb".into(),
        timezone: "Europe/London".into(),
        currency_code: "GBP".into(),
        prices_include_tax: true,
        location_name: "Main".into(),
        register_name: "Till 1".into(),
        owner_name: "Olu".into(),
        owner_pin: Pin::parse("1234").unwrap(),
        tax_rates: vec![
            NewTaxRate {
                name: "VAT 20%".into(),
                rate: RatePpm::new(200_000).unwrap(),
            },
            NewTaxRate {
                name: "VAT 5%".into(),
                rate: RatePpm::new(50_000).unwrap(),
            },
        ],
    }
}

async fn scalar(store: &SqliteStore, sql: &str) -> String {
    sqlx::query_scalar::<_, String>(sql)
        .fetch_one(store.db().readers())
        .await
        .unwrap()
}

#[tokio::test]
async fn creates_the_whole_workspace_in_one_transaction() {
    let e = env().await;
    let ws = e.setup.create_workspace(input()).await.unwrap();
    assert_eq!(ws.register_code, "R01");

    assert_eq!(
        scalar(&e.store, "SELECT name FROM businesses").await,
        "Corner Shop"
    );
    assert_eq!(
        scalar(&e.store, "SELECT country_code FROM businesses").await,
        "GB"
    );
    assert_eq!(
        scalar(
            &e.store,
            "SELECT currency_code || ':' || currency_exponent FROM businesses"
        )
        .await,
        "GBP:2"
    );
    assert_eq!(scalar(&e.store, "SELECT code FROM registers").await, "R01");
    assert_eq!(
        scalar(&e.store, "SELECT mode FROM device_identity").await,
        "sandbox"
    );
    assert_eq!(scalar(&e.store, "SELECT role FROM staff_users").await, "owner");
    assert_eq!(
        scalar(
            &e.store,
            "SELECT group_concat(r.rate_ppm) FROM tax_category_rates c JOIN tax_rates r USING (tax_rate_id)"
        )
        .await,
        "200000,50000"
    );
    let identity = e.store.device_identity().await.unwrap().unwrap();
    assert_eq!(identity.register_id, ws.register.register_id);

    // AT-01 precondition and ADR-009: every record is in the outbox, sequenced from 1.
    let events: Vec<(i64, String)> =
        sqlx::query_as("SELECT local_sequence, event_type FROM outbox_events ORDER BY local_sequence")
            .fetch_all(e.store.db().readers())
            .await
            .unwrap();
    assert_eq!(events.first().map(|e| e.0), Some(1));
    assert!(events.windows(2).all(|w| w[1].0 == w[0].0 + 1), "gapless");
    let types: std::collections::BTreeSet<&str> = events.iter().map(|e| e.1.as_str()).collect();
    for t in [
        "business.updated",
        "location.upserted",
        "register.upserted",
        "staff.upserted",
        "tax_rate.upserted",
        "tax_category.upserted",
        "setting.updated",
    ] {
        assert!(types.contains(t), "missing {t}");
    }
    let settings: i64 = sqlx::query_scalar("SELECT count(*) FROM business_settings")
        .fetch_one(e.store.db().readers())
        .await
        .unwrap();
    assert_eq!(settings, pos_application::settings::defaults().len() as i64);
}

#[tokio::test]
async fn the_owner_can_sign_in_straight_after_setup() {
    let e = env().await;
    let ws = e.setup.create_workspace(input()).await.unwrap();
    let auth = AuthService {
        store: Arc::new(e.store.clone()),
        staff: Arc::new(e.store.clone()),
        hasher: e.hasher.clone(),
        approvals: Arc::new(pos_application::approval::Approvals::default()),
    };
    let ctx = CommandContext {
        register: ws.register.clone(),
        actor: None,
        clock: e.clock.clone(),
        ids: e.ids.clone(),
        approval: None,
    };
    let grant = auth
        .login(&ctx, ws.owner_user_id, Pin::parse("1234").unwrap())
        .await
        .unwrap();
    assert_eq!(grant.display_name, "Olu");
    assert_eq!(e.store.active_staff().await.unwrap().len(), 1);
}

#[tokio::test]
async fn setup_runs_once() {
    let e = env().await;
    let ws = e.setup.create_workspace(input()).await.unwrap();
    let err = e.setup.create_workspace(input()).await.unwrap_err();
    assert_eq!(err, AppError::Setup(SetupError::AlreadyDone));
    assert_eq!(err.error_code(), ErrorCode::PermissionDenied);
    // Even bypassing the service's pre-check, the database refuses a second identity.
    let rebind = pos_application::WriteSet::builder(pos_application::WriteContext {
        business_id: ws.register.business_id,
        location_id: ws.register.location_id,
        register_id: ws.register.register_id,
        actor: None,
        occurred_at: e.clock.now(),
    })
    .set_device_identity(pos_application::records::DeviceIdentity {
        installation_id: "other".into(),
        business_id: ws.register.business_id,
        register_id: ws.register.register_id,
        mode: pos_application::records::RegisterMode::Live,
    })
    .build()
    .unwrap();
    assert!(matches!(
        e.store.apply(rebind).await,
        Err(pos_application::WriteError::UniqueViolation { .. })
    ));
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM businesses")
        .fetch_one(e.store.db().readers())
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn validation() {
    let e = env().await;
    type Mutation = Box<dyn Fn(&mut CreateWorkspace)>;
    let cases: Vec<(Mutation, AppError)> = vec![
        (
            Box::new(|i| i.business_name = "  ".into()),
            AppError::Setup(SetupError::Required("business name")),
        ),
        (
            Box::new(|i| i.country_code = "UK".into()),
            AppError::Setup(SetupError::Country("UK".into())),
        ),
        (
            Box::new(|i| i.timezone = "Europe/Londn".into()),
            AppError::Setup(SetupError::TimeZone("Europe/Londn".into())),
        ),
        (
            Box::new(|i| i.owner_name = "".into()),
            AppError::Staff(StaffError::NameRequired),
        ),
        (
            Box::new(|i| {
                i.tax_rates.push(NewTaxRate {
                    name: "vat 20%".into(),
                    rate: RatePpm::ZERO,
                })
            }),
            AppError::Setup(SetupError::RateName),
        ),
        (
            Box::new(|i| {
                for n in 0..2 {
                    i.tax_rates.push(NewTaxRate {
                        name: format!("Extra {n}"),
                        rate: RatePpm::ZERO,
                    });
                }
            }),
            AppError::Setup(SetupError::TooManyRates),
        ),
    ];
    for (mutate, expected) in cases {
        let mut i = input();
        mutate(&mut i);
        assert_eq!(e.setup.create_workspace(i).await.unwrap_err(), expected);
    }
    let mut i = input();
    i.currency_code = "XYZ".into();
    assert!(matches!(
        e.setup.create_workspace(i).await.unwrap_err(),
        AppError::Setup(SetupError::Currency(_))
    ));
    // Nothing was written by any failed attempt.
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM outbox_events")
        .fetch_one(e.store.db().readers())
        .await
        .unwrap();
    assert_eq!(count, 0);
    // An exempt business (no rates) is valid: its default category applies no tax.
    let mut i = input();
    i.tax_rates.clear();
    e.setup.create_workspace(i).await.unwrap();
    let _: &dyn LocalStore = &e.store;
}
