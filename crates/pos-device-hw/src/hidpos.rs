//! USB HID-POS barcode scanners (HID Point of Sale usage page 0x8C, "Bar Code Scanner"),
//! implementing [`BarcodeInputPort`].
//!
//! In HID-POS mode a scanner does not type keys: it sends each scan in input reports of
//! the *Scanned Data Report* (usage 0x8C:0x12). Each report carries the three symbology
//! identifier bytes (usages 0xFB–0xFD, usually the AIM identifier such as `]E0`), a block
//! of decoded data (0xFE, 56 bytes on most scanners) and a *Decode Data Continued* bit
//! (0xFF) set on every report but the last of a long code. [`ScanAssembler`] joins those
//! parts into one scan; the AIM identifier is kept in front of the data so the scan
//! pipeline's decoder (E21-T01) uses it exactly as for a keyboard or serial scan.
//!
//! Where the fields sit comes from the device's report descriptor when the system gives
//! it ([`ReportLayout::from_descriptor`]); otherwise the common layout of Honeywell,
//! Zebra and Datalogic scanners is assumed ([`ReportLayout::default`]): report ID 2, a
//! length byte, the three symbology bytes, 56 data bytes and the continued flag in bit 0
//! of the last byte. Reports with another ID (status, trigger, vendor reports) are ignored.
//!
//! The HID I/O is behind [`HidBackend`] / [`HidLink`]: the app uses [`HidApiBackend`]
//! (`hidapi`: hidraw on Linux without libudev, `hid.dll` on Windows, IOKit on macOS), tests
//! an emulated scanner that replays report bytes. Like the serial scanner, a reader thread
//! owns the device, reopens it after unplugging (found again by vendor, product and serial
//! number, since the path may change) and publishes every change as [`DeviceHealth`].
//!
//! Written from the HID Point of Sale Usage Tables and vendor HID-POS documentation; not yet
//! tried on a real scanner (D-07).

use crate::config::FieldError;
use crate::scanner::Backoff;
use pos_device_ports::{BarcodeInputPort, DeviceError, DeviceHealth, ScanEvent, ScanSource};
use pos_domain::{Clock, IdGenerator, ScanEventId, SystemClock, UuidV7Generator};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, watch};

/// HID usage page "Bar Code Scanner" (HID Point of Sale Usage Tables).
pub const POS_BARCODE_PAGE: u16 = 0x8C;
/// Usage "Bar Code Scanner" on that page (the top-level collection of a scanner).
pub const USAGE_BARCODE_SCANNER: u16 = 0x02;

const USAGE_SYMBOLOGY_1: u16 = 0xFB;
const USAGE_SYMBOLOGY_2: u16 = 0xFC;
const USAGE_SYMBOLOGY_3: u16 = 0xFD;
const USAGE_DECODED_DATA: u16 = 0xFE;
const USAGE_DATA_CONTINUED: u16 = 0xFF;

/// Longest scan accepted (a QR code holds at most 7,089 characters).
pub const MAX_SCAN_BYTES: usize = 7_089;

// ---------------------------------------------------------------------------------------
// Report layout

/// Where the parts of a scanned-data report sit, as byte offsets in the report that the
/// system returns (with the report ID as byte 0 when the device numbers its reports).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReportLayout {
    /// 0: the device does not number its reports (no ID byte).
    pub report_id: u8,
    /// A byte giving how many data bytes of this report are used. Without one, trailing
    /// zero bytes are padding.
    pub length_at: Option<usize>,
    /// The three symbology identifier bytes.
    pub symbology_at: [Option<usize>; 3],
    pub data_at: usize,
    pub data_len: usize,
    /// Byte and bit mask of *Decode Data Continued*.
    pub continued: Option<(usize, u8)>,
}

impl Default for ReportLayout {
    /// The layout most HID-POS scanners use (64-byte report 2).
    fn default() -> Self {
        Self {
            report_id: 2,
            length_at: Some(1),
            symbology_at: [Some(2), Some(3), Some(4)],
            data_at: 5,
            data_len: 56,
            continued: Some((63, 0x01)),
        }
    }
}

/// One report's share of a scan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReportPart {
    pub symbology: [u8; 3],
    pub data: Vec<u8>,
    pub continued: bool,
}

impl ReportLayout {
    /// Reads the layout from a HID report descriptor: the input report holding *Decoded
    /// Data* on the bar code scanner page, with its symbology and continued fields. `None`
    /// when the descriptor has no such report (the caller then uses the default layout).
    pub fn from_descriptor(descriptor: &[u8]) -> Option<Self> {
        let fields = input_fields(descriptor)?;
        let page = u32::from(POS_BARCODE_PAGE) << 16;
        let usage = |u: u16| page | u32::from(u);
        let data_field = fields
            .iter()
            .find(|f| f.size == 8 && f.usage_at(0) == Some(usage(USAGE_DECODED_DATA)))?;
        let report_id = data_field.report_id;
        let base = if report_id == 0 { 0 } else { 8 };
        // Every element of every field of this report, by usage.
        let mut at: BTreeMap<u32, Vec<(u32, u32)>> = BTreeMap::new();
        for f in fields.iter().filter(|f| f.report_id == report_id) {
            for i in 0..f.count {
                if let Some(u) = f.usage_at(i) {
                    at.entry(u)
                        .or_default()
                        .push((base + f.bit_offset + i * f.size, f.size));
                }
            }
        }
        let byte_of = |u: u16| -> Option<usize> {
            let (bit, size) = *at.get(&usage(u))?.first()?;
            (size == 8 && bit % 8 == 0).then_some((bit / 8) as usize)
        };
        let data = at.get(&usage(USAGE_DECODED_DATA))?;
        let (first, _) = *data.first()?;
        if first % 8 != 0 {
            return None;
        }
        let continued = at
            .get(&usage(USAGE_DATA_CONTINUED))
            .and_then(|v| v.first())
            .map(|&(bit, _)| ((bit / 8) as usize, 1u8 << (bit % 8)));
        Some(Self {
            report_id,
            length_at: None,
            symbology_at: [
                byte_of(USAGE_SYMBOLOGY_1),
                byte_of(USAGE_SYMBOLOGY_2),
                byte_of(USAGE_SYMBOLOGY_3),
            ],
            data_at: (first / 8) as usize,
            data_len: data.len(),
            continued,
        })
    }

    /// This report's part of a scan, or `None` when it is another report (another ID) or
    /// too short to be one.
    pub fn parse(&self, report: &[u8]) -> Option<ReportPart> {
        if self.report_id != 0 && report.first() != Some(&self.report_id) {
            return None;
        }
        if report.len() <= self.data_at {
            return None;
        }
        let end = (self.data_at + self.data_len).min(report.len());
        let block = &report[self.data_at..end];
        let used = match self.length_at.and_then(|i| report.get(i)) {
            Some(&n) if usize::from(n) <= block.len() => usize::from(n),
            _ => block.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1),
        };
        let byte = |i: Option<usize>| i.and_then(|i| report.get(i)).copied().unwrap_or(0);
        Some(ReportPart {
            symbology: [
                byte(self.symbology_at[0]),
                byte(self.symbology_at[1]),
                byte(self.symbology_at[2]),
            ],
            data: block[..used].to_vec(),
            continued: self
                .continued
                .and_then(|(i, mask)| report.get(i).map(|b| b & mask != 0))
                .unwrap_or(false),
        })
    }
}

/// One input field of a report descriptor.
#[derive(Clone, Debug)]
struct InputField {
    report_id: u8,
    /// Bits from the start of the report's data (after the ID byte).
    bit_offset: u32,
    size: u32,
    count: u32,
    /// Full usages (page << 16 | id); the last repeats for the remaining elements.
    usages: Vec<u32>,
}

impl InputField {
    fn usage_at(&self, i: u32) -> Option<u32> {
        let i = usize::try_from(i).ok()?;
        self.usages.get(i).or(self.usages.last()).copied()
    }
}

#[derive(Clone, Copy, Default)]
struct Globals {
    usage_page: u32,
    report_size: u32,
    report_count: u32,
    report_id: u8,
}

/// The input fields of a report descriptor (HID 1.11 §6.2.2), or `None` if it is malformed.
fn input_fields(d: &[u8]) -> Option<Vec<InputField>> {
    let mut g = Globals::default();
    let mut stack: Vec<Globals> = Vec::new();
    let mut usages: Vec<u32> = Vec::new();
    let (mut usage_min, mut usage_max): (Option<u32>, Option<u32>) = (None, None);
    let mut offsets: BTreeMap<u8, u32> = BTreeMap::new();
    let mut fields = Vec::new();
    let mut i = 0;
    while i < d.len() {
        let prefix = d[i];
        if prefix == 0xFE {
            // Long item: size in the next byte.
            let size = usize::from(*d.get(i + 1)?);
            i += 3 + size;
            continue;
        }
        let size = match prefix & 0x03 {
            3 => 4,
            n => usize::from(n),
        };
        let bytes = d.get(i + 1..i + 1 + size)?;
        let value = bytes.iter().rev().fold(0u32, |acc, &b| (acc << 8) | u32::from(b));
        let local_usage = |v: u32| {
            if size == 4 {
                v
            } else {
                (g.usage_page << 16) | (v & 0xFFFF)
            }
        };
        match (prefix >> 2) & 0x03 {
            // Main items.
            0 => {
                let tag = prefix >> 4;
                if tag == 0x8 {
                    // Input.
                    let bits = g.report_size.checked_mul(g.report_count)?;
                    let mut list = std::mem::take(&mut usages);
                    if let (Some(lo), Some(hi)) = (usage_min, usage_max)
                        && hi >= lo
                        && hi - lo < 4096
                    {
                        list.extend(lo..=hi);
                    }
                    let constant = value & 0x01 != 0;
                    let offset = offsets.entry(g.report_id).or_default();
                    if !constant && !list.is_empty() {
                        fields.push(InputField {
                            report_id: g.report_id,
                            bit_offset: *offset,
                            size: g.report_size,
                            count: g.report_count,
                            usages: list,
                        });
                    }
                    *offset = offset.checked_add(bits)?;
                }
                usages.clear();
                usage_min = None;
                usage_max = None;
            }
            // Global items.
            1 => match prefix >> 4 {
                0x0 => g.usage_page = value,
                0x7 => g.report_size = value,
                0x8 => g.report_id = u8::try_from(value).ok()?,
                0x9 => g.report_count = value,
                0xA => stack.push(g),
                0xB => g = stack.pop()?,
                _ => {}
            },
            // Local items.
            2 => match prefix >> 4 {
                0x0 => usages.push(local_usage(value)),
                0x1 => usage_min = Some(local_usage(value)),
                0x2 => usage_max = Some(local_usage(value)),
                _ => {}
            },
            _ => {}
        }
        i += 1 + size;
    }
    Some(fields)
}

// ---------------------------------------------------------------------------------------
// Assembling scans

/// One complete scan from a HID-POS scanner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HidScan {
    pub symbology: [u8; 3],
    pub data: Vec<u8>,
}

impl HidScan {
    /// The AIM symbology identifier (`]E0`), when the scanner sent one.
    pub fn aim(&self) -> Option<String> {
        let [a, b, c] = self.symbology;
        (a == b']' && b.is_ascii_graphic() && c.is_ascii_graphic())
            .then(|| format!("]{}{}", char::from(b), char::from(c)))
    }

    /// What the scan pipeline gets: the AIM identifier, then the data (GS kept; a trailing
    /// CR or LF suffix dropped). UTF-8, or Latin-1 when the bytes are not UTF-8.
    pub fn raw(&self) -> String {
        let mut data: &[u8] = &self.data;
        while let [rest @ .., b'\r' | b'\n'] = data {
            data = rest;
        }
        let text = match std::str::from_utf8(data) {
            Ok(s) => s.to_owned(),
            Err(_) => data.iter().map(|&b| char::from(b)).collect(),
        };
        match self.aim() {
            Some(aim) if !text.is_empty() => format!("{aim}{text}"),
            _ => text,
        }
    }
}

/// What one report did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Assembled {
    /// A scan is complete.
    Scan(HidScan),
    /// Part of a long scan; more reports follow.
    Partial,
    /// Not a scanned-data report (another report ID, too short, empty).
    Ignored,
}

/// Joins the reports of a scan. A scan whose last report never comes (the scanner was
/// unplugged mid-code) is dropped after `max_gap`, never sent half.
#[derive(Debug)]
pub struct ScanAssembler {
    layout: ReportLayout,
    max_gap: Duration,
    pending: Option<(HidScan, Instant)>,
}

impl ScanAssembler {
    pub fn new(layout: ReportLayout) -> Self {
        Self {
            layout,
            max_gap: Duration::from_millis(500),
            pending: None,
        }
    }

    pub fn layout(&self) -> &ReportLayout {
        &self.layout
    }

    pub fn push(&mut self, report: &[u8], now: Instant) -> Assembled {
        let Some(part) = self.layout.parse(report) else {
            return Assembled::Ignored;
        };
        if self
            .pending
            .as_ref()
            .is_some_and(|(_, at)| now.saturating_duration_since(*at) > self.max_gap)
        {
            self.pending = None;
        }
        let (mut scan, _) = self.pending.take().unwrap_or_else(|| {
            (
                HidScan {
                    symbology: part.symbology,
                    data: Vec::new(),
                },
                now,
            )
        });
        scan.data.extend_from_slice(&part.data);
        if scan.data.len() > MAX_SCAN_BYTES {
            return Assembled::Ignored;
        }
        if part.continued {
            self.pending = Some((scan, now));
            return Assembled::Partial;
        }
        if scan.data.is_empty() {
            Assembled::Ignored
        } else {
            Assembled::Scan(scan)
        }
    }

    /// Drops a partial scan (the device went away).
    pub fn reset(&mut self) {
        self.pending = None;
    }
}

// ---------------------------------------------------------------------------------------
// HID I/O

/// A HID-POS scanner the system lists.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct HidPosDevice {
    /// The system's path for it (`/dev/hidraw3`, `\\?\HID#VID_0C2E…`); may change after
    /// unplugging.
    pub path: String,
    pub vendor_id: u16,
    pub product_id: u16,
    pub serial_number: Option<String>,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
}

impl HidPosDevice {
    /// "Honeywell Xenon 1900 (0c2e:0b61)".
    pub fn label(&self) -> String {
        let name = [self.manufacturer.as_deref(), self.product.as_deref()]
            .into_iter()
            .flatten()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let ids = format!("{:04x}:{:04x}", self.vendor_id, self.product_id);
        if name.is_empty() {
            format!("HID-POS scanner ({ids})")
        } else {
            format!("{name} ({ids})")
        }
    }
}

/// An open HID device.
pub trait HidLink: Send {
    /// Reads one input report into `buf`; `Ok(0)` when none came within `timeout`. An error
    /// means the device is gone or broken.
    fn read(&mut self, buf: &mut [u8], timeout: Duration) -> Result<usize, String>;

    /// The device's report descriptor, when the system gives it.
    fn report_descriptor(&mut self) -> Option<Vec<u8>> {
        None
    }
}

/// Lists and opens HID-POS scanners.
pub trait HidBackend: Send + Sync {
    /// The attached devices with a bar code scanner collection (usage page 0x8C).
    fn list(&self) -> Result<Vec<HidPosDevice>, String>;
    fn open(&self, device: &HidPosDevice) -> Result<Box<dyn HidLink>, String>;
}

/// The real backend: `hidapi`.
#[derive(Debug, Default)]
pub struct HidApiBackend;

impl HidBackend for HidApiBackend {
    fn list(&self) -> Result<Vec<HidPosDevice>, String> {
        let api = hidapi::HidApi::new().map_err(|e| e.to_string())?;
        let mut out: Vec<HidPosDevice> = Vec::new();
        for d in api.device_list().filter(|d| d.usage_page() == POS_BARCODE_PAGE) {
            let path = d.path().to_string_lossy().into_owned();
            if out.iter().any(|o| o.path == path) {
                continue;
            }
            out.push(HidPosDevice {
                path,
                vendor_id: d.vendor_id(),
                product_id: d.product_id(),
                serial_number: d.serial_number().filter(|s| !s.is_empty()).map(str::to_owned),
                manufacturer: d.manufacturer_string().map(str::to_owned),
                product: d.product_string().map(str::to_owned),
            });
        }
        Ok(out)
    }

    fn open(&self, device: &HidPosDevice) -> Result<Box<dyn HidLink>, String> {
        let api = hidapi::HidApi::new().map_err(|e| e.to_string())?;
        let path = std::ffi::CString::new(device.path.clone()).map_err(|e| e.to_string())?;
        let dev = api.open_path(&path).map_err(|e| e.to_string())?;
        Ok(Box::new(HidApiLink(dev)))
    }
}

struct HidApiLink(hidapi::HidDevice);

impl HidLink for HidApiLink {
    fn read(&mut self, buf: &mut [u8], timeout: Duration) -> Result<usize, String> {
        let ms = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
        self.0.read_timeout(buf, ms).map_err(|e| e.to_string())
    }

    fn report_descriptor(&mut self) -> Option<Vec<u8>> {
        let mut buf = vec![0u8; hidapi::MAX_REPORT_DESCRIPTOR_SIZE];
        let n = self.0.get_report_descriptor(&mut buf).ok()?;
        buf.truncate(n);
        Some(buf)
    }
}

// ---------------------------------------------------------------------------------------
// Settings

/// Which HID-POS scanner to use. It is found again by vendor and product ID (and serial
/// number, when it has one) after being unplugged, since its path can change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct HidPosConfig {
    pub vendor_id: u16,
    pub product_id: u16,
    #[serde(default)]
    pub serial_number: Option<String>,
    /// The path it had when chosen: preferred when several identical scanners are attached.
    #[serde(default)]
    pub path: Option<String>,
    /// What the settings screen called it ("Honeywell Xenon 1900 (0c2e:0b61)").
    #[serde(default)]
    pub label: Option<String>,
}

impl HidPosConfig {
    pub fn for_device(d: &HidPosDevice) -> Self {
        Self {
            vendor_id: d.vendor_id,
            product_id: d.product_id,
            serial_number: d.serial_number.clone(),
            path: Some(d.path.clone()),
            label: Some(d.label()),
        }
    }

    pub fn validate(&self) -> Result<(), Vec<FieldError>> {
        let mut errors = Vec::new();
        if self.vendor_id == 0 {
            errors.push(FieldError::new(
                "hidPos.vendorId",
                "choose a scanner from the list",
            ));
        }
        if self.product_id == 0 {
            errors.push(FieldError::new(
                "hidPos.productId",
                "choose a scanner from the list",
            ));
        }
        if self.serial_number.as_ref().is_some_and(|s| s.len() > 128) {
            errors.push(FieldError::new(
                "hidPos.serialNumber",
                "is longer than 128 characters",
            ));
        }
        if self.path.as_ref().is_some_and(|p| p.len() > 512) {
            errors.push(FieldError::new("hidPos.path", "is longer than 512 characters"));
        }
        if self.label.as_ref().is_some_and(|l| l.chars().count() > 120) {
            errors.push(FieldError::new("hidPos.label", "is longer than 120 characters"));
        }
        if errors.is_empty() { Ok(()) } else { Err(errors) }
    }

    /// "Honeywell Xenon 1900 (0c2e:0b61)", or the IDs.
    pub fn describe(&self) -> String {
        self.label
            .clone()
            .unwrap_or_else(|| format!("HID-POS scanner ({:04x}:{:04x})", self.vendor_id, self.product_id))
    }

    /// The attached device this configuration means: same IDs (and serial number when
    /// stored), the stored path first.
    pub fn find<'a>(&self, devices: &'a [HidPosDevice]) -> Option<&'a HidPosDevice> {
        let same = |d: &&HidPosDevice| {
            d.vendor_id == self.vendor_id
                && d.product_id == self.product_id
                && (self.serial_number.is_none() || d.serial_number == self.serial_number)
        };
        devices
            .iter()
            .filter(same)
            .find(|d| Some(&d.path) == self.path.as_ref())
            .or_else(|| devices.iter().find(same))
    }
}

// ---------------------------------------------------------------------------------------
// The scanner

struct Worker {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

pub struct HidPosScanner {
    config: HidPosConfig,
    backend: Arc<dyn HidBackend>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
    backoff: Backoff,
    scans: broadcast::Sender<ScanEvent>,
    health: Arc<watch::Sender<DeviceHealth>>,
    worker: Mutex<Option<Worker>>,
}

impl std::fmt::Debug for HidPosScanner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HidPosScanner")
            .field("device", &self.config.describe())
            .finish_non_exhaustive()
    }
}

impl HidPosScanner {
    pub fn new(config: HidPosConfig, backend: Arc<dyn HidBackend>) -> Self {
        let (scans, _) = broadcast::channel(64);
        let (health, _) = watch::channel(DeviceHealth::Unknown);
        Self {
            config,
            backend,
            clock: Arc::new(SystemClock),
            ids: Arc::new(UuidV7Generator),
            backoff: Backoff::default(),
            scans,
            health: Arc::new(health),
            worker: Mutex::new(None),
        }
    }

    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    pub fn with_ids(mut self, ids: Arc<dyn IdGenerator>) -> Self {
        self.ids = ids;
        self
    }

    pub fn with_backoff(mut self, backoff: Backoff) -> Self {
        self.backoff = backoff;
        self
    }

    pub fn config(&self) -> &HidPosConfig {
        &self.config
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Worker>> {
        self.worker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Drop for HidPosScanner {
    fn drop(&mut self) {
        if let Some(w) = self.lock().take() {
            w.stop.store(true, Ordering::SeqCst);
        }
    }
}

#[async_trait::async_trait]
impl BarcodeInputPort for HidPosScanner {
    async fn start(&self) -> Result<(), DeviceError> {
        self.config.validate().map_err(|errors| {
            DeviceError::Unsupported(format!(
                "invalid scanner settings: {}",
                errors
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ")
            ))
        })?;
        let mut worker = self.lock();
        if worker.as_ref().is_some_and(|w| !w.thread.is_finished()) {
            return Ok(());
        }
        let stop = Arc::new(AtomicBool::new(false));
        let reader = Reader {
            config: self.config.clone(),
            backend: Arc::clone(&self.backend),
            clock: Arc::clone(&self.clock),
            ids: Arc::clone(&self.ids),
            backoff: self.backoff,
            scans: self.scans.clone(),
            health: Arc::clone(&self.health),
            stop: Arc::clone(&stop),
        };
        let thread = std::thread::Builder::new()
            .name(format!(
                "hid-pos {:04x}:{:04x}",
                self.config.vendor_id, self.config.product_id
            ))
            .spawn(move || reader.run())
            .map_err(|e| DeviceError::Io(e.to_string()))?;
        *worker = Some(Worker { stop, thread });
        Ok(())
    }

    async fn stop(&self) -> Result<(), DeviceError> {
        let Some(w) = self.lock().take() else {
            return Ok(());
        };
        w.stop.store(true, Ordering::SeqCst);
        // The reader checks the flag at least every 50 ms.
        let _ = tokio::task::spawn_blocking(move || w.thread.join()).await;
        self.health.send_replace(DeviceHealth::Unknown);
        Ok(())
    }

    fn subscribe(&self) -> broadcast::Receiver<ScanEvent> {
        self.scans.subscribe()
    }

    fn health(&self) -> watch::Receiver<DeviceHealth> {
        self.health.subscribe()
    }
}

struct Reader {
    config: HidPosConfig,
    backend: Arc<dyn HidBackend>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
    backoff: Backoff,
    scans: broadcast::Sender<ScanEvent>,
    health: Arc<watch::Sender<DeviceHealth>>,
    stop: Arc<AtomicBool>,
}

/// How long one read waits, so stop() is prompt.
const READ_SLICE: Duration = Duration::from_millis(50);

impl Reader {
    fn stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    fn set_health(&self, h: DeviceHealth) {
        if *self.health.borrow() != h {
            tracing::info!(target: "pos_device_hw", device = %self.config.describe(), health = ?h, "hid-pos scanner");
            self.health.send_replace(h);
        }
    }

    fn pause(&self, d: Duration) {
        let until = Instant::now() + d;
        while !self.stopped() && Instant::now() < until {
            std::thread::sleep(
                Duration::from_millis(20).min(until.saturating_duration_since(Instant::now())),
            );
        }
    }

    fn connect(&self) -> Result<(HidPosDevice, Box<dyn HidLink>), String> {
        let devices = self.backend.list()?;
        let device = self
            .config
            .find(&devices)
            .cloned()
            .ok_or_else(|| format!("{} is not connected", self.config.describe()))?;
        let link = self
            .backend
            .open(&device)
            .map_err(|e| format!("{} could not be opened: {e}", self.config.describe()))?;
        Ok((device, link))
    }

    fn run(self) {
        let mut delay = self.backoff.initial;
        let mut buf = vec![0u8; 1024];
        while !self.stopped() {
            let (device, mut link) = match self.connect() {
                Ok(c) => c,
                Err(why) => {
                    self.set_health(DeviceHealth::Offline(why));
                    self.pause(delay);
                    delay = (delay * 2).min(self.backoff.max);
                    continue;
                }
            };
            let layout = link
                .report_descriptor()
                .and_then(|d| ReportLayout::from_descriptor(&d))
                .unwrap_or_default();
            tracing::debug!(target: "pos_device_hw", path = %device.path, ?layout, "hid-pos layout");
            let mut assembler = ScanAssembler::new(layout);
            delay = self.backoff.initial;
            self.set_health(DeviceHealth::Ready);
            while !self.stopped() {
                match link.read(&mut buf, READ_SLICE) {
                    Ok(0) => {}
                    Ok(n) => match assembler.push(&buf[..n], Instant::now()) {
                        Assembled::Scan(scan) => self.emit(&scan),
                        Assembled::Partial => {}
                        Assembled::Ignored => {
                            tracing::debug!(target: "pos_device_hw", id = buf[0], len = n, "hid-pos report ignored");
                        }
                    },
                    Err(e) => {
                        self.set_health(DeviceHealth::Offline(format!(
                            "{} disconnected (unplugged?): {e}",
                            self.config.describe()
                        )));
                        break;
                    }
                }
            }
            // A scan cut off by the disconnect is incomplete: dropped, never guessed.
            assembler.reset();
            drop(link);
            self.pause(delay);
        }
    }

    fn emit(&self, scan: &HidScan) {
        let event = ScanEvent {
            id: ScanEventId::generate(self.ids.as_ref()),
            source: ScanSource::HidPos(self.config.describe()),
            raw: scan.raw(),
            symbology_hint: None,
            received_at: self.clock.now(),
        };
        tracing::debug!(target: "pos_device_hw", device = %self.config.describe(), id = %event.id, "scan");
        let _ = self.scans.send(event);
    }
}

// ---------------------------------------------------------------------------------------
// Test support: an emulated HID-POS scanner

/// Builds the reports a scanner with the [default layout](ReportLayout::default) sends for
/// `data`: 56 bytes per report, the continued flag on all but the last.
pub fn default_reports(aim: &[u8; 3], data: &[u8]) -> Vec<Vec<u8>> {
    let layout = ReportLayout::default();
    let chunks: Vec<&[u8]> = if data.is_empty() {
        vec![&[][..]]
    } else {
        data.chunks(layout.data_len).collect()
    };
    let last = chunks.len() - 1;
    chunks
        .into_iter()
        .enumerate()
        .map(|(i, chunk)| {
            let mut r = vec![0u8; 64];
            r[0] = layout.report_id;
            r[1] = u8::try_from(chunk.len()).unwrap_or(u8::MAX);
            r[2..5].copy_from_slice(aim);
            r[5..5 + chunk.len()].copy_from_slice(chunk);
            if i != last {
                r[63] |= 0x01;
            }
            r
        })
        .collect()
}

/// The report descriptor of the HID-POS spec's example scanner: report 2 with three
/// symbology bytes, 56 data bytes, the continued bit and 7 bits of padding (no length
/// byte), plus a trigger output report.
pub fn example_descriptor() -> Vec<u8> {
    vec![
        0x05, 0x8C, // Usage Page (Bar Code Scanner)
        0x09, 0x02, // Usage (Bar Code Scanner)
        0xA1, 0x01, // Collection (Application)
        0x09, 0x12, //   Usage (Scanned Data Report)
        0xA1, 0x02, //   Collection (Logical)
        0x85, 0x02, //     Report ID (2)
        0x15, 0x00, //     Logical Minimum (0)
        0x26, 0xFF, 0x00, // Logical Maximum (255)
        0x75, 0x08, //     Report Size (8)
        0x95, 0x01, //     Report Count (1)
        0x09, 0xFB, //     Usage (Symbology Identifier 1)
        0x81, 0x02, //     Input (Data, Var, Abs)
        0x09, 0xFC, //     Usage (Symbology Identifier 2)
        0x81, 0x02, //     Input
        0x09, 0xFD, //     Usage (Symbology Identifier 3)
        0x81, 0x02, //     Input
        0x95, 0x38, //     Report Count (56)
        0x09, 0xFE, //     Usage (Decoded Data)
        0x82, 0x02, 0x01, // Input (Data, Var, Abs, Buffered Bytes)
        0x75, 0x01, //     Report Size (1)
        0x95, 0x01, //     Report Count (1)
        0x25, 0x01, //     Logical Maximum (1)
        0x09, 0xFF, //     Usage (Decode Data Continued)
        0x81, 0x02, //     Input
        0x95, 0x07, //     Report Count (7)
        0x81, 0x03, //     Input (Constant): padding
        0xC0, //       End Collection
        0x85, 0x04, //   Report ID (4)
        0x75, 0x08, //   Report Size (8)
        0x95, 0x01, //   Report Count (1)
        0x09, 0x14, //   Usage (Trigger Report)
        0x91, 0x02, //   Output
        0xC0, // End Collection
    ]
}

/// An emulated HID-POS scanner for tests (here and in the app's tests): it is listed while
/// plugged in, and each open link reads the reports queued with [`EmulatedHid::send`].
/// Unplugging breaks every open link; plugging in again may give it a new path.
#[derive(Clone)]
pub struct EmulatedHid {
    state: Arc<Mutex<EmulatedState>>,
}

struct EmulatedState {
    device: HidPosDevice,
    descriptor: Option<Vec<u8>>,
    plugged: bool,
    /// Bumped by every unplug: links from before it are dead.
    generation: u64,
    queue: std::collections::VecDeque<Vec<u8>>,
    opens: usize,
}

impl EmulatedHid {
    pub fn new(device: HidPosDevice) -> Self {
        Self {
            state: Arc::new(Mutex::new(EmulatedState {
                device,
                descriptor: None,
                plugged: true,
                generation: 0,
                queue: std::collections::VecDeque::new(),
                opens: 0,
            })),
        }
    }

    /// The report descriptor the device gives (none: the default layout is used).
    pub fn with_descriptor(self, descriptor: Vec<u8>) -> Self {
        self.lock().descriptor = Some(descriptor);
        self
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, EmulatedState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Queues input reports, read one per `read()`.
    pub fn send(&self, reports: impl IntoIterator<Item = Vec<u8>>) {
        self.lock().queue.extend(reports);
    }

    pub fn unplug(&self) {
        let mut s = self.lock();
        s.plugged = false;
        s.generation += 1;
        s.queue.clear();
    }

    /// Plugs the scanner back in, at `path` when given.
    pub fn plug(&self, path: Option<&str>) {
        let mut s = self.lock();
        if let Some(p) = path {
            s.device.path = p.to_owned();
        }
        s.plugged = true;
    }

    /// How many times the device was opened.
    pub fn opens(&self) -> usize {
        self.lock().opens
    }

    /// Whether every queued report has been read.
    pub fn drained(&self) -> bool {
        self.lock().queue.is_empty()
    }
}

impl HidBackend for EmulatedHid {
    fn list(&self) -> Result<Vec<HidPosDevice>, String> {
        let s = self.lock();
        Ok(if s.plugged {
            vec![s.device.clone()]
        } else {
            Vec::new()
        })
    }

    fn open(&self, device: &HidPosDevice) -> Result<Box<dyn HidLink>, String> {
        let mut s = self.lock();
        if !s.plugged || s.device.path != device.path {
            return Err("no such device".into());
        }
        s.opens += 1;
        Ok(Box::new(EmulatedLink {
            hid: self.clone(),
            generation: s.generation,
        }))
    }
}

struct EmulatedLink {
    hid: EmulatedHid,
    generation: u64,
}

impl HidLink for EmulatedLink {
    fn read(&mut self, buf: &mut [u8], timeout: Duration) -> Result<usize, String> {
        let until = Instant::now() + timeout;
        loop {
            {
                let mut s = self.hid.lock();
                if !s.plugged || s.generation != self.generation {
                    return Err("device disconnected".into());
                }
                if let Some(r) = s.queue.pop_front() {
                    let n = r.len().min(buf.len());
                    buf[..n].copy_from_slice(&r[..n]);
                    return Ok(n);
                }
            }
            if Instant::now() >= until {
                return Ok(0);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn report_descriptor(&mut self) -> Option<Vec<u8>> {
        self.hid.lock().descriptor.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_layout_reads_a_single_report() {
        let reports = default_reports(b"]E0", b"4006381333931");
        assert_eq!(reports.len(), 1);
        let mut a = ScanAssembler::new(ReportLayout::default());
        let Assembled::Scan(scan) = a.push(&reports[0], Instant::now()) else {
            panic!("no scan");
        };
        assert_eq!(scan.aim().as_deref(), Some("]E0"));
        assert_eq!(scan.raw(), "]E04006381333931");
    }

    #[test]
    fn long_codes_are_joined_across_reports() {
        // A GS1 DataMatrix with FNC1 (GS) separators, 150 bytes: three reports.
        let mut data = b"010950123456789\x1d10ABC123\x1d21".to_vec();
        data.extend(std::iter::repeat_n(b'X', 150 - data.len()));
        let reports = default_reports(b"]d2", &data);
        assert_eq!(reports.len(), 3);
        let mut a = ScanAssembler::new(ReportLayout::default());
        let now = Instant::now();
        assert_eq!(a.push(&reports[0], now), Assembled::Partial);
        assert_eq!(a.push(&reports[1], now), Assembled::Partial);
        let Assembled::Scan(scan) = a.push(&reports[2], now) else {
            panic!("no scan");
        };
        assert_eq!(scan.data, data);
        assert!(scan.raw().starts_with("]d2010950123456789\u{1d}10ABC123"));
    }

    #[test]
    fn other_reports_are_ignored_and_a_stalled_scan_is_dropped() {
        let mut a = ScanAssembler::new(ReportLayout::default());
        let now = Instant::now();
        // Report 4 (trigger), a short report and an empty one: nothing.
        assert_eq!(a.push(&[0x04, 0x01, 0x00], now), Assembled::Ignored);
        assert_eq!(a.push(&[0x02, 0x03], now), Assembled::Ignored);
        assert_eq!(
            a.push(&default_reports(b"\0\0\0", b"")[0], now),
            Assembled::Ignored
        );
        // The first half of a long code, then nothing for a second: the next scan starts
        // afresh instead of being glued to the stale half.
        let long = default_reports(b"]Q1", &[b'A'; 80]);
        assert_eq!(a.push(&long[0], now), Assembled::Partial);
        let next = default_reports(b"]E0", b"12345670");
        assert_eq!(
            a.push(&next[0], now + Duration::from_secs(1)),
            Assembled::Scan(HidScan {
                symbology: *b"]E0",
                data: b"12345670".to_vec()
            })
        );
        // And reset() drops a partial one.
        assert_eq!(a.push(&long[0], now), Assembled::Partial);
        a.reset();
        assert_eq!(
            a.push(&long[1], now),
            Assembled::Scan(HidScan {
                symbology: *b"]Q1",
                data: vec![b'A'; 24]
            })
        );
    }

    #[test]
    fn the_layout_comes_from_the_report_descriptor() {
        let layout = ReportLayout::from_descriptor(&example_descriptor()).expect("layout");
        assert_eq!(
            layout,
            ReportLayout {
                report_id: 2,
                length_at: None,
                symbology_at: [Some(1), Some(2), Some(3)],
                data_at: 4,
                data_len: 56,
                continued: Some((60, 0x01)),
            }
        );
        // A report in that layout: no length byte, zero padding, continued bit at byte 60.
        let mut first = vec![0u8; 61];
        first[0] = 2;
        first[1..4].copy_from_slice(b"]C0");
        first[4..60].copy_from_slice(&[b'7'; 56]);
        first[60] = 0x01;
        let mut second = vec![0u8; 61];
        second[0] = 2;
        second[1..4].copy_from_slice(b"]C0");
        second[4..8].copy_from_slice(b"TAIL");
        let mut a = ScanAssembler::new(layout);
        assert_eq!(a.push(&first, Instant::now()), Assembled::Partial);
        let Assembled::Scan(scan) = a.push(&second, Instant::now()) else {
            panic!("no scan");
        };
        assert_eq!(scan.raw(), format!("]C0{}TAIL", "7".repeat(56)));
        // A keyboard's descriptor has no scanner report.
        let keyboard = [
            0x05, 0x01, 0x09, 0x06, 0xA1, 0x01, 0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7, 0x15, 0x00, 0x25, 0x01,
            0x75, 0x01, 0x95, 0x08, 0x81, 0x02, 0xC0,
        ];
        assert_eq!(ReportLayout::from_descriptor(&keyboard), None);
        // Truncated descriptors are refused, not misread.
        assert_eq!(ReportLayout::from_descriptor(&example_descriptor()[..39]), None);
    }

    #[test]
    fn raw_text_drops_a_suffix_and_survives_latin1() {
        let s = HidScan {
            symbology: [0, 0, 0],
            data: b"caf\xe9\r\n".to_vec(),
        };
        assert_eq!(s.raw(), "caf\u{e9}");
        // A symbology byte that is not an AIM identifier (a vendor code) is not prefixed.
        let s = HidScan {
            symbology: [0x0D, 0, 0],
            data: b"123".to_vec(),
        };
        assert_eq!(s.raw(), "123");
    }

    #[test]
    fn the_configured_scanner_is_found_again_by_ids() {
        let dev = |path: &str, serial: Option<&str>| HidPosDevice {
            path: path.into(),
            vendor_id: 0x0c2e,
            product_id: 0x0b61,
            serial_number: serial.map(str::to_owned),
            manufacturer: Some("Honeywell".into()),
            product: Some("Xenon 1900".into()),
        };
        let chosen = dev("/dev/hidraw3", Some("A1"));
        let config = HidPosConfig::for_device(&chosen);
        assert_eq!(config.describe(), "Honeywell Xenon 1900 (0c2e:0b61)");
        // Replugged: a new path, same serial number.
        let now = [dev("/dev/hidraw1", Some("B2")), dev("/dev/hidraw5", Some("A1"))];
        assert_eq!(config.find(&now).map(|d| d.path.as_str()), Some("/dev/hidraw5"));
        assert_eq!(config.find(&now[..1]), None);
        // Without serial numbers, the stored path wins among identical scanners.
        let mut config = HidPosConfig::for_device(&dev("/dev/hidraw5", None));
        let twins = [dev("/dev/hidraw1", None), dev("/dev/hidraw5", None)];
        assert_eq!(config.find(&twins).map(|d| d.path.as_str()), Some("/dev/hidraw5"));
        config.path = Some("/dev/hidraw9".into());
        assert_eq!(config.find(&twins).map(|d| d.path.as_str()), Some("/dev/hidraw1"));
        assert!(config.validate().is_ok());
        config.vendor_id = 0;
        assert_eq!(config.validate().unwrap_err()[0].field, "hidPos.vendorId");
    }
}
