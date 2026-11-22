//! Devices attached to this till (E20-T11, E21-T04, E20-T06, E22-T02, ADR-024): the hardware
//! driver behind `DeviceService`, serial and HID-POS scanners forwarded to the UI, the
//! customer display, the scale, the sandbox payment terminal, and the IPC commands of
//! Settings → Devices.

use crate::app_commands::err;
use crate::{AppServices, IpcError};
use pos_application::devices::{DeviceConfigProblem, DeviceDriver, DeviceInput, PrinterHandle, PrinterView};
use pos_application::records::{DeviceKind, DeviceRecord};
use pos_device_hw::{
    DisplayConfig, HidBackend, HidPosScanner, HwCustomerDisplay, HwScale, PrinterConfig, ScaleConfig,
    ScannerSettings, SerialScanner,
};
use pos_device_ports::{BarcodeInputPort, DeviceError, DeviceHealth, PaperLevel, PrintResult, PushedStatus};
use pos_domain::{DeviceId, ErrorCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use specta::Type;
use std::sync::Arc;
use std::time::Duration;
use tauri::State;

/// The payment providers this build can drive. Real connectors join as their agreements
/// exist (E22-T03); the sandbox terminal works only in sandbox mode.
pub const PAYMENT_PROVIDERS: &[&str] = &["sandbox"];

/// Builds real devices from stored configuration (`pos-device-hw`).
pub struct HwDriver;

fn first_problem(errors: Vec<pos_device_hw::FieldError>) -> DeviceConfigProblem {
    errors
        .into_iter()
        .next()
        .map(|e| DeviceConfigProblem {
            field: e.field,
            message: e.message,
        })
        .unwrap_or_else(|| DeviceConfigProblem {
            field: "config".into(),
            message: "invalid settings".into(),
        })
}

fn parse<T: serde::de::DeserializeOwned>(config: &Value) -> Result<T, DeviceConfigProblem> {
    serde_json::from_value(config.clone()).map_err(|e| DeviceConfigProblem {
        field: "config".into(),
        message: format!("settings not understood: {e}"),
    })
}

#[async_trait::async_trait]
impl DeviceDriver for HwDriver {
    fn validate(&self, kind: DeviceKind, config: &Value) -> Result<Value, DeviceConfigProblem> {
        match kind {
            DeviceKind::ReceiptPrinter => {
                let c: PrinterConfig = parse(config)?;
                c.validate().map_err(first_problem)?;
                Ok(serde_json::to_value(c).unwrap_or_default())
            }
            DeviceKind::Scanner => {
                let c = ScannerSettings::from_json(config).map_err(|e| DeviceConfigProblem {
                    field: "config".into(),
                    message: format!("settings not understood: {e}"),
                })?;
                c.validate().map_err(first_problem)?;
                Ok(c.to_json())
            }
            DeviceKind::PaymentTerminal => {
                let provider = config.get("provider").and_then(Value::as_str).unwrap_or_default();
                if PAYMENT_PROVIDERS.contains(&provider) {
                    Ok(serde_json::json!({ "provider": provider }))
                } else {
                    Err(DeviceConfigProblem {
                        field: "provider".into(),
                        message: format!(
                            "no connector for {provider:?} in this version; available: {}",
                            PAYMENT_PROVIDERS.join(", ")
                        ),
                    })
                }
            }
            DeviceKind::CustomerDisplay => {
                let c: DisplayConfig = parse(config)?;
                c.validate().map_err(first_problem)?;
                Ok(serde_json::to_value(c).unwrap_or_default())
            }
            DeviceKind::Scale => {
                let c: ScaleConfig = parse(config)?;
                c.validate().map_err(first_problem)?;
                Ok(serde_json::to_value(c).unwrap_or_default())
            }
        }
    }

    async fn open_scale(&self, config: &Value) -> Result<Arc<dyn pos_device_ports::ScalePort>, DeviceError> {
        let c: ScaleConfig = serde_json::from_value(config.clone())
            .map_err(|e| DeviceError::Unsupported(format!("stored scale settings: {e}")))?;
        Ok(Arc::new(HwScale::from_config(&c)?))
    }

    async fn open_display(
        &self,
        config: &Value,
    ) -> Result<Arc<dyn pos_device_ports::CustomerDisplayPort>, DeviceError> {
        let c: DisplayConfig = serde_json::from_value(config.clone())
            .map_err(|e| DeviceError::Unsupported(format!("stored display settings: {e}")))?;
        Ok(Arc::new(HwCustomerDisplay::from_config(&c)?))
    }

    async fn open_printer(&self, config: &Value) -> Result<PrinterHandle, DeviceError> {
        let c: PrinterConfig = serde_json::from_value(config.clone())
            .map_err(|e| DeviceError::Unsupported(format!("stored printer settings: {e}")))?;
        let pin = c.drawer_pin.into();
        let printer = pos_device_hw::build_printer(&c)?;
        // Printers that push their status (ESC/POS automatic status back) do so from now on;
        // the others are polled as before.
        printer.start_status_watch();
        Ok(PrinterHandle {
            printer: printer.clone(),
            drawer: Some(printer),
            drawer_pin: pin,
        })
    }
}

// ---------- serial and HID-POS scanners → the UI ----------

/// One scan from a serial or HID-POS scanner, for the UI to submit like a wedge scan.
#[derive(Clone, Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SerialScanDto {
    pub scan_event_id: pos_domain::ScanEventId,
    pub raw: String,
    /// The serial port, or the HID-POS scanner's name.
    pub port: String,
}

/// A scanner's health, for the status bar and Settings → Devices.
#[derive(Clone, Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ScannerHealthDto {
    pub name: String,
    /// `ready`, `warning`, `offline` or `unknown`.
    pub state: String,
    pub message: Option<String>,
}

/// A configured scanner and how it is doing (`scanner_status`).
#[derive(Clone, Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ScannerStatusDto {
    pub device_id: DeviceId,
    pub name: String,
    /// `serial` or `hidPos`.
    pub connection: String,
    /// The port, or the HID-POS scanner's name and IDs.
    pub target: String,
    /// `ready`, `warning`, `offline` or `unknown`.
    pub state: String,
    pub message: Option<String>,
}

type RunningScanner = (Arc<dyn BarcodeInputPort>, tokio::task::JoinHandle<()>);

/// Runs the configured serial and HID-POS scanners. Scans and health go out on broadcast
/// channels the shell forwards to the window as `scanner://scan` and `scanner://status`.
pub struct ScannerHub {
    running: tokio::sync::Mutex<Vec<RunningScanner>>,
    /// The latest health of each running scanner, by device.
    status: Arc<std::sync::Mutex<Vec<ScannerStatusDto>>>,
    /// How HID-POS scanners are reached (`hidapi`; tests use an emulated scanner).
    hid: Arc<dyn HidBackend>,
    pub scans: tokio::sync::broadcast::Sender<SerialScanDto>,
    pub health: tokio::sync::broadcast::Sender<ScannerHealthDto>,
}

impl Default for ScannerHub {
    fn default() -> Self {
        Self::with_hid_backend(Arc::new(pos_device_hw::HidApiBackend))
    }
}

fn health_parts(h: &DeviceHealth) -> (&'static str, Option<String>) {
    match h {
        DeviceHealth::Ready => ("ready", None),
        DeviceHealth::Warning(m) => ("warning", Some(m.clone())),
        DeviceHealth::Offline(m) => ("offline", Some(m.clone())),
        DeviceHealth::Unknown => ("unknown", None),
    }
}

fn health_dto(name: &str, h: &DeviceHealth) -> ScannerHealthDto {
    let (state, message) = health_parts(h);
    ScannerHealthDto {
        name: name.to_owned(),
        state: state.into(),
        message,
    }
}

impl ScannerHub {
    pub fn with_hid_backend(hid: Arc<dyn HidBackend>) -> Self {
        Self {
            running: tokio::sync::Mutex::new(Vec::new()),
            status: Arc::new(std::sync::Mutex::new(Vec::new())),
            hid,
            scans: tokio::sync::broadcast::channel(64).0,
            health: tokio::sync::broadcast::channel(16).0,
        }
    }

    /// The HID-POS scanners attached now (Settings → Devices list).
    pub async fn hid_devices(&self) -> Result<Vec<pos_device_hw::HidPosDevice>, String> {
        let hid = self.hid.clone();
        tokio::task::spawn_blocking(move || hid.list())
            .await
            .map_err(|e| e.to_string())?
    }

    /// The configured scanners and their latest health.
    pub fn status(&self) -> Vec<ScannerStatusDto> {
        self.status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Stops the running scanners and starts the enabled ones in `devices`.
    pub async fn restart(&self, devices: &[DeviceRecord]) {
        let mut running = self.running.lock().await;
        for (scanner, task) in running.drain(..) {
            let _ = scanner.stop().await;
            task.abort();
        }
        self.status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        for d in devices
            .iter()
            .filter(|d| d.kind == DeviceKind::Scanner && d.enabled)
        {
            let settings = match ScannerSettings::from_json(&d.config) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(device = %d.name, error = %e, "scanner settings not understood");
                    continue;
                }
            };
            let target = settings.describe();
            let (scanner, connection): (Arc<dyn BarcodeInputPort>, &str) = match settings {
                ScannerSettings::Serial(c) => (Arc::new(SerialScanner::new(c)), "serial"),
                ScannerSettings::HidPos(c) => (Arc::new(HidPosScanner::new(c, self.hid.clone())), "hidPos"),
            };
            self.status
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(ScannerStatusDto {
                    device_id: d.device_id,
                    name: d.name.clone(),
                    connection: connection.into(),
                    target: target.clone(),
                    state: "unknown".into(),
                    message: None,
                });
            if let Err(e) = scanner.start().await {
                tracing::warn!(device = %d.name, error = %e, "scanner did not start");
            }
            let mut scans = scanner.subscribe();
            let mut health = scanner.health();
            let (tx, htx, name, id) = (
                self.scans.clone(),
                self.health.clone(),
                d.name.clone(),
                d.device_id,
            );
            let table = self.status.clone();
            let task = tokio::spawn(async move {
                let record = |h: &DeviceHealth| {
                    let (state, message) = health_parts(h);
                    let mut t = table.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    if let Some(row) = t.iter_mut().find(|r| r.device_id == id) {
                        row.state = state.into();
                        row.message = message;
                    }
                };
                record(&health.borrow_and_update().clone());
                loop {
                    tokio::select! {
                        s = scans.recv() => match s {
                            Ok(e) => {
                                let _ = tx.send(SerialScanDto { scan_event_id: e.id, raw: e.raw, port: target.clone() });
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                tracing::warn!(lost = n, "scanner events lagged");
                            }
                            Err(_) => break,
                        },
                        changed = health.changed() => {
                            if changed.is_err() { break; }
                            let h = health.borrow_and_update().clone();
                            record(&h);
                            let _ = htx.send(health_dto(&name, &h));
                        }
                    }
                }
            });
            running.push((scanner, task));
        }
    }
}

// ---------- sandbox payment terminal ----------

/// The sandbox terminal (E22-T02): the mock provider, driven by the amount's last two
/// digits so every outcome can be practised at the till, like a card network's test cards.
/// `…01` declines, `…02` times out (reconciliation), `…03` waits a few polls, else approves.
pub struct SandboxTerminal(pub pos_device_virtual::payment::MockPaymentProvider);

#[async_trait::async_trait]
impl pos_device_ports::PaymentPort for SandboxTerminal {
    fn provider(&self) -> &'static str {
        "sandbox"
    }
    async fn initiate(
        &self,
        request: &pos_device_ports::PaymentRequest,
    ) -> Result<pos_device_ports::PaymentStatus, DeviceError> {
        use pos_device_virtual::payment::PaymentScenario;
        let scenario = match request.amount.minor().rem_euclid(100) {
            1 => PaymentScenario::Decline("Declined by the sandbox terminal (amount ends in .01)".into()),
            2 => PaymentScenario::Timeout,
            3 => PaymentScenario::Delayed(3),
            _ => PaymentScenario::Approve,
        };
        self.0.script([scenario]);
        self.0.initiate(request).await
    }
    async fn query(
        &self,
        id: pos_domain::PaymentAttemptId,
    ) -> Result<pos_device_ports::PaymentStatus, DeviceError> {
        self.0.query(id).await
    }
    async fn cancel(
        &self,
        id: pos_domain::PaymentAttemptId,
    ) -> Result<pos_device_ports::PaymentStatus, DeviceError> {
        self.0.cancel(id).await
    }
    async fn reverse(
        &self,
        id: pos_domain::PaymentAttemptId,
    ) -> Result<pos_device_ports::PaymentStatus, DeviceError> {
        self.0.reverse(id).await
    }
}

// ---------- DTOs ----------

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DeviceDto {
    pub device_id: DeviceId,
    /// `receipt_printer`, `scanner`, `payment_terminal` or `customer_display`.
    pub kind: String,
    pub role: String,
    pub name: String,
    /// The adapter's settings, as JSON text (see `PrinterConfig`, `ScannerConfig`).
    pub config_json: String,
    pub enabled: bool,
    pub version: u32,
}

impl DeviceDto {
    fn from_record(d: &DeviceRecord) -> Self {
        Self {
            device_id: d.device_id,
            kind: d.kind.as_str().into(),
            role: d.role.clone(),
            name: d.name.clone(),
            config_json: d.config.to_string(),
            enabled: d.enabled,
            version: u32::try_from(d.version).unwrap_or(u32::MAX),
        }
    }
}

#[derive(Debug, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInputDto {
    pub device_id: Option<DeviceId>,
    pub kind: String,
    pub role: Option<String>,
    pub name: String,
    pub config_json: String,
    pub enabled: bool,
    pub base_version: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PrinterStatusDto {
    /// False: no printer is set up; receipts go to the virtual printer (files) or PDF.
    pub configured: bool,
    pub adapter: String,
    pub target: Option<String>,
    /// `ready`, `warning`, `offline` or `unknown` (the link cannot report status).
    pub state: String,
    /// What to tell the cashier, e.g. "Paper is running out".
    pub message: Option<String>,
    pub drawer: bool,
    pub drawer_open: Option<bool>,
    /// The printer pushes its status: changes arrive as `printer://status` events, and
    /// polling is only a slow check.
    pub push: bool,
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TestPrintDto {
    /// `printed`, `failed` or `uncertain`.
    pub outcome: String,
    pub message: String,
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PrinterProfileDto {
    pub id: String,
    pub vendor: String,
    pub name: String,
    /// `escPos`, `starLine` or `plainText`.
    pub dialect: String,
    pub columns: u16,
    pub columns_font_b: Option<u16>,
    pub paper_width_mm: Option<u16>,
    pub cutter: bool,
    pub drawer: bool,
    pub barcode: bool,
    pub qr: bool,
    pub status_query: bool,
    pub text_supported: bool,
    /// `false`: added by hand, not yet checked on a real printer.
    pub verified: bool,
    pub notes: Option<String>,
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryDto {
    pub serial_ports: Vec<pos_device_hw::discovery::SerialCandidate>,
    pub device_files: Vec<String>,
    pub queues: Vec<String>,
    pub network: Vec<pos_device_hw::discovery::NetworkResponder>,
    /// Why the network scan did not run, when it did not.
    pub network_problem: Option<String>,
}

fn kind_of(s: &str) -> Result<DeviceKind, IpcError> {
    DeviceKind::parse(s)
        .ok_or_else(|| IpcError::coded(ErrorCode::ValidationFailed, format!("unknown device kind {s:?}")))
}

// ---------- commands ----------

/// Devices attached to this till (DeviceConfigure).
#[tauri::command]
#[specta::specta]
pub async fn devices_list(services: State<'_, AppServices>) -> Result<Vec<DeviceDto>, IpcError> {
    let ctx = services.context().await?;
    let list = services.app()?.devices.list(&ctx).await.map_err(err)?;
    Ok(list.iter().map(DeviceDto::from_record).collect())
}

/// Adds or changes a device; a printer is used from the next print, a scanner restarts.
#[tauri::command]
#[specta::specta]
pub async fn device_save(
    services: State<'_, AppServices>,
    input: DeviceInputDto,
) -> Result<DeviceDto, IpcError> {
    let ctx = services.context().await?;
    let config: Value = serde_json::from_str(&input.config_json).map_err(|_| {
        IpcError::coded(
            ErrorCode::ValidationFailed,
            "the device settings are not valid JSON",
        )
    })?;
    let app = services.app()?;
    services.refresh_shop_name().await;
    let saved = app
        .devices
        .save(
            &ctx,
            DeviceInput {
                device_id: input.device_id,
                kind: kind_of(&input.kind)?,
                role: input.role.unwrap_or_default(),
                name: input.name,
                config,
                enabled: input.enabled,
                base_version: input.base_version.map(u64::from),
            },
        )
        .await
        .map_err(err)?;
    services.devices_changed().await;
    Ok(DeviceDto::from_record(&saved))
}

#[tauri::command]
#[specta::specta]
pub async fn device_remove(services: State<'_, AppServices>, device_id: DeviceId) -> Result<(), IpcError> {
    let ctx = services.context().await?;
    services
        .app()?
        .devices
        .remove(&ctx, device_id)
        .await
        .map_err(err)?;
    services.devices_changed().await;
    Ok(())
}

/// The receipt printer's live state (any signed-in user: the status bar). A printer that
/// pushes its status also sends every change to the window as `printer://status`.
#[tauri::command]
#[specta::specta]
pub async fn printer_status(services: State<'_, AppServices>) -> Result<PrinterStatusDto, IpcError> {
    let ctx = services.context().await?;
    let v = services.app()?.devices.printer_view(&ctx).await.map_err(err)?;
    Ok(printer_status_dto(&v))
}

/// What the status bar shows for a printer view.
pub fn printer_status_dto(v: &PrinterView) -> PrinterStatusDto {
    let (state, message) = match (&v.status, &v.problem) {
        (_, Some(p)) => ("offline", Some(p.clone())),
        (Some(s), None) => {
            if let Some(f) = s.blocking_failure() {
                let m = match f {
                    pos_device_ports::PrintFailure::PaperOut => "Out of paper",
                    pos_device_ports::PrintFailure::CoverOpen => "The cover is open",
                    _ => "The printer is offline",
                };
                ("offline", Some(s.error.clone().unwrap_or_else(|| m.into())))
            } else if s.paper == PaperLevel::NearEnd {
                ("warning", Some("Paper is running out".into()))
            } else if s.paper == PaperLevel::Unknown {
                ("unknown", None)
            } else {
                ("ready", None)
            }
        }
        (None, None) => ("unknown", None),
    };
    PrinterStatusDto {
        configured: v.configured,
        adapter: v.adapter.into(),
        target: v.target.clone(),
        state: state.into(),
        message,
        drawer: v.drawer,
        drawer_open: v.status.as_ref().and_then(|s| s.drawer_open),
        push: v.push,
    }
}

/// Follows the current printer's pushed status and sends every change out as a
/// [`PrinterStatusDto`] (the shell forwards them to the window as `printer://status`).
pub struct PrinterStatusFeed {
    task: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    pub events: tokio::sync::broadcast::Sender<PrinterStatusDto>,
}

impl Default for PrinterStatusFeed {
    fn default() -> Self {
        Self {
            task: std::sync::Mutex::new(None),
            events: tokio::sync::broadcast::channel(16).0,
        }
    }
}

impl PrinterStatusFeed {
    /// Follows `printer` (the one just activated) instead of the previous one.
    pub fn follow(&self, printer: Arc<pos_application::devices::SwitchablePrinter>) {
        let mut task = self
            .task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(t) = task.take() {
            t.abort();
        }
        let handle = printer.current();
        let Some(mut updates) = handle.printer.status_updates() else {
            return;
        };
        let events = self.events.clone();
        *task = Some(tokio::spawn(async move {
            while updates.changed().await.is_ok() {
                let pushed = updates.borrow_and_update().clone();
                let (status, problem) = match pushed {
                    PushedStatus::Status(s) => (Some(s), None),
                    PushedStatus::Lost(why) => (None, Some(why)),
                    PushedStatus::None => (None, None),
                };
                let view = PrinterView {
                    configured: printer.is_configured(),
                    adapter: handle.printer.adapter(),
                    target: handle.printer.target(),
                    status,
                    problem,
                    drawer: handle.drawer.is_some(),
                    push: handle.printer.pushes_status(),
                };
                let _ = events.send(printer_status_dto(&view));
            }
        }));
    }
}

/// Prints a test page on the current printer: ruler, character set, barcode, cut.
#[tauri::command]
#[specta::specta]
pub async fn printer_test(services: State<'_, AppServices>) -> Result<TestPrintDto, IpcError> {
    use pos_device_ports::ReceiptOutputPort;
    use pos_receipts::text::{Style, TextLine};
    let ctx = services.context().await?;
    let app = services.app()?;
    let printer = app.devices.printer.clone();
    let caps = printer.capabilities();
    let columns = pos_application::printing::columns_for(&caps, 80);
    let ruler: String = (0..columns)
        .map(|i| char::from(b'0' + u8::try_from((i + 1) % 10).unwrap_or(0)))
        .collect();
    let line = |text: String, style| TextLine { text, style };
    let lines = vec![
        line("SALE OS TEST PRINT".into(), Style::Centered),
        line(
            format!("{} · {}", printer.adapter(), printer.target().unwrap_or_default()),
            Style::Centered,
        ),
        line("-".repeat(columns), Style::Normal),
        line(ruler, Style::Normal),
        line(format!("{columns} characters per line"), Style::Normal),
        line("Bold line".into(), Style::Bold),
        line("Café crème · Größe · Ñandú · £ € ¥".into(), Style::Normal),
        line("Żółć · Čeština · Привет · Ελλάδα".into(), Style::Normal),
        line("TEST-0001".into(), Style::Barcode),
    ];
    let bytes = pos_application::printing::encode_for(&caps, &lines, columns);
    let result = tokio::time::timeout(Duration::from_secs(15), app.devices.test_print(&ctx, &bytes))
        .await
        .map_err(|_| {
            IpcError::coded(
                ErrorCode::PrintUnknownOutcome,
                "the printer did not answer in 15 seconds",
            )
        })?
        .map_err(err)?;
    Ok(match result {
        PrintResult::Printed => TestPrintDto {
            outcome: "printed".into(),
            message: "Printed. Check the ruler fits one line and the accents look right.".into(),
        },
        PrintResult::Failed(f) => TestPrintDto {
            outcome: "failed".into(),
            message: format!("Not printed: {}", f.code().replace('_', " ")),
        },
        PrintResult::Uncertain(r) => TestPrintDto {
            outcome: "uncertain".into(),
            message: format!("Sent, but the printer did not confirm: {r}"),
        },
    })
}

/// Printer models the till knows (Settings → Devices model picker).
#[tauri::command]
#[specta::specta]
pub async fn printer_profiles(services: State<'_, AppServices>) -> Result<Vec<PrinterProfileDto>, IpcError> {
    let ctx = services.context().await?;
    ctx.authorize(pos_domain::Permission::DeviceConfigure)
        .map_err(err)?;
    Ok(pos_device_hw::profiles()
        .iter()
        .map(|p| PrinterProfileDto {
            id: p.id.clone(),
            vendor: p.vendor.clone(),
            name: p.name.clone(),
            dialect: format!("{:?}", p.dialect),
            columns: p.font_a_columns,
            columns_font_b: p.font_b_columns,
            paper_width_mm: p.paper_width_mm,
            cutter: p.full_cut || p.partial_cut,
            drawer: p.drawer,
            barcode: p.barcode,
            qr: p.qr,
            status_query: p.status_query,
            text_supported: p.text_supported,
            verified: p.verified != Some(false),
            notes: p.notes.clone(),
        })
        .collect())
}

/// Finds printers and scanners: serial/USB ports with vendor hints, device files, printer
/// queues, and, when `subnet` is given (a /24 or smaller), network printers on `port`.
#[tauri::command]
#[specta::specta]
pub async fn devices_discover(
    services: State<'_, AppServices>,
    subnet: Option<String>,
    port: Option<u16>,
) -> Result<DiscoveryDto, IpcError> {
    let ctx = services.context().await?;
    ctx.authorize(pos_domain::Permission::DeviceConfigure)
        .map_err(err)?;
    let t = Duration::from_secs(3);
    let (serial_ports, device_files, queues) = tokio::join!(
        pos_device_hw::discovery::serial_ports(t),
        pos_device_hw::discovery::device_files(t),
        pos_device_hw::discovery::spooler_queues(t),
    );
    let (network, network_problem) = match subnet.filter(|s| !s.trim().is_empty()) {
        None => (Vec::new(), None),
        Some(s) => match pos_device_hw::discovery::scan_subnet(
            s.trim(),
            port.unwrap_or(9100),
            64,
            Duration::from_millis(400),
            true,
        )
        .await
        {
            Ok(found) => (found, None),
            Err(e) => (Vec::new(), Some(e.to_string())),
        },
    };
    Ok(DiscoveryDto {
        serial_ports,
        device_files,
        queues,
        network,
        network_problem,
    })
}

/// The USB HID-POS scanners attached now (Settings → Devices picks one).
#[tauri::command]
#[specta::specta]
pub async fn hid_scanners_list(
    services: State<'_, AppServices>,
) -> Result<Vec<pos_device_hw::HidPosDevice>, IpcError> {
    let ctx = services.context().await?;
    ctx.authorize(pos_domain::Permission::DeviceConfigure)
        .map_err(err)?;
    let app = services.app()?;
    tokio::time::timeout(Duration::from_secs(5), app.scanners.hid_devices())
        .await
        .map_err(|_| IpcError::coded(ErrorCode::DeviceUnavailable, "listing USB scanners took too long"))?
        .map_err(|e| {
            IpcError::coded(
                ErrorCode::DeviceUnavailable,
                format!("USB scanners could not be listed: {e}"),
            )
        })
}

/// The serial and HID-POS scanners set up on this till and how each is doing (any
/// signed-in user: the status bar).
#[tauri::command]
#[specta::specta]
pub async fn scanner_status(services: State<'_, AppServices>) -> Result<Vec<ScannerStatusDto>, IpcError> {
    let ctx = services.context().await?;
    ctx.actor().map_err(err)?;
    Ok(services.app()?.scanners.status())
}

/// "No sale": opens the cash drawer with a reason. Cashiers need a manager's approval.
#[tauri::command]
#[specta::specta]
pub async fn drawer_open(
    services: State<'_, AppServices>,
    reason: String,
    approval_token_id: Option<pos_domain::ApprovalId>,
) -> Result<(), IpcError> {
    let (ctx, token) = services.context_with_approval(approval_token_id).await?;
    let result = services
        .app()?
        .devices
        .open_drawer(&ctx, &reason)
        .await
        .map_err(err);
    services.settle(token, &result);
    result
}

// ---------- customer display ----------

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DisplayStatusDto {
    /// False: no customer display is set up.
    pub configured: bool,
    /// Which display, e.g. `COM5 @ 9600, ESC/POS`.
    pub target: Option<String>,
    /// `ready`, `offline` or `unknown` (nothing shown yet).
    pub state: String,
    pub message: Option<String>,
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DisplayTestDto {
    /// `shown` or `failed`.
    pub outcome: String,
    /// Which display it went to (`shown`), or why it did not take it (`failed`).
    pub message: String,
}

/// How the customer display is doing (any signed-in user). Its problems never stop a sale.
#[tauri::command]
#[specta::specta]
pub async fn display_status(services: State<'_, AppServices>) -> Result<DisplayStatusDto, IpcError> {
    let ctx = services.context().await?;
    let v = services.app()?.devices.display_view(&ctx).map_err(err)?;
    let (state, message) = match v.health {
        DeviceHealth::Ready => ("ready", None),
        DeviceHealth::Warning(m) | DeviceHealth::Offline(m) => ("offline", Some(m)),
        DeviceHealth::Unknown => ("unknown", None),
    };
    Ok(DisplayStatusDto {
        configured: v.configured,
        target: v.target,
        state: state.into(),
        message,
    })
}

/// Shows a test pattern on the customer display (Settings → Devices; audited).
#[tauri::command]
#[specta::specta]
pub async fn display_test(services: State<'_, AppServices>) -> Result<DisplayTestDto, IpcError> {
    let ctx = services.context().await?;
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        services.app()?.devices.test_display(&ctx),
    )
    .await
    .map_err(|_| {
        IpcError::coded(
            ErrorCode::DeviceUnavailable,
            "the display did not answer in 10 seconds",
        )
    })?
    .map_err(err)?;
    Ok(match result {
        Ok(target) => DisplayTestDto {
            outcome: "shown".into(),
            message: target,
        },
        Err(why) => DisplayTestDto {
            outcome: "failed".into(),
            message: why,
        },
    })
}

// ---------- scale ----------

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ScaleStatusDto {
    /// False: no scale is set up; items sold by weight take a typed quantity.
    pub configured: bool,
    /// Which scale, e.g. `COM3 @ 9600, Toledo 8217 (kg)`.
    pub target: Option<String>,
    /// `ready`, `offline` or `unknown` (not read yet).
    pub state: String,
    pub message: Option<String>,
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ScaleTestDto {
    /// `stable`, `moving`, `overload`, `underload` or `failed`.
    pub outcome: String,
    /// The weight in grams, exact decimal text, when the scale gave one.
    pub weight_grams: Option<String>,
    /// Why it failed (`failed`).
    pub message: Option<String>,
}

/// How the scale is doing (any signed-in user: the till weighs only when one is set up).
#[tauri::command]
#[specta::specta]
pub async fn scale_status(services: State<'_, AppServices>) -> Result<ScaleStatusDto, IpcError> {
    let ctx = services.context().await?;
    let v = services.app()?.devices.scale_view(&ctx).map_err(err)?;
    let (state, message) = match v.health {
        DeviceHealth::Ready => ("ready", None),
        DeviceHealth::Warning(m) | DeviceHealth::Offline(m) => ("offline", Some(m)),
        DeviceHealth::Unknown => ("unknown", None),
    };
    Ok(ScaleStatusDto {
        configured: v.configured,
        target: v.target,
        state: state.into(),
        message,
    })
}

/// Reads the scale once (Settings → Devices → Test scale; audited).
#[tauri::command]
#[specta::specta]
pub async fn scale_test(services: State<'_, AppServices>) -> Result<ScaleTestDto, IpcError> {
    use pos_application::devices::ScaleTest;
    use pos_device_ports::ScaleReading;
    let ctx = services.context().await?;
    let result = tokio::time::timeout(Duration::from_secs(10), services.app()?.devices.test_scale(&ctx))
        .await
        .map_err(|_| {
            IpcError::coded(
                ErrorCode::DeviceUnavailable,
                "the scale did not answer in 10 seconds",
            )
        })?
        .map_err(err)?;
    let dto = |outcome: &str, w: Option<pos_device_ports::Weight>| ScaleTestDto {
        outcome: outcome.into(),
        weight_grams: w.map(|w| w.grams_text()),
        message: None,
    };
    Ok(match result {
        ScaleTest::Read(ScaleReading::Stable(w)) => dto("stable", Some(w)),
        ScaleTest::Read(ScaleReading::Moving(w)) => dto("moving", w),
        ScaleTest::Read(ScaleReading::Overload) => dto("overload", None),
        ScaleTest::Read(ScaleReading::Underload) => dto("underload", None),
        ScaleTest::Failed(why) => ScaleTestDto {
            outcome: "failed".into(),
            weight_grams: None,
            message: Some(why),
        },
    })
}

/// At payment: the customer display shows the open cart's total due. Never fails because
/// of the display (it may be missing or unplugged).
#[tauri::command]
#[specta::specta]
pub async fn display_show_total(services: State<'_, AppServices>) -> Result<(), IpcError> {
    let ctx = services.context().await?;
    let app = services.app()?;
    let view = app.carts.current(&ctx).await.map_err(err)?;
    app.devices.display.show_total_due(&view);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pos_device_hw::hidpos::{EmulatedHid, default_reports};
    use pos_domain::Timestamp;

    fn hid_device_record(device: &pos_device_hw::HidPosDevice) -> DeviceRecord {
        DeviceRecord {
            device_id: "0190a5b0-0000-7000-8000-000000000001".parse().unwrap(),
            register_id: "0190a5b0-0000-7000-8000-000000000002".parse().unwrap(),
            kind: DeviceKind::Scanner,
            role: "hidpos".into(),
            name: "USB scanner".into(),
            config: ScannerSettings::HidPos(pos_device_hw::HidPosConfig::for_device(device)).to_json(),
            enabled: true,
            version: 1,
            created_at: Timestamp::from_millis(0),
            updated_at: Timestamp::from_millis(0),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_hid_pos_scanner_feeds_the_same_scan_events_as_a_serial_one() {
        let device = pos_device_hw::HidPosDevice {
            path: "/dev/hidraw2".into(),
            vendor_id: 0x05e0,
            product_id: 0x1300,
            serial_number: None,
            manufacturer: Some("Zebra".into()),
            product: Some("DS2208".into()),
        };
        let emu = EmulatedHid::new(device.clone());
        let hub = ScannerHub::with_hid_backend(Arc::new(emu.clone()));
        assert_eq!(hub.hid_devices().await.unwrap(), std::slice::from_ref(&device));
        let mut scans = hub.scans.subscribe();
        hub.restart(&[hid_device_record(&device)]).await;

        let wait_state = |want: &'static str| {
            let hub = &hub;
            async move {
                for _ in 0..250 {
                    let s = hub.status();
                    if s.first().is_some_and(|r| r.state == want) {
                        return s[0].clone();
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                panic!("scanner never {want}: {:?}", hub.status());
            }
        };
        let ready = wait_state("ready").await;
        assert_eq!(ready.connection, "hidPos");
        assert_eq!(ready.target, "Zebra DS2208 (05e0:1300)");

        emu.send(default_reports(b"]E0", b"4006381333931"));
        let scan = tokio::time::timeout(Duration::from_secs(5), scans.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(scan.raw, "]E04006381333931");
        assert_eq!(scan.port, "Zebra DS2208 (05e0:1300)");

        emu.unplug();
        let offline = wait_state("offline").await;
        assert!(offline.message.unwrap().contains("Zebra DS2208"));
        hub.restart(&[]).await;
        assert!(hub.status().is_empty());
    }
}
