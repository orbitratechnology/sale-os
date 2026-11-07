//! `AppServices`: the process-wide container of ports and services (E01-T04). Commands get
//! it as Tauri managed state; nothing else holds database handles, clocks or the session.

use crate::IpcError;
use pos_application::approval::Approvals;
use pos_application::auth::{Argon2PinHasher, AuthService, PinHasher};
use pos_application::cart::CartService;
use pos_application::cash::CashService;
use pos_application::catalog::CatalogService;
use pos_application::checkout::CheckoutService;
use pos_application::context::{Actor, ApprovalToken, CommandContext};
use pos_application::printing::{DEFAULT_PRINT_TIMEOUT, PrintService};
use pos_application::queries::SetupQueries;
use pos_application::receipts::ReceiptService;
use pos_application::returns::ReturnService;
use pos_application::sales::SalesService;
use pos_application::setup::SetupService;
use pos_application::staff::StaffService;
use pos_application::stock::StockService;
use pos_domain::{ApprovalId, Clock, ErrorCode, IdGenerator, SystemClock, UuidV7Generator};
use pos_local_db::{DbOptions, LocalDb, SqliteStore};
use pos_observability::LogGuard;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct AppServices {
    /// The local database, or why it could not be opened (shown by `app_info`).
    pub db: Result<LocalDb, IpcError>,
    /// The single write path (ADR-008). `None` when the database failed to open.
    pub store: Option<SqliteStore>,
    pub clock: Arc<dyn Clock>,
    pub ids: Arc<dyn IdGenerator>,
    /// The app data directory (database, logs, virtual-printer captures).
    pub data_dir: PathBuf,
    /// The use cases. `None` when the database failed to open.
    app: Option<Services>,
    /// The signed-in staff member. Lives only in memory: a restart signs everyone out.
    session: RwLock<Option<Session>>,
    _log: Option<LogGuard>,
}

pub(crate) struct Services {
    pub setup: SetupService,
    pub auth: AuthService,
    pub staff: StaffService,
    pub catalog: CatalogService,
    pub carts: Arc<CartService>,
    pub checkout: CheckoutService,
    pub receipts: ReceiptService,
    pub returns: ReturnService,
    pub sales: SalesService,
    pub cash: CashService,
    pub stock: StockService,
    /// Prints on the virtual printer (captures in `<app data>/virtual-printer/`) until the
    /// hardware adapters arrive in Stage 5.
    pub prints: PrintService,
    pub queries: SqliteStore,
    /// Issued manager approvals waiting to be used (E05-T04).
    pub approvals: Arc<Approvals>,
}

/// The unlocked session. Commands take the actor from here, never from IPC arguments
/// (docs/backlog/07 §1.1).
#[derive(Clone, Debug)]
pub(crate) struct Session {
    pub actor: Actor,
    pub display_name: String,
    pub must_change_pin: bool,
}

impl AppServices {
    /// Opens the database under `data_dir` and wires the production ports.
    pub async fn start(data_dir: &Path, log: Option<LogGuard>) -> Self {
        Self::start_with_hasher(data_dir, log, Arc::new(Argon2PinHasher::new())).await
    }

    /// As [`AppServices::start`] with a chosen PIN hasher. Tests use cheap Argon2 parameters;
    /// the app always uses the production cost (ADR-013).
    pub async fn start_with_hasher(
        data_dir: &Path,
        log: Option<LogGuard>,
        hasher: Arc<dyn PinHasher>,
    ) -> Self {
        let db = match LocalDb::open(&DbOptions::in_dir(data_dir)).await {
            Ok(db) => {
                if let Ok(Some(register)) = db.device_register_id().await {
                    pos_observability::set_context("register_id", register);
                }
                Ok(db)
            }
            Err(e) => {
                // A database failure must not crash the app silently: `app_info` reports it.
                tracing::error!(error = %e, "local database unavailable");
                Err(IpcError::from_error(&e))
            }
        };
        let store = db.as_ref().ok().map(|db| SqliteStore::new(db.clone()));
        if let Some(s) = &store {
            // E11-T06: check the stock projection without delaying start-up (2 s at most).
            let s = s.clone();
            tokio::spawn(async move {
                match s.startup_stock_check(std::time::Duration::from_secs(2)).await {
                    Ok(pos_local_db::StartupStockCheck::Healthy) => tracing::info!("stock balances verified"),
                    Ok(pos_local_db::StartupStockCheck::Drift(n)) => {
                        tracing::warn!(
                            balances = n,
                            "stock balances differ from their movements; rebuild from diagnostics"
                        )
                    }
                    Ok(pos_local_db::StartupStockCheck::IntegrityProblem(m)) => {
                        tracing::error!(problem = %m, "database quick_check failed; stock not verified")
                    }
                    Ok(pos_local_db::StartupStockCheck::Deferred) => {
                        tracing::info!("stock verify took longer than 2 s; run it from diagnostics")
                    }
                    Err(e) => tracing::warn!(error = %e, "stock verify failed"),
                }
            });
        }
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let ids: Arc<dyn IdGenerator> = Arc::new(UuidV7Generator);
        let app = store.as_ref().map(|s| {
            let a = Arc::new(s.clone());
            let approvals = Arc::new(Approvals::default());
            let carts = Arc::new(CartService::new(a.clone(), a.clone(), a.clone()));
            let printer = Arc::new(pos_device_virtual::printer::VirtualPrinter::new(
                data_dir.join("virtual-printer"),
            ));
            Services {
                setup: SetupService {
                    store: a.clone(),
                    setup: a.clone(),
                    hasher: hasher.clone(),
                    clock: clock.clone(),
                    ids: ids.clone(),
                },
                auth: AuthService {
                    store: a.clone(),
                    staff: a.clone(),
                    hasher: hasher.clone(),
                    approvals: approvals.clone(),
                },
                staff: StaffService {
                    store: a.clone(),
                    staff: a.clone(),
                    hasher,
                },
                catalog: CatalogService {
                    store: a.clone(),
                    catalog: a.clone(),
                },
                carts: carts.clone(),
                checkout: CheckoutService {
                    carts,
                    store: a.clone(),
                    setup: a.clone(),
                    stock: a.clone(),
                    receipts: a.clone(),
                },
                receipts: ReceiptService { receipts: a.clone() },
                sales: SalesService { sales: a.clone() },
                stock: StockService {
                    store: a.clone(),
                    carts: a.clone(),
                    stock: a.clone(),
                },
                cash: CashService {
                    store: a.clone(),
                    cash: a.clone(),
                    receipts: a.clone(),
                    setup: a.clone(),
                    pricing: a.clone(),
                },
                returns: ReturnService {
                    store: a.clone(),
                    returns: a.clone(),
                    setup: a.clone(),
                    receipts: a.clone(),
                    pricing: a.clone(),
                },
                prints: PrintService {
                    store: a.clone(),
                    prints: a.clone(),
                    receipts: a,
                    printer,
                    timeout: DEFAULT_PRINT_TIMEOUT,
                },
                queries: s.clone(),
                approvals,
            }
        });
        Self {
            db,
            store,
            clock,
            ids,
            data_dir: data_dir.to_path_buf(),
            app,
            session: RwLock::new(None),
            _log: log,
        }
    }

    /// The use cases, or the database error when it could not be opened.
    pub(crate) fn app(&self) -> Result<&Services, IpcError> {
        match (&self.app, &self.db) {
            (Some(app), _) => Ok(app),
            (None, Err(e)) => Err(e.clone()),
            (None, Ok(_)) => Err(IpcError::internal("services not started")),
        }
    }

    pub(crate) async fn session(&self) -> Option<Session> {
        self.session.read().await.clone()
    }

    pub(crate) async fn set_session(&self, session: Option<Session>) {
        *self.session.write().await = session;
    }

    /// The context for a command on this register, with the signed-in actor if there is one.
    /// Services refuse with `SESSION_LOCKED` when they need an actor and there is none.
    /// Before setup there is no register, and so nothing to act on.
    pub(crate) async fn context(&self) -> Result<CommandContext, IpcError> {
        let app = self.app()?;
        let register = app
            .queries
            .register_context()
            .await
            .map_err(|e| IpcError::from_error(&pos_application::context::AppError::from(e)))?
            .ok_or_else(|| {
                IpcError::coded(
                    ErrorCode::SessionLocked,
                    "setup has not been completed on this computer",
                )
            })?;
        Ok(CommandContext {
            register,
            actor: self.session().await.map(|s| s.actor),
            clock: self.clock.clone(),
            ids: self.ids.clone(),
            approval: None,
        })
    }

    /// As [`AppServices::context`], with the approval the caller presented. The token is
    /// taken from the registry now, so it can only ever be used once; hand it back with
    /// [`AppServices::settle`].
    pub(crate) async fn context_with_approval(
        &self,
        token: Option<ApprovalId>,
    ) -> Result<(CommandContext, Option<ApprovalToken>), IpcError> {
        let mut ctx = self.context().await?;
        let token = match token {
            Some(id) => Some(
                self.app()?
                    .approvals
                    .take(id, ctx.now())
                    .map_err(|e| IpcError::from_app(&e))?,
            ),
            None => None,
        };
        ctx.approval = token.clone();
        Ok((ctx, token))
    }

    /// After the command: a token whose command failed is put back for a retry (it still
    /// expires on time); a used one is gone for good.
    pub(crate) fn settle<T>(&self, token: Option<ApprovalToken>, result: &Result<T, IpcError>) {
        if let (Some(t), Err(_), Ok(app)) = (token, result, self.app()) {
            app.approvals.restore(t);
        }
    }
}
