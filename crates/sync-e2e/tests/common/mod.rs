//! A real cloud (the API on a local port, over HTTP, against `TEST_DATABASE_URL`) and real
//! registers (SQLite in temp dirs) wired the way the app wires them.
#![allow(dead_code)]

pub mod proxy;

use pos_application::auth::{Argon2PinHasher, PinHasher};
use pos_application::cart::CartService;
use pos_application::cash::CashService;
use pos_application::catalog::CatalogService;
use pos_application::checkout::{CheckoutService, CompleteSale, CompletedSale};
use pos_application::context::{Actor, CommandContext};
use pos_application::link::{LinkService, OwnerSession};
use pos_application::setup::{CreateWorkspace, NewTaxRate, SetupService};
use pos_application::sync::{Next, OsRandom, SyncWorker};
use pos_domain::cart::AddVia;
use pos_domain::catalog::{ProductInput, ProductType, Unit, VariantInput};
use pos_domain::settlement::{TenderInput, TenderKind};
use pos_domain::staff::Pin;
use pos_domain::{
    Clock, IdGenerator, Money, Quantity, RatePpm, Role, Secret, SessionId, TaxCategoryId, Timestamp,
    UuidV7Generator, VariantId,
};
use pos_local_db::{DbOptions, LocalDb, SqliteStore};
use pos_sync_http::{CloudClient, DeviceSession, MemoryCredentials};
use pos_testkit::FixedClock;
use std::sync::Arc;
use tempfile::TempDir;

/// The cloud API serving on a free local port.
pub struct Cloud {
    pub url: String,
    pub pool: sqlx::PgPool,
}

pub async fn cloud() -> Option<Cloud> {
    let Ok(db) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return None;
    };
    let pool = pos_cloud_db::connect_lazy(&db).unwrap();
    pos_cloud_db::migrate(&pool).await.unwrap();
    let state = cloud_api::AppState::new(cloud_api::Config::for_tests(), Some(pool.clone()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, cloud_api::router(state)).await.unwrap();
    });
    Some(Cloud { url, pool })
}

impl Cloud {
    /// Another server on the same database, with its own configuration (E19-T09: a raised
    /// minimum app version). Returns its URL.
    pub async fn serve_with(&self, config: cloud_api::Config) -> String {
        let state = cloud_api::AppState::new(config, Some(self.pool.clone()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, cloud_api::router(state)).await.unwrap();
        });
        url
    }

    /// A new owner account; returns its credentials.
    pub async fn owner(&self) -> (String, Secret<String>) {
        let email = format!("owner-{}@example.com", uuid::Uuid::now_v7().simple());
        let password = "correct horse battery staple";
        let http = CloudClient::new(&self.url, "0.1.0").unwrap();
        let _ = http; // the account API has no sign-up call: use the endpoint directly
        let body = serde_json::json!({"email": email, "password": password});
        let status = raw_post(&format!("{}/v1/auth/signup", self.url), &body.to_string()).await;
        assert!(status.starts_with("HTTP/1.1 201"), "{status}");
        (email, Secret::new(password.into()))
    }
}

/// A minimal HTTP/1.1 POST (sign-up is not part of the register's client).
async fn raw_post(url: &str, body: &str) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let rest = url.strip_prefix("http://").unwrap();
    let (host, path) = rest.split_once('/').unwrap();
    let mut s = tokio::net::TcpStream::connect(host).await.unwrap();
    let req = format!(
        "POST /{path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    out
}

/// A register: its database, services, and its link to the cloud.
pub struct Register {
    pub _dir: TempDir,
    pub store: SqliteStore,
    pub clock: Arc<FixedClock>,
    pub owner: CommandContext,
    pub tax_category: TaxCategoryId,
    pub hasher: Arc<dyn PinHasher>,
    pub session: Arc<DeviceSession>,
    pub credentials: Arc<MemoryCredentials>,
    pub link: LinkService,
    pub worker: SyncWorker,
}

pub async fn register(cloud: &Cloud, owner_pin: &str) -> Register {
    register_at(&cloud.url, owner_pin).await
}

/// A register that reaches the cloud at `url` (the cloud itself, or a proxy in front of it).
pub async fn register_at(url: &str, owner_pin: &str) -> Register {
    let dir = TempDir::new().unwrap();
    let store = SqliteStore::new(LocalDb::open(&DbOptions::in_dir(dir.path())).await.unwrap());
    let hasher: Arc<dyn PinHasher> = Arc::new(Argon2PinHasher::with_params(256, 1, 1));
    let clock = Arc::new(FixedClock::new(Timestamp::from_millis(1_767_225_600_000)));
    let ids: Arc<dyn IdGenerator> = Arc::new(UuidV7Generator);
    let ws = SetupService {
        store: Arc::new(store.clone()),
        setup: Arc::new(store.clone()),
        hasher: hasher.clone(),
        clock: clock.clone() as Arc<dyn Clock>,
        ids: ids.clone(),
    }
    .create_workspace(CreateWorkspace {
        business_name: "Corner Shop".into(),
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
        owner_pin: Pin::parse(owner_pin).unwrap(),
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
    let client = CloudClient::new(url, "0.1.0").unwrap();
    let credentials = Arc::new(MemoryCredentials::default());
    let session = Arc::new(DeviceSession::new(client.clone(), credentials.clone()));
    let link = LinkService {
        account: Arc::new(client),
        credentials: credentials.clone(),
        store: Arc::new(store.clone()),
        local: Arc::new(store.clone()),
        transport: session.clone(),
        seed: session.clone(),
        app_version: "0.1.0".into(),
    };
    let worker = SyncWorker::new(
        Arc::new(store.clone()),
        session.clone(),
        clock.clone(),
        Arc::new(OsRandom),
    );
    Register {
        _dir: dir,
        store,
        clock,
        owner,
        tax_category: tax.parse().unwrap(),
        hasher,
        session,
        credentials,
        link,
        worker,
    }
}

impl Register {
    pub fn catalog(&self) -> CatalogService {
        CatalogService {
            store: Arc::new(self.store.clone()),
            catalog: Arc::new(self.store.clone()),
        }
    }

    pub async fn product(&self, name: &str, price: i64) -> VariantId {
        self.catalog()
            .create(
                &self.owner,
                ProductInput {
                    name: name.into(),
                    description: None,
                    category_id: None,
                    product_type: ProductType::Stock,
                    unit: Unit::Ea,
                    tax_category_id: self.tax_category,
                    quick_select: false,
                    variants: vec![VariantInput::simple(Money::from_minor(price))],
                },
            )
            .await
            .unwrap()
            .variants[0]
            .id
    }

    /// Goes live with an open drawer, as a trading register is.
    pub async fn go_live(&self) {
        sqlx::query("UPDATE device_identity SET mode = 'live'")
            .execute(self.store.db().writer())
            .await
            .unwrap();
        self.cash()
            .open(&self.owner, Money::from_minor(10_000))
            .await
            .unwrap();
    }

    pub fn cash(&self) -> CashService {
        let s = self.store.clone();
        CashService {
            store: Arc::new(s.clone()),
            cash: Arc::new(s.clone()),
            receipts: Arc::new(s.clone()),
            setup: Arc::new(s.clone()),
            pricing: Arc::new(s),
        }
    }

    /// One sale of `qty` of `variant`, paid in exact cash.
    pub async fn sell(&self, carts: &Arc<CartService>, variant: VariantId, qty: i64) -> CompletedSale {
        let (view, _) = carts
            .add_variant(
                &self.owner,
                variant,
                Some(Quantity::from_milli(qty * 1000)),
                AddVia::Search,
            )
            .await
            .unwrap();
        let due = view.priced.total;
        let cart = carts.current(&self.owner).await.unwrap().cart;
        let s = self.store.clone();
        CheckoutService {
            carts: carts.clone(),
            store: Arc::new(s.clone()),
            setup: Arc::new(s.clone()),
            stock: Arc::new(s.clone()),
            receipts: Arc::new(s),
        }
        .complete_sale(
            &self.owner,
            CompleteSale {
                cart_id: cart.id,
                revision: cart.revision,
                tenders: vec![TenderInput {
                    kind: TenderKind::Cash,
                    amount: due,
                    external_reference: None,
                }],
            },
        )
        .await
        .unwrap()
    }

    pub fn carts(&self) -> Arc<CartService> {
        let s = self.store.clone();
        Arc::new(CartService::new(
            Arc::new(s.clone()),
            Arc::new(s.clone()),
            Arc::new(s),
        ))
    }

    /// Pushes and pulls until there is nothing left to do now; returns the last `Next`.
    pub async fn sync(&self) -> Next {
        loop {
            let push = self.worker.push_once().await.unwrap().next;
            if push == Next::Immediately {
                continue;
            }
            let pull = self.worker.pull_once().await.unwrap().next;
            if pull == Next::Immediately {
                continue;
            }
            return match (push, pull) {
                (Next::Idle, Next::Idle) => Next::Idle,
                (Next::Idle, other) | (other, _) => other,
            };
        }
    }

    pub async fn local(&self, sql: &str) -> i64 {
        sqlx::query_scalar(sql)
            .fetch_one(self.store.db().readers())
            .await
            .unwrap()
    }

    /// Signs the owner in to the cloud.
    pub async fn sign_in(&self, email: &str, password: &Secret<String>) -> OwnerSession {
        self.link.start(&self.owner, email, password).await.unwrap().0
    }
}

impl Register {
    /// After a join: act as the joined business's owner on this register (their staff
    /// record came with the bootstrap).
    pub async fn become_joined_owner(&mut self) {
        async fn one(pool: &sqlx::SqlitePool, sql: &'static str) -> String {
            sqlx::query_scalar::<_, String>(sql)
                .fetch_one(pool)
                .await
                .unwrap()
        }
        let pool = self.store.db().readers().clone();
        let business = one(&pool, "SELECT business_id FROM device_identity").await;
        let register = one(&pool, "SELECT register_id FROM device_identity").await;
        let location = one(
            &pool,
            "SELECT r.location_id FROM registers r JOIN device_identity d ON d.register_id = r.register_id",
        )
        .await;
        let owner = one(
            &pool,
            "SELECT user_id FROM staff_users WHERE role = 'owner' LIMIT 1",
        )
        .await;
        self.owner.register = pos_application::context::RegisterContext {
            business_id: business.parse().unwrap(),
            location_id: location.parse().unwrap(),
            register_id: register.parse().unwrap(),
        };
        self.owner.actor = Some(Actor {
            user_id: owner.parse().unwrap(),
            role: Role::Owner,
            session_id: SessionId::generate(&UuidV7Generator),
        });
    }

    pub fn conflicts(&self) -> pos_application::conflict::ConflictService {
        pos_application::conflict::ConflictService {
            store: Arc::new(self.store.clone()),
            conflicts: Arc::new(self.store.clone()),
        }
    }
}
