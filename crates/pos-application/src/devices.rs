//! Devices attached to this till (E20-T05, E20-T11, E20-T06, ADR-024): the receipt printer
//! and its cash drawer, scanners, the customer display, the scale and payment terminals.
//!
//! Configuration is **per till and local** (`devices`, never synced): an IP address or a
//! COM port belongs to one machine. What a configuration *means* (which transport, which
//! model profile) is the [`DeviceDriver`]'s business. The desktop shell implements it with
//! the real adapters, tests with simulators. This module owns the rules:
//!
//! - who may change devices (`DeviceConfigure`) and open the drawer without a sale
//!   (`DrawerOpen`; a cashier needs a manager's approval), and that both are audited;
//! - that a saved printer is used by the very next print, without a restart, and that a
//!   broken configuration never leaves the till without its fallback printer;
//! - that the drawer opens automatically after a sale only when cash changed hands.

use crate::audit::{self, AuditEntry};
use crate::context::{AppError, CommandContext};
use crate::customer_display::CustomerDisplay;
use crate::queries::QueryError;
use crate::records::{DeviceKind, DeviceRecord};
use crate::scale::Scale;
use crate::{LocalStore, WriteSet};
use pos_device_ports::{
    CashDrawerPort, CustomerDisplayPort, DeviceError, DeviceHealth, DrawerPin, PrintResult,
    PrinterCapabilities, PrinterStatus, ReceiptOutputPort, ScalePort, ScaleReading,
};
use pos_domain::{DeviceId, ErrorCode, Permission, RegisterId};
use serde_json::Value;
use std::sync::{Arc, RwLock};

#[async_trait::async_trait]
pub trait DeviceQueries: Send + Sync {
    /// The devices attached to `register`, in kind and role order.
    async fn devices(&self, register: RegisterId) -> Result<Vec<DeviceRecord>, QueryError>;
}

/// A configuration problem, named by the input at fault.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceConfigProblem {
    pub field: String,
    pub message: String,
}

impl From<DeviceConfigProblem> for AppError {
    fn from(p: DeviceConfigProblem) -> Self {
        AppError::DeviceConfig {
            field: p.field,
            message: p.message,
        }
    }
}

/// A working printer, with the drawer wired to its kick connector when there is one.
#[derive(Clone)]
pub struct PrinterHandle {
    pub printer: Arc<dyn ReceiptOutputPort>,
    pub drawer: Option<Arc<dyn CashDrawerPort>>,
    pub drawer_pin: DrawerPin,
}

impl std::fmt::Debug for PrinterHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrinterHandle")
            .field("adapter", &self.printer.adapter())
            .field("target", &self.printer.target())
            .field("drawer", &self.drawer.is_some())
            .finish()
    }
}

/// Turns stored configurations into devices (implemented by the shell with real adapters).
#[async_trait::async_trait]
pub trait DeviceDriver: Send + Sync {
    /// Checks and normalises a configuration for `kind`.
    fn validate(&self, kind: DeviceKind, config: &Value) -> Result<Value, DeviceConfigProblem>;
    /// Builds a receipt printer from a validated configuration. Must not block on the
    /// device itself: connecting happens per job.
    async fn open_printer(&self, config: &Value) -> Result<PrinterHandle, DeviceError>;
    /// Builds a customer display from a validated configuration, without waiting for it.
    async fn open_display(&self, _config: &Value) -> Result<Arc<dyn CustomerDisplayPort>, DeviceError> {
        Err(DeviceError::Unsupported(
            "customer displays are not supported".into(),
        ))
    }
    /// Builds a scale from a validated configuration, without reading it yet.
    async fn open_scale(&self, _config: &Value) -> Result<Arc<dyn ScalePort>, DeviceError> {
        Err(DeviceError::Unsupported("scales are not supported".into()))
    }
}

/// The printer the print queue uses. Swapped when the configuration changes; falls back to
/// the virtual printer (or none) when nothing usable is configured.
pub struct SwitchablePrinter {
    current: RwLock<PrinterHandle>,
    fallback: PrinterHandle,
}

impl SwitchablePrinter {
    pub fn new(fallback: PrinterHandle) -> Self {
        Self {
            current: RwLock::new(fallback.clone()),
            fallback,
        }
    }

    pub fn current(&self) -> PrinterHandle {
        self.current
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn set(&self, handle: PrinterHandle) {
        *self
            .current
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = handle;
    }

    pub fn use_fallback(&self) {
        self.set(self.fallback.clone());
    }

    /// Whether a real (configured) printer is active rather than the fallback.
    pub fn is_configured(&self) -> bool {
        !Arc::ptr_eq(&self.current().printer, &self.fallback.printer)
    }
}

#[async_trait::async_trait]
impl ReceiptOutputPort for SwitchablePrinter {
    fn adapter(&self) -> &'static str {
        self.current().printer.adapter()
    }
    fn target(&self) -> Option<String> {
        self.current().printer.target()
    }
    fn capabilities(&self) -> PrinterCapabilities {
        self.current().printer.capabilities()
    }
    async fn submit(&self, job_name: &str, bytes: &[u8]) -> PrintResult {
        // Clone the handle first: a settings change mid-job must not block on the lock.
        let printer = self.current().printer;
        printer.submit(job_name, bytes).await
    }
    async fn status(&self) -> Result<PrinterStatus, DeviceError> {
        let printer = self.current().printer;
        printer.status().await
    }
    fn pushes_status(&self) -> bool {
        self.current().printer.pushes_status()
    }
    fn status_updates(&self) -> Option<tokio::sync::watch::Receiver<pos_device_ports::PushedStatus>> {
        self.current().printer.status_updates()
    }
}

/// What the operator entered for one device.
#[derive(Clone, Debug, PartialEq)]
pub struct DeviceInput {
    /// `None` adds a device; `Some` changes it (with `base_version`).
    pub device_id: Option<DeviceId>,
    pub kind: DeviceKind,
    pub role: String,
    pub name: String,
    pub config: Value,
    pub enabled: bool,
    pub base_version: Option<u64>,
}

/// How a device is doing right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrinterView {
    /// False: the fallback (virtual) printer is in use.
    pub configured: bool,
    pub adapter: &'static str,
    pub target: Option<String>,
    pub status: Option<PrinterStatus>,
    /// Why the status could not be read, when it could not.
    pub problem: Option<String>,
    pub drawer: bool,
    /// The printer pushes its status (automatic status back): polling is only a slow
    /// check, and changes arrive by themselves.
    pub push: bool,
}

/// The outcome of opening the drawer after a sale. Never an error: the sale is done.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DrawerKick {
    NotNeeded,
    NoDrawer,
    Opened,
    Failed(String),
}

pub struct DeviceService {
    pub store: Arc<dyn LocalStore>,
    pub devices: Arc<dyn DeviceQueries>,
    pub driver: Arc<dyn DeviceDriver>,
    pub printer: Arc<SwitchablePrinter>,
    /// The customer display (role `main`), if one is set up.
    pub display: Arc<CustomerDisplay>,
    /// The scale (role `main`), if one is set up; shared with the cart service.
    pub scale: Arc<Scale>,
}

/// How the scale is doing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScaleView {
    pub configured: bool,
    /// Which scale, e.g. `COM3 @ 9600, Toledo 8217 (kg)`.
    pub target: Option<String>,
    pub health: DeviceHealth,
}

/// What "Test scale" read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScaleTest {
    Read(ScaleReading),
    Failed(String),
}

fn is_main_scale(d: &DeviceRecord) -> bool {
    d.kind == DeviceKind::Scale && d.role == "main"
}

fn reading_text(r: &ScaleReading) -> String {
    match r {
        ScaleReading::Stable(w) => format!("stable {}", w.grams_text()),
        ScaleReading::Moving(Some(w)) => format!("moving {}", w.grams_text()),
        ScaleReading::Moving(None) => "moving".into(),
        ScaleReading::Overload => "overload".into(),
        ScaleReading::Underload => "underload".into(),
    }
}

/// How the customer display is doing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisplayView {
    pub configured: bool,
    /// Which display, e.g. `/dev/ttyUSB0 @ 9600, ESC/POS`.
    pub target: Option<String>,
    pub health: DeviceHealth,
}

fn is_main_display(d: &DeviceRecord) -> bool {
    d.kind == DeviceKind::CustomerDisplay && d.role == "main"
}

fn device_error(e: &DeviceError) -> AppError {
    AppError::Device(e.to_string())
}

fn clean_name(name: &str, kind: DeviceKind) -> Result<String, AppError> {
    let name = name.trim();
    if name.chars().count() > 80 {
        return Err(AppError::DeviceConfig {
            field: "name".into(),
            message: "the name is longer than 80 characters".into(),
        });
    }
    Ok(if name.is_empty() {
        match kind {
            DeviceKind::ReceiptPrinter => "Receipt printer",
            DeviceKind::Scanner => "Scanner",
            DeviceKind::PaymentTerminal => "Payment terminal",
            DeviceKind::CustomerDisplay => "Customer display",
            DeviceKind::Scale => "Scale",
        }
        .to_owned()
    } else {
        name.to_owned()
    })
}

fn clean_role(role: &str) -> Result<String, AppError> {
    let role = role.trim();
    let role = if role.is_empty() { "main" } else { role };
    if role.len() > 32
        || !role
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(AppError::DeviceConfig {
            field: "role".into(),
            message: "use up to 32 letters, digits, - or _".into(),
        });
    }
    Ok(role.to_owned())
}

impl DeviceService {
    pub async fn list(&self, ctx: &CommandContext) -> Result<Vec<DeviceRecord>, AppError> {
        ctx.authorize(Permission::DeviceConfigure)?;
        Ok(self.devices.devices(ctx.register.register_id).await?)
    }

    /// Adds or changes a device. The configuration is validated by the driver first, and a
    /// receipt printer is built before anything is stored, so a configuration that cannot
    /// work is refused rather than saved.
    pub async fn save(&self, ctx: &CommandContext, input: DeviceInput) -> Result<DeviceRecord, AppError> {
        ctx.authorize(Permission::DeviceConfigure)?;
        let name = clean_name(&input.name, input.kind)?;
        let role = clean_role(&input.role)?;
        let config = self.driver.validate(input.kind, &input.config)?;
        let existing = self.devices.devices(ctx.register.register_id).await?;
        let now = ctx.now();
        let record = match input.device_id {
            None => {
                if existing.iter().any(|d| d.kind == input.kind && d.role == role) {
                    return Err(AppError::DeviceConfig {
                        field: "role".into(),
                        message: format!(
                            "this till already has a {} with role {role:?}",
                            input.kind.as_str()
                        ),
                    });
                }
                DeviceRecord {
                    device_id: DeviceId::generate(ctx.ids.as_ref()),
                    register_id: ctx.register.register_id,
                    kind: input.kind,
                    role,
                    name,
                    config,
                    enabled: input.enabled,
                    version: 1,
                    created_at: now,
                    updated_at: now,
                }
            }
            Some(id) => {
                let old = existing.iter().find(|d| d.device_id == id).ok_or_else(|| {
                    AppError::Validation("that device does not exist".into(), ErrorCode::NotFound)
                })?;
                if input.base_version != Some(old.version) {
                    return Err(AppError::Validation(
                        "someone changed this device meanwhile; reload".into(),
                        ErrorCode::ConflictVersion,
                    ));
                }
                if old.kind != input.kind {
                    return Err(AppError::DeviceConfig {
                        field: "kind".into(),
                        message: "a device cannot change kind; remove it and add a new one".into(),
                    });
                }
                if existing
                    .iter()
                    .any(|d| d.device_id != id && d.kind == input.kind && d.role == role)
                {
                    return Err(AppError::DeviceConfig {
                        field: "role".into(),
                        message: format!(
                            "this till already has a {} with role {role:?}",
                            input.kind.as_str()
                        ),
                    });
                }
                DeviceRecord {
                    role,
                    name,
                    config,
                    enabled: input.enabled,
                    version: old.version + 1,
                    updated_at: now,
                    ..old.clone()
                }
            }
        };
        let handle = if record.kind == DeviceKind::ReceiptPrinter && record.enabled && record.role == "main" {
            Some(
                self.driver
                    .open_printer(&record.config)
                    .await
                    .map_err(|e| device_error(&e))?,
            )
        } else {
            None
        };
        let display = if is_main_display(&record) && record.enabled {
            Some(
                self.driver
                    .open_display(&record.config)
                    .await
                    .map_err(|e| device_error(&e))?,
            )
        } else {
            None
        };
        let scale = if is_main_scale(&record) && record.enabled {
            Some(
                self.driver
                    .open_scale(&record.config)
                    .await
                    .map_err(|e| device_error(&e))?,
            )
        } else {
            None
        };
        let entry = AuditEntry::new(ctx.ids.as_ref(), audit::DEVICE_SAVED, "device", record.device_id)
            .with_metadata(serde_json::json!({
                "kind": record.kind.as_str(),
                "role": record.role,
                "name": record.name,
                "enabled": record.enabled,
                "version": record.version,
            }));
        let set = WriteSet::builder(ctx.write_context())
            .put_device(record.clone())
            .audit(entry)
            .build()?;
        self.store.apply(set).await?;
        if record.kind == DeviceKind::ReceiptPrinter && record.role == "main" {
            match handle {
                Some(h) => self.printer.set(h),
                None => self.printer.use_fallback(),
            }
        }
        if is_main_display(&record) {
            let config = display.is_some().then(|| record.config.clone());
            self.display.set_built_from(display, config);
        }
        if is_main_scale(&record) {
            let config = scale.is_some().then(|| record.config.clone());
            self.scale.set_built_from(scale, config);
        }
        Ok(record)
    }

    pub async fn remove(&self, ctx: &CommandContext, id: DeviceId) -> Result<(), AppError> {
        ctx.authorize(Permission::DeviceConfigure)?;
        let existing = self.devices.devices(ctx.register.register_id).await?;
        let old = existing
            .iter()
            .find(|d| d.device_id == id)
            .ok_or_else(|| AppError::Validation("that device does not exist".into(), ErrorCode::NotFound))?;
        let entry = AuditEntry::new(ctx.ids.as_ref(), audit::DEVICE_REMOVED, "device", id).with_metadata(
            serde_json::json!({ "kind": old.kind.as_str(), "role": old.role, "name": old.name }),
        );
        let set = WriteSet::builder(ctx.write_context())
            .remove_device(id)
            .audit(entry)
            .build()?;
        self.store.apply(set).await?;
        if old.kind == DeviceKind::ReceiptPrinter && old.role == "main" {
            self.printer.use_fallback();
        }
        if is_main_display(old) {
            self.display.set(None);
        }
        if is_main_scale(old) {
            self.scale.set(None);
        }
        Ok(())
    }

    /// At start-up: puts the stored main receipt printer to work. A configuration that no
    /// longer builds (say a COM port that vanished from the driver's list) leaves the
    /// fallback in place and is reported, never fatal.
    pub async fn activate(&self, register: RegisterId) -> Result<(), String> {
        let devices = self.devices.devices(register).await.map_err(|e| e.0)?;
        let Some(printer) = devices
            .iter()
            .find(|d| d.kind == DeviceKind::ReceiptPrinter && d.role == "main" && d.enabled)
        else {
            self.printer.use_fallback();
            return Ok(());
        };
        match self.driver.open_printer(&printer.config).await {
            Ok(h) => {
                self.printer.set(h);
                Ok(())
            }
            Err(e) => {
                self.printer.use_fallback();
                Err(format!("receipt printer {:?}: {e}", printer.name))
            }
        }
    }

    /// At start-up and after device changes: puts the stored customer display to work, or
    /// none. A display that cannot be built is reported, never fatal.
    pub async fn activate_display(&self, register: RegisterId) -> Result<(), String> {
        let devices = self.devices.devices(register).await.map_err(|e| e.0)?;
        let Some(stored) = devices.iter().find(|d| is_main_display(d) && d.enabled) else {
            if self.display.port().is_some() {
                self.display.set(None);
            }
            return Ok(());
        };
        // Just saved (or unchanged): keep the display that is running. A second one on the
        // same port would find it busy.
        if self.display.port().is_some() && self.display.built_from().as_ref() == Some(&stored.config) {
            return Ok(());
        }
        match self.driver.open_display(&stored.config).await {
            Ok(port) => {
                self.display
                    .set_built_from(Some(port), Some(stored.config.clone()));
                Ok(())
            }
            Err(e) => {
                self.display.set(None);
                Err(format!("customer display {:?}: {e}", stored.name))
            }
        }
    }

    /// At start-up and after device changes: puts the stored scale to work, or none. A scale
    /// that cannot be built is reported, never fatal.
    pub async fn activate_scale(&self, register: RegisterId) -> Result<(), String> {
        let devices = self.devices.devices(register).await.map_err(|e| e.0)?;
        let Some(stored) = devices.iter().find(|d| is_main_scale(d) && d.enabled) else {
            self.scale.set(None);
            return Ok(());
        };
        if self.scale.port().is_some() && self.scale.built_from().as_ref() == Some(&stored.config) {
            return Ok(());
        }
        match self.driver.open_scale(&stored.config).await {
            Ok(port) => {
                self.scale.set_built_from(Some(port), Some(stored.config.clone()));
                Ok(())
            }
            Err(e) => {
                self.scale.set(None);
                Err(format!("scale {:?}: {e}", stored.name))
            }
        }
    }

    /// Any signed-in user may see how the scale is doing.
    pub fn scale_view(&self, ctx: &CommandContext) -> Result<ScaleView, AppError> {
        ctx.actor()?;
        let port = self.scale.port();
        Ok(ScaleView {
            configured: port.is_some(),
            target: port.as_ref().map(|p| p.describe()),
            health: port.map_or(DeviceHealth::Unknown, |p| p.health().borrow().clone()),
        })
    }

    /// "Test scale": one reading, whatever it is (audited). A scale that cannot be read is
    /// an answer here, not an error.
    pub async fn test_scale(&self, ctx: &CommandContext) -> Result<ScaleTest, AppError> {
        ctx.authorize(Permission::DeviceConfigure)?;
        let port = self
            .scale
            .port()
            .ok_or_else(|| AppError::Device("no scale is set up on this till".into()))?;
        let result = match port.read().await {
            Ok(r) => ScaleTest::Read(r),
            Err(e) => ScaleTest::Failed(e.to_string()),
        };
        let entry = AuditEntry::new(
            ctx.ids.as_ref(),
            audit::DEVICE_TEST_WEIGHED,
            "register",
            ctx.register.register_id,
        )
        .with_metadata(serde_json::json!({
            "target": port.describe(),
            "outcome": match &result {
                ScaleTest::Read(r) => reading_text(r),
                ScaleTest::Failed(e) => format!("failed: {e}"),
            },
        }));
        let set = WriteSet::builder(ctx.write_context()).audit(entry).build()?;
        self.store.apply(set).await?;
        Ok(result)
    }

    /// Any signed-in user may see how the customer display is doing (status bar).
    pub fn display_view(&self, ctx: &CommandContext) -> Result<DisplayView, AppError> {
        ctx.actor()?;
        let port = self.display.port();
        Ok(DisplayView {
            configured: port.is_some(),
            target: port.as_ref().map(|p| p.describe()),
            health: port.map_or(DeviceHealth::Unknown, |p| p.health().borrow().clone()),
        })
    }

    /// Shows the test pattern on the customer display (audited) and says whether it got
    /// there. A display that is not working is an answer here, not an error.
    pub async fn test_display(&self, ctx: &CommandContext) -> Result<Result<String, String>, AppError> {
        ctx.authorize(Permission::DeviceConfigure)?;
        if self.display.port().is_none() {
            return Err(AppError::Device(
                "no customer display is set up on this till".into(),
            ));
        }
        let result = self.display.test().await.map_err(|e| e.to_string());
        let entry = AuditEntry::new(
            ctx.ids.as_ref(),
            audit::DEVICE_TEST_DISPLAYED,
            "register",
            ctx.register.register_id,
        )
        .with_metadata(serde_json::json!({
            "target": self.display.port().map(|p| p.describe()),
            "outcome": match &result {
                Ok(_) => "shown".to_owned(),
                Err(e) => format!("failed: {e}"),
            },
        }));
        let set = WriteSet::builder(ctx.write_context()).audit(entry).build()?;
        self.store.apply(set).await?;
        Ok(result)
    }

    /// Any signed-in user may see whether the printer is ready (status bar).
    pub async fn printer_view(&self, ctx: &CommandContext) -> Result<PrinterView, AppError> {
        ctx.actor()?;
        let handle = self.printer.current();
        let (status, problem) = match handle.printer.status().await {
            Ok(s) => (Some(s), None),
            Err(e) => (None, Some(e.to_string())),
        };
        Ok(PrinterView {
            configured: self.printer.is_configured(),
            adapter: handle.printer.adapter(),
            target: handle.printer.target(),
            status,
            problem,
            drawer: handle.drawer.is_some(),
            push: handle.printer.pushes_status(),
        })
    }

    /// Prints a test page on the current printer (audited). `bytes` is the page already
    /// encoded for the printer's capabilities by the caller.
    pub async fn test_print(&self, ctx: &CommandContext, bytes: &[u8]) -> Result<PrintResult, AppError> {
        ctx.authorize(Permission::DeviceConfigure)?;
        let printer = self.printer.current().printer;
        let result = printer.submit("test-print", bytes).await;
        let outcome = match &result {
            PrintResult::Printed => "printed".to_owned(),
            PrintResult::Failed(f) => format!("failed: {}", f.code()),
            PrintResult::Uncertain(r) => format!("uncertain: {r}"),
        };
        let entry = AuditEntry::new(
            ctx.ids.as_ref(),
            audit::DEVICE_TEST_PRINTED,
            "register",
            ctx.register.register_id,
        )
        .with_metadata(serde_json::json!({
            "adapter": printer.adapter(),
            "target": printer.target(),
            "outcome": outcome,
        }));
        let set = WriteSet::builder(ctx.write_context()).audit(entry).build()?;
        self.store.apply(set).await?;
        Ok(result)
    }

    /// "No sale": opens the drawer on request, with a reason. A cashier needs a manager's
    /// approval (scope: this register). Audited as `cash_drawer.opened` with who approved.
    pub async fn open_drawer(&self, ctx: &CommandContext, reason: &str) -> Result<(), AppError> {
        let approver = ctx.authorize_with_approval(Permission::DrawerOpen, ctx.register.register_id, None)?;
        let reason = reason.trim();
        if reason.is_empty() || reason.chars().count() > 200 {
            return Err(AppError::DeviceConfig {
                field: "reason".into(),
                message: "say why the drawer is opened (up to 200 characters)".into(),
            });
        }
        let handle = self.printer.current();
        let drawer = handle
            .drawer
            .ok_or_else(|| AppError::Device("no cash drawer is set up on this till".into()))?;
        drawer
            .open(handle.drawer_pin)
            .await
            .map_err(|e| device_error(&e))?;
        let mut entry = AuditEntry::new(
            ctx.ids.as_ref(),
            audit::CASH_DRAWER_OPENED,
            "register",
            ctx.register.register_id,
        )
        .with_metadata(serde_json::json!({ "reason": reason, "trigger": "no_sale" }));
        if let Some(a) = approver {
            entry = entry.with_approver(a);
        }
        let set = WriteSet::builder(ctx.write_context()).audit(entry).build()?;
        self.store.apply(set).await?;
        Ok(())
    }

    /// After a completed sale: opens the drawer when cash was taken (or given as change)
    /// and `drawer.open_on_cash_sale` is on. Never for card- or transfer-only sales.
    pub async fn after_sale(
        &self,
        ctx: &CommandContext,
        sale: pos_domain::SaleId,
        took_cash: bool,
        open_on_cash_sale: bool,
    ) -> DrawerKick {
        if !took_cash || !open_on_cash_sale {
            return DrawerKick::NotNeeded;
        }
        let handle = self.printer.current();
        let Some(drawer) = handle.drawer else {
            return DrawerKick::NoDrawer;
        };
        if let Err(e) = drawer.open(handle.drawer_pin).await {
            return DrawerKick::Failed(e.to_string());
        }
        let entry = AuditEntry::new(ctx.ids.as_ref(), audit::CASH_DRAWER_OPENED, "sale", sale)
            .with_metadata(serde_json::json!({ "trigger": "cash_sale" }));
        match WriteSet::builder(ctx.write_context()).audit(entry).build() {
            Ok(set) => match self.store.apply(set).await {
                Ok(_) => DrawerKick::Opened,
                Err(e) => DrawerKick::Failed(format!("opened, but the audit entry failed: {e}")),
            },
            Err(e) => DrawerKick::Failed(format!("opened, but the audit entry failed: {e}")),
        }
    }
}
