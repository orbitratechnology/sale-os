//! Devices attached to this till (E20-T05, E20-T11, ADR-024): the receipt printer and its
//! cash drawer, serial scanners and payment terminals.
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
use crate::queries::QueryError;
use crate::records::{DeviceKind, DeviceRecord};
use crate::{LocalStore, WriteSet};
use pos_device_ports::{
    CashDrawerPort, DeviceError, DrawerPin, PrintResult, PrinterCapabilities, PrinterStatus,
    ReceiptOutputPort,
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
