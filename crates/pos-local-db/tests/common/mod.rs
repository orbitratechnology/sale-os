//! Shared fixture: a real workspace created through SetupService, with an owner context.
#![allow(dead_code)]

pub mod contract;
pub mod coverage;

use pos_application::auth::{Argon2PinHasher, PinHasher};
use pos_application::context::{Actor, CommandContext};
use pos_application::setup::{CreateWorkspace, NewTaxRate, SetupService};
use pos_domain::staff::Pin;
use pos_domain::{
    Clock, IdGenerator, RatePpm, Role, SessionId, TaxCategoryId, Timestamp, UserId, UuidV7Generator,
};
use pos_local_db::{DbOptions, LocalDb, SqliteStore};
use pos_testkit::FixedClock;
use std::sync::Arc;
use tempfile::TempDir;

pub struct Shop {
    pub _dir: TempDir,
    pub store: SqliteStore,
    pub clock: Arc<FixedClock>,
    pub owner: CommandContext,
    pub tax_category: TaxCategoryId,
    /// Cheap Argon2 parameters, for creating staff in tests.
    pub hasher: Arc<dyn PinHasher>,
}

impl Shop {
    /// A context for another role on the same register.
    pub fn as_role(&self, role: Role) -> CommandContext {
        let mut ctx = self.owner.clone();
        ctx.actor = Some(Actor {
            user_id: UserId::generate(&UuidV7Generator),
            role,
            session_id: SessionId::generate(&UuidV7Generator),
        });
        ctx
    }
}

/// GBP, prices include tax, one 20% VAT rate in the default category.
pub async fn shop() -> Shop {
    shop_with("").await
}

/// As [`shop`], running `prepare` (SQL) on the new database before the workspace is set up
/// (e.g. the E17-T02 coverage triggers).
pub async fn shop_with(prepare: &str) -> Shop {
    let dir = TempDir::new().unwrap();
    let store = SqliteStore::new(LocalDb::open(&DbOptions::in_dir(dir.path())).await.unwrap());
    if !prepare.is_empty() {
        sqlx::raw_sql(prepare).execute(store.db().writer()).await.unwrap();
    }
    let hasher: Arc<dyn PinHasher> = Arc::new(Argon2PinHasher::with_params(256, 1, 1));
    let clock = Arc::new(FixedClock::new(Timestamp::from_millis(1_767_225_600_000)));
    let ids: Arc<dyn IdGenerator> = Arc::new(UuidV7Generator);
    let setup = SetupService {
        store: Arc::new(store.clone()),
        setup: Arc::new(store.clone()),
        hasher: hasher.clone(),
        clock: clock.clone() as Arc<dyn Clock>,
        ids: ids.clone(),
    };
    let ws = setup
        .create_workspace(CreateWorkspace {
            business_name: "Shop".into(),
            legal_name: None,
            address: serde_json::json!({}),
            phone: None,
            email: None,
            country_code: "GB".into(),
            timezone: "Europe/London".into(),
            currency_code: "GBP".into(),
            prices_include_tax: true,
            location_name: "Main".into(),
            register_name: "Till".into(),
            owner_name: "Olu".into(),
            owner_pin: Pin::parse("1234").unwrap(),
            tax_rates: vec![NewTaxRate {
                name: "VAT 20%".into(),
                rate: RatePpm::new(200_000).unwrap(),
            }],
        })
        .await
        .unwrap();
    let tax: String = sqlx::query_scalar("SELECT tax_category_id FROM tax_categories")
        .fetch_one(store.db().readers())
        .await
        .unwrap();
    let owner = CommandContext {
        register: ws.register,
        actor: Some(Actor {
            user_id: ws.owner_user_id,
            role: Role::Owner,
            session_id: SessionId::generate(&UuidV7Generator),
        }),
        clock: clock.clone(),
        ids,
        approval: None,
    };
    Shop {
        _dir: dir,
        store,
        clock,
        owner,
        tax_category: tax.parse().unwrap(),
        hasher,
    }
}
