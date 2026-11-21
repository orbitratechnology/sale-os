//! `AppServices`: the process-wide container of ports and services (E01-T04). Commands get
//! it as Tauri managed state; nothing else holds database handles, clocks or the session.

use crate::IpcError;
use pos_application::approval::Approvals;
use pos_application::audit_log::AuditService;
use pos_application::auth::{Argon2PinHasher, AuthService, PinHasher};
use pos_application::cart::CartService;
use pos_application::cash::CashService;
use pos_application::catalog::CatalogService;
use pos_application::checkout::CheckoutService;
use pos_application::config::ConfigService;
use pos_application::context::{Actor, ApprovalToken, CommandContext};
use pos_application::export::ExportService;
use pos_application::import::ImportService;
use pos_application::mode::ModeService;
use pos_application::printing::{DEFAULT_PRINT_TIMEOUT, PrintService};
use pos_application::queries::SetupQueries;
use pos_application::receipts::{ReceiptPdfService, ReceiptService};
use pos_application::reports::ReportService;
use pos_application::returns::ReturnService;
use pos_application::sales::SalesService;
use pos_application::settings::SettingsService;
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
    /// Asks where to save a file (the native dialog in the app; none in tests).
    pub(crate) file_dialog: Arc<dyn FileDialog>,
    /// The background push worker (E17-T03). `None` when the database failed to open.
    pub(crate) sync: Option<crate::sync_loop::SyncRunner>,
    /// The cloud this app links to (`POS_CLOUD_URL`), and the link wizard (E17-T08). `None`
    /// when no cloud is configured or the database failed to open.
    pub(crate) cloud: Option<Cloud>,
    /// Automatic backups (E19-T03). `None` when the database failed to open.
    pub(crate) backups: Option<Arc<crate::backups::Backups>>,
    /// Restarts the app (after a restore). Set by the app; does nothing in tests.
    pub(crate) restart: Arc<dyn Fn() + Send + Sync>,
    /// Where the database key is kept, and whether the data is encrypted (ADR-023).
    pub(crate) db_keys: Arc<dyn crate::db_key::DbKeyStore>,
    pub(crate) encryption: crate::db_key::Encryption,
    _log: Option<LogGuard>,
}

/// Asks the user which file to open or where to save one. Rust opens the dialog and reads
/// or writes the file, so the WebView never names a path (docs/backlog/07 §1).
pub trait FileDialog: Send + Sync {
    /// Where to save: the chosen path, or `None` when the user cancels (or there is no dialog).
    fn ask(
        &self,
        suggested_name: &str,
        filter: (&str, &[&str]),
    ) -> tokio::sync::oneshot::Receiver<Option<PathBuf>>;

    /// Which folder, or `None` when the user cancels (or there is no dialog).
    fn folder(&self) -> tokio::sync::oneshot::Receiver<Option<PathBuf>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let _ = tx.send(None);
        rx
    }

    /// Which file to open, or `None` when the user cancels (or there is no dialog).
    fn open(&self, filter: (&str, &[&str])) -> tokio::sync::oneshot::Receiver<Option<PathBuf>> {
        let _ = filter;
        let (tx, rx) = tokio::sync::oneshot::channel();
        let _ = tx.send(None);
        rx
    }
}

/// No dialog: every open and save is "cancelled". Used until the app wires the native one.
struct NoDialog;

impl FileDialog for NoDialog {
    fn ask(&self, _: &str, _: (&str, &[&str])) -> tokio::sync::oneshot::Receiver<Option<PathBuf>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let _ = tx.send(None);
        rx
    }
}

pub(crate) struct Services {
    pub setup: SetupService,
    pub auth: AuthService,
    pub staff: StaffService,
    pub mode: ModeService,
    pub settings: SettingsService,
    pub config: ConfigService,
    pub catalog: CatalogService,
    pub carts: Arc<CartService>,
    pub checkout: CheckoutService,
    pub receipts: ReceiptService,
    pub receipt_pdfs: ReceiptPdfService,
    pub returns: ReturnService,
    pub tax_invoices: pos_application::tax_invoice::TaxInvoiceService,
    pub sales: SalesService,
    pub cash: CashService,
    pub stock: StockService,
    pub reports: ReportService,
    pub exports: ExportService,
    pub imports: ImportService,
    pub audit: AuditService,
    /// Sync conflicts (E18).
    pub conflicts: pos_application::conflict::ConflictService,
    pub backup_audit: pos_application::backups::BackupAudit,
    /// Prints on the configured receipt printer (Settings → Devices), or on the virtual
    /// printer (captures in `<app data>/virtual-printer/`) when none is set up.
    pub prints: PrintService,
    /// This till's printers, drawer, scanners and terminal (E20-T11, ADR-024).
    pub devices: pos_application::devices::DeviceService,
    /// Serial scanners, forwarded to the window as `scanner://scan` (E21-T04).
    pub scanners: Arc<crate::device_commands::ScannerHub>,
    /// Status the receipt printer pushes, forwarded to the window as `printer://status`.
    pub printer_feed: Arc<crate::device_commands::PrinterStatusFeed>,
    /// The card terminal's payment flow, when one is set up (E22-T02).
    pub payments: RwLock<Option<Arc<pos_application::payments::ProviderPaymentFlow>>>,
    pub queries: SqliteStore,
    /// Issued manager approvals waiting to be used (E05-T04).
    pub approvals: Arc<Approvals>,
}

/// Linking (E17-T08): the wizard's service, the keychain, and the owner's cloud sign-in
/// between `sync_link_start` and `sync_link_complete` (memory only).
pub(crate) struct Cloud {
    pub link: pos_application::link::LinkService,
    pub reports: pos_application::cloud_reports::CloudReportService,
    pub owner: tokio::sync::Mutex<Option<pos_application::link::OwnerSession>>,
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
        Self::start_with(
            data_dir,
            log,
            Arc::new(Argon2PinHasher::new()),
            Arc::new(crate::db_key::KeychainDbKey::new(data_dir)),
        )
        .await
    }

    /// How the app restarts itself (after a restore).
    pub fn with_restart(mut self, restart: Arc<dyn Fn() + Send + Sync>) -> Self {
        self.restart = restart;
        self
    }

    /// Uses `dialog` to ask where files are saved (the app passes the native dialog).
    pub fn with_file_dialog(mut self, dialog: Arc<dyn FileDialog>) -> Self {
        self.file_dialog = dialog;
        self
    }

    /// As [`AppServices::start`] with a chosen PIN hasher. Tests use cheap Argon2 parameters;
    /// the app always uses the production cost (ADR-013).
    pub async fn start_with_hasher(
        data_dir: &Path,
        log: Option<LogGuard>,
        hasher: Arc<dyn PinHasher>,
    ) -> Self {
        Self::start_with(data_dir, log, hasher, Arc::new(crate::db_key::NoDbKey)).await
    }

    /// As [`AppServices::start`] with a chosen PIN hasher and key store (tests use one in
    /// memory, or none for a plain database).
    pub async fn start_with(
        data_dir: &Path,
        log: Option<LogGuard>,
        hasher: Arc<dyn PinHasher>,
        db_keys: Arc<dyn crate::db_key::DbKeyStore>,
    ) -> Self {
        let mut opts = DbOptions::in_dir(data_dir);
        let (key, encryption) = crate::db_key::key_for_start(db_keys.as_ref(), &opts.path).await;
        opts.key = key;
        let db = match LocalDb::open(&opts).await {
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
            // The scale (E20-T06): read by the cart service for weighed items, set up by devices.
            let scale = Arc::new(pos_application::scale::Scale::default());
            let carts = Arc::new(
                CartService::new(a.clone(), a.clone(), a.clone())
                    .with_stock(a.clone())
                    .with_scale(scale.clone()),
            );
            let virtual_printer = Arc::new(pos_device_virtual::printer::VirtualPrinter::new(
                data_dir.join("virtual-printer"),
            ));
            // The virtual printer stands in until a real one is set up; it has no drawer.
            let printer = Arc::new(pos_application::devices::SwitchablePrinter::new(
                pos_application::devices::PrinterHandle {
                    printer: virtual_printer,
                    drawer: None,
                    drawer_pin: pos_device_ports::DrawerPin::Pin2,
                },
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
                config: ConfigService {
                    store: a.clone(),
                    config: a.clone(),
                },
                settings: SettingsService {
                    store: a.clone(),
                    settings: a.clone(),
                },
                mode: ModeService {
                    store: a.clone(),
                    staff: a.clone(),
                    hasher: hasher.clone(),
                    setup: a.clone(),
                    carts: a.clone(),
                    cash: a.clone(),
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
                receipts: ReceiptService {
                    receipts: a.clone(),
                    printer: Some(printer.clone()),
                },
                receipt_pdfs: ReceiptPdfService {
                    receipts: a.clone(),
                    cash: a.clone(),
                    store: a.clone(),
                },
                sales: SalesService { sales: a.clone() },
                reports: ReportService { reports: a.clone() },
                audit: AuditService {
                    audit: a.clone(),
                    pricing: a.clone(),
                },
                imports: ImportService {
                    store: a.clone(),
                    imports: a.clone(),
                    catalog: a.clone(),
                    config: a.clone(),
                    setup: a.clone(),
                },
                exports: ExportService {
                    store: a.clone(),
                    exports: a.clone(),
                    reports: a.clone(),
                },
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
                tax_invoices: pos_application::tax_invoice::TaxInvoiceService {
                    store: a.clone(),
                    receipts: a.clone(),
                    sales: a.clone(),
                    pricing: a.clone(),
                    printer: Some(printer.clone()),
                },
                prints: PrintService {
                    store: a.clone(),
                    prints: a.clone(),
                    receipts: a.clone(),
                    printer: printer.clone(),
                    timeout: DEFAULT_PRINT_TIMEOUT,
                },
                devices: pos_application::devices::DeviceService {
                    store: a.clone(),
                    devices: a,
                    driver: Arc::new(crate::device_commands::HwDriver),
                    printer,
                    display: Arc::new(pos_application::customer_display::CustomerDisplay::default()),
                    scale,
                },
                scanners: Arc::new(crate::device_commands::ScannerHub::default()),
                printer_feed: Arc::new(crate::device_commands::PrinterStatusFeed::default()),
                payments: RwLock::new(None),
                conflicts: pos_application::conflict::ConflictService {
                    store: Arc::new(s.clone()),
                    conflicts: Arc::new(s.clone()),
                },
                backup_audit: pos_application::backups::BackupAudit {
                    store: Arc::new(s.clone()),
                },
                queries: s.clone(),
                approvals,
            }
        });
        // The cloud, when one is configured: one session (and keychain entry) for this app.
        let wiring = std::env::var("POS_CLOUD_URL")
            .ok()
            .filter(|u| !u.trim().is_empty())
            .and_then(
                |url| match pos_sync_http::CloudClient::new(&url, env!("CARGO_PKG_VERSION")) {
                    Ok(client) => Some(client),
                    Err(e) => {
                        tracing::error!(error = %e, "cloud client could not be created");
                        None
                    }
                },
            );
        let credentials: Arc<dyn pos_application::link::CredentialStore> =
            Arc::new(pos_sync_http::KeychainCredentials::new("refresh-token"));
        let (sync, cloud) = match (store.as_ref(), wiring) {
            (Some(s), Some(client)) => {
                let session = Arc::new(pos_sync_http::DeviceSession::new(
                    client.clone(),
                    credentials.clone(),
                ));
                let runner = crate::sync_loop::SyncRunner::start(
                    s.clone(),
                    session.clone(),
                    clock.clone(),
                    Some(credentials.clone()),
                );
                let reports = pos_application::cloud_reports::CloudReportService {
                    transport: session.clone(),
                    sync: Arc::new(s.clone()),
                    cache: Arc::new(s.clone()),
                    clock: clock.clone(),
                };
                let link = pos_application::link::LinkService {
                    account: Arc::new(client),
                    credentials,
                    store: Arc::new(s.clone()),
                    local: Arc::new(s.clone()),
                    transport: session.clone(),
                    seed: session,
                    app_version: env!("CARGO_PKG_VERSION").into(),
                };
                (
                    Some(runner),
                    Some(Cloud {
                        link,
                        reports,
                        owner: tokio::sync::Mutex::new(None),
                    }),
                )
            }
            (Some(s), None) => (
                Some(crate::sync_loop::SyncRunner::start(
                    s.clone(),
                    Arc::new(crate::sync_loop::NotLinked),
                    clock.clone(),
                    None,
                )),
                None,
            ),
            (None, _) => (None, None),
        };
        // E19-T03: hourly (when something changed), daily and on shutdown.
        let backups = db.as_ref().ok().map(|db| {
            let b = crate::backups::Backups::new(db.clone(), data_dir);
            b.start();
            b
        });
        let services = Self {
            db,
            store,
            clock,
            ids,
            data_dir: data_dir.to_path_buf(),
            sync,
            cloud,
            app,
            session: RwLock::new(None),
            file_dialog: Arc::new(NoDialog),
            backups,
            restart: Arc::new(|| tracing::warn!("restart requested; no app to restart")),
            db_keys,
            encryption,
            _log: log,
        };
        // Put the stored printer, scanners and terminal to work (never fatal).
        services.devices_changed().await;
        services
    }

    /// After start-up and after any device change: activates the receipt printer, restarts
    /// serial scanners and rebuilds the card terminal's payment flow from storage.
    pub(crate) async fn devices_changed(&self) {
        let Ok(app) = self.app() else { return };
        let Ok(Some(register)) = app.queries.register_context().await else {
            return;
        };
        if let Err(e) = app.devices.activate(register.register_id).await {
            tracing::warn!(problem = %e, "receipt printer not activated; using the virtual printer");
        }
        app.printer_feed.follow(app.devices.printer.clone());
        self.refresh_shop_name().await;
        if let Err(e) = app.devices.activate_display(register.register_id).await {
            tracing::warn!(problem = %e, "customer display not activated");
        }
        if let Err(e) = app.devices.activate_scale(register.register_id).await {
            tracing::warn!(problem = %e, "scale not activated");
        }
        let devices = match pos_application::devices::DeviceQueries::devices(
            &app.queries,
            register.register_id,
        )
        .await
        {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(error = %e.0, "devices could not be read");
                return;
            }
        };
        app.scanners.restart(&devices).await;
        let terminal = devices.iter().find(|d| {
            d.kind == pos_application::records::DeviceKind::PaymentTerminal && d.enabled && d.role == "main"
        });
        let port: Option<Arc<dyn pos_device_ports::PaymentPort>> = match terminal
            .and_then(|t| t.config.get("provider"))
            .and_then(|p| p.as_str())
        {
            Some("sandbox") => Some(Arc::new(crate::device_commands::SandboxTerminal(
                pos_device_virtual::payment::MockPaymentProvider::new(),
            ))),
            _ => None,
        };
        let a = Arc::new(app.queries.clone());
        *app.payments.write().await = port.map(|port| {
            Arc::new(pos_application::payments::ProviderPaymentFlow {
                store: a.clone(),
                payments: a.clone(),
                carts: a.clone(),
                setup: a.clone(),
                port,
                timeout: pos_application::payments::DEFAULT_PAYMENT_TIMEOUT,
            })
        });
    }

    /// The customer display's welcome line names the shop.
    pub(crate) async fn refresh_shop_name(&self) {
        let Ok(app) = self.app() else { return };
        if let Ok(Some(ws)) = app.queries.workspace_summary().await {
            app.devices.display.set_shop(&ws.business_name);
        }
    }

    /// Receivers for serial-scanner scans and health (forwarded to the window by the shell).
    pub(crate) fn scanner_events(
        &self,
    ) -> Option<(
        tokio::sync::broadcast::Receiver<crate::device_commands::SerialScanDto>,
        tokio::sync::broadcast::Receiver<crate::device_commands::ScannerHealthDto>,
    )> {
        let app = self.app().ok()?;
        Some((app.scanners.scans.subscribe(), app.scanners.health.subscribe()))
    }

    /// Receiver for the status the receipt printer pushes (forwarded to the window).
    pub(crate) fn printer_events(
        &self,
    ) -> Option<tokio::sync::broadcast::Receiver<crate::device_commands::PrinterStatusDto>> {
        Some(self.app().ok()?.printer_feed.events.subscribe())
    }

    /// The card terminal's payment flow, when one is set up.
    pub(crate) async fn payment_flow(&self) -> Option<Arc<pos_application::payments::ProviderPaymentFlow>> {
        self.app().ok()?.payments.read().await.clone()
    }

    /// Whether this till is in sandbox (practice) mode.
    pub(crate) async fn in_sandbox(&self) -> Result<bool, IpcError> {
        use pos_application::queries::SetupQueries;
        let identity = self
            .app()?
            .queries
            .device_identity()
            .await
            .map_err(|e| IpcError::from_error(&pos_application::context::AppError::from(e)))?;
        Ok(!matches!(
            identity.map(|d| d.mode),
            Some(pos_application::records::RegisterMode::Live)
        ))
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
