//! Products CSV import, the pure part (E06-T07, CAT-008, PRD §16.3): reading the file,
//! mapping its columns, checking every row, grouping rows into products with variants, and
//! finding duplicates inside the file. Matching against the existing catalogue and saving
//! are the application's (`pos_application::import`).
//!
//! - Encoding: UTF-8 (with or without a byte-order mark); anything that is not valid UTF-8
//!   is read as Windows-1252 (what Excel writes on Windows).
//! - Delimiter: `,`, `;` or tab, whichever the header row has most of.
//! - RFC 4180 quoting; an unterminated quote or a row with the wrong number of columns is
//!   an error for that row (it is never guessed at).
//! - Rows with the same parent SKU (or, without that column, the same product name) are
//!   one product; each row is a variant. If any row of a product has an error, none of the
//!   product is imported.
//! - The products export (E14-T04) uses the same column names, so an export imports back.

use crate::catalog::barcode::normalize_barcode;
use crate::catalog::product::{BarcodeInput, ProductInput, ProductType, VariantInput};
use crate::catalog::unit::Unit;
use crate::{CategoryId, Money, Quantity, TaxCategoryId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// The most data rows a file may have.
pub const MAX_ROWS: usize = 50_000;
/// The largest file accepted (bytes).
pub const MAX_FILE_BYTES: usize = 20 * 1024 * 1024;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Encoding {
    Utf8,
    Utf8Bom,
    Windows1252,
}

/// Windows-1252 bytes 0x80–0x9F (the rest of the range is the same as Latin-1). Unused
/// positions map to the C1 control of the same value.
const CP1252_HIGH: [char; 32] = [
    '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8D}', 'Ž', '\u{8F}', '\u{90}',
    '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9D}', 'ž', 'Ÿ',
];

/// The file's text and how it was encoded.
pub fn decode(bytes: &[u8]) -> (String, Encoding) {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF])
        && let Ok(s) = std::str::from_utf8(rest)
    {
        return (s.to_owned(), Encoding::Utf8Bom);
    }
    if let Ok(s) = std::str::from_utf8(bytes) {
        return (s.to_owned(), Encoding::Utf8);
    }
    let text = bytes
        .iter()
        .map(|&b| match b {
            0x80..=0x9F => CP1252_HIGH[usize::from(b - 0x80)],
            _ => char::from(b),
        })
        .collect();
    (text, Encoding::Windows1252)
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FileError {
    #[error("the file is empty")]
    Empty,
    #[error("the file is larger than {} MB", MAX_FILE_BYTES / 1024 / 1024)]
    TooLarge,
    #[error("the file has {0} rows; at most {MAX_ROWS} can be imported at once")]
    TooManyRows(usize),
    #[error("the header row has an unterminated quote")]
    BadHeader,
}

/// One data row as read: where it starts in the file and its fields.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CsvRow {
    /// Line of the file where the row starts (the header is line 1).
    pub line: u32,
    pub fields: Vec<String>,
    /// A problem reading the row itself (quoting, column count).
    pub problem: Option<RowError>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CsvFile {
    pub delimiter: char,
    pub header: Vec<String>,
    pub rows: Vec<CsvRow>,
}

/// Picks the delimiter the header line (outside quotes) has most of; `,` on a tie.
fn detect_delimiter(text: &str) -> char {
    let mut counts = [(',', 0usize), (';', 0), ('\t', 0)];
    let mut quoted = false;
    for c in text.chars() {
        match c {
            '"' => quoted = !quoted,
            '\n' | '\r' if !quoted => break,
            _ if !quoted => {
                if let Some(e) = counts.iter_mut().find(|(d, _)| *d == c) {
                    e.1 += 1;
                }
            }
            _ => {}
        }
    }
    counts
        .iter()
        .fold((',', 0), |best, &(d, n)| if n > best.1 { (d, n) } else { best })
        .0
}

/// Splits RFC 4180 text into records: `(first line, fields, terminated)`. A quote that is
/// never closed swallows the rest of the file into that record, which is then reported.
fn records(text: &str, delimiter: char) -> Vec<(u32, Vec<String>, bool)> {
    let mut out = Vec::new();
    let mut fields: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut line: u32 = 1;
    let mut start: u32 = 1;
    let mut chars = text.chars().peekable();
    let mut any = false;
    while let Some(c) = chars.next() {
        any = true;
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => quoted = false,
                '\n' => {
                    line += 1;
                    field.push('\n');
                }
                _ => field.push(c),
            }
            continue;
        }
        match c {
            '"' if field.is_empty() => quoted = true,
            c if c == delimiter => fields.push(std::mem::take(&mut field)),
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' | '\r' => {
                fields.push(std::mem::take(&mut field));
                out.push((start, std::mem::take(&mut fields), true));
                line += 1;
                start = line;
                any = false;
            }
            _ => field.push(c),
        }
    }
    if any || !field.is_empty() || !fields.is_empty() {
        fields.push(field);
        out.push((start, fields, !quoted));
    }
    out
}

/// Reads the CSV text: the header and the data rows, skipping blank lines.
pub fn read_csv(text: &str) -> Result<CsvFile, FileError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    if text.trim().is_empty() {
        return Err(FileError::Empty);
    }
    let delimiter = detect_delimiter(text);
    let mut recs = records(text, delimiter)
        .into_iter()
        .filter(|(_, f, _)| !(f.len() == 1 && f[0].trim().is_empty()));
    let (_, header, ok) = recs.next().ok_or(FileError::Empty)?;
    if !ok {
        return Err(FileError::BadHeader);
    }
    let header: Vec<String> = header.into_iter().map(|h| h.trim().to_owned()).collect();
    let width = header.len();
    let mut rows = Vec::new();
    for (line, mut fields, terminated) in recs {
        let problem = if !terminated {
            Some(RowError::new(None, ErrorKind::UnterminatedQuote))
        } else {
            // Extra empty trailing fields (a trailing delimiter) are harmless.
            while fields.len() > width && fields.last().is_some_and(|f| f.trim().is_empty()) {
                fields.pop();
            }
            (fields.len() != width).then(|| {
                RowError::new(
                    None,
                    ErrorKind::ColumnCount {
                        expected: width,
                        found: fields.len(),
                    },
                )
            })
        };
        rows.push(CsvRow {
            line,
            fields,
            problem,
        });
        if rows.len() > MAX_ROWS {
            return Err(FileError::TooManyRows(rows.len()));
        }
    }
    Ok(CsvFile {
        delimiter,
        header,
        rows,
    })
}

/// What a column of the file means.
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, strum::EnumIter,
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum Column {
    Name,
    Variant,
    ParentSku,
    Sku,
    Barcode,
    Price,
    Cost,
    Unit,
    TaxCategory,
    Category,
    TrackStock,
    LowStockThreshold,
    ScanQuantity,
    Status,
}

impl Column {
    pub fn label(self) -> &'static str {
        match self {
            Column::Name => "name",
            Column::Variant => "variant",
            Column::ParentSku => "parent SKU",
            Column::Sku => "SKU",
            Column::Barcode => "barcode",
            Column::Price => "price",
            Column::Cost => "cost",
            Column::Unit => "unit",
            Column::TaxCategory => "tax category",
            Column::Category => "category",
            Column::TrackStock => "track stock",
            Column::LowStockThreshold => "low-stock threshold",
            Column::ScanQuantity => "scan quantity",
            Column::Status => "status",
        }
    }

    /// Header names recognised for this column (compared lower case, with `_ - .` read as
    /// spaces). The first is what the products export writes.
    fn synonyms(self) -> &'static [&'static str] {
        match self {
            Column::Name => &["name", "product", "product name", "item", "item name", "title"],
            Column::Variant => &["variant", "variant name", "variant label", "option", "options"],
            Column::ParentSku => &["parent sku", "parent", "group", "handle"],
            Column::Sku => &["sku", "code", "item code", "product code", "article number"],
            Column::Barcode => &["barcode", "barcodes", "ean", "ean13", "upc", "gtin"],
            Column::Price => &[
                "price",
                "sale price",
                "selling price",
                "retail price",
                "unit price",
            ],
            Column::Cost => &["cost", "cost price", "unit cost", "purchase price"],
            Column::Unit => &["unit", "uom", "unit of measure"],
            Column::TaxCategory => &["tax category", "tax", "vat", "tax class", "vat category"],
            Column::Category => &["category", "department"],
            Column::TrackStock => &["track stock", "stock tracked", "inventory tracked"],
            Column::LowStockThreshold => &[
                "low stock threshold",
                "low stock",
                "reorder level",
                "reorder point",
            ],
            Column::ScanQuantity => &["scan quantity", "units per scan"],
            Column::Status => &["status"],
        }
    }

    pub const REQUIRED: [Column; 2] = [Column::Name, Column::Price];
}

/// Which file column (by index) holds each meaning.
pub type Mapping = BTreeMap<Column, usize>;

fn header_key(h: &str) -> String {
    h.to_lowercase()
        .chars()
        .map(|c| if matches!(c, '_' | '-' | '.') { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Maps the headers that name a known column; each column and each header is used once.
pub fn suggest_mapping(header: &[String]) -> Mapping {
    let keys: Vec<String> = header.iter().map(|h| header_key(h)).collect();
    let mut m = Mapping::new();
    for col in <Column as strum::IntoEnumIterator>::iter() {
        if let Some(i) = keys
            .iter()
            .enumerate()
            .find(|(i, k)| col.synonyms().contains(&k.as_str()) && !m.values().any(|v| v == i))
            .map(|(i, _)| i)
        {
            m.insert(col, i);
        }
    }
    m
}

/// Why a row cannot be imported.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    #[error("the row has an unterminated quote")]
    UnterminatedQuote,
    #[error("the row has {found} columns; the header has {expected}")]
    ColumnCount { expected: usize, found: usize },
    #[error("required")]
    Required,
    #[error("not a number: {value:?}")]
    InvalidNumber { value: String },
    #[error("cannot be negative")]
    Negative,
    #[error("too many decimal places (at most {max})")]
    TooManyDecimals { max: u32 },
    #[error("must be greater than zero")]
    NotPositive,
    #[error("must be a whole number for unit {unit}")]
    FractionNotAllowed { unit: String },
    #[error("{value:?} is not a unit (use ea, kg, g, l, ml, m, cm, box or pack)")]
    UnknownUnit { value: String },
    #[error("there is none called {value:?}")]
    UnknownTaxCategory { value: String },
    #[error("there is none called {value:?} (categories are not created by an import)")]
    UnknownCategory { value: String },
    #[error("not yes or no: {value:?}")]
    InvalidBoolean { value: String },
    #[error("not active or archived: {value:?}")]
    InvalidStatus { value: String },
    #[error("too long (at most {max} characters)")]
    TooLong { max: usize },
    #[error("{value:?}: {reason}")]
    InvalidBarcode { value: String, reason: String },
    #[error("{value} is also on row {line}")]
    DuplicateBarcodeInFile { value: String, line: u32 },
    #[error("{value} is also on row {line}")]
    DuplicateSkuInFile { value: String, line: u32 },
    #[error("{value} already belongs to {product:?}")]
    BarcodeTaken { value: String, product: String },
    #[error("{value} already belongs to {product:?}")]
    SkuTaken { value: String, product: String },
    #[error("{product:?} already exists")]
    ProductExists { product: String },
    #[error("matches more than one existing product ({products})")]
    AmbiguousMatch { products: String },
    #[error("differs from the product's first row (row {line})")]
    DiffersInGroup { line: u32 },
    #[error("a product with several rows needs a variant name on each row")]
    VariantRequired,
    #[error("variant {value:?} is also on row {line}")]
    DuplicateVariant { value: String, line: u32 },
    #[error("another row of this product has an error")]
    GroupHasErrors,
    #[error("{reason}")]
    Product { reason: String },
}

/// An error on a row, and the column it is about (if one).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowError {
    pub column: Option<Column>,
    pub kind: ErrorKind,
}

impl RowError {
    pub fn new(column: Option<Column>, kind: ErrorKind) -> Self {
        Self { column, kind }
    }

    /// "price: not a number: \"1,2,3\"", or just the message without a column.
    pub fn message(&self) -> String {
        match self.column {
            Some(c) => format!("{}: {}", c.label(), self.kind),
            None => self.kind.to_string(),
        }
    }
}

/// What the import needs to know about the shop.
#[derive(Clone, Debug)]
pub struct ImportContext {
    pub currency_exponent: u8,
    /// Active tax categories: id and name.
    pub tax_categories: Vec<(TaxCategoryId, String)>,
    pub default_tax_category: TaxCategoryId,
    /// Active categories: id and name.
    pub categories: Vec<(CategoryId, String)>,
}

/// A row that read correctly, ready to be grouped.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedRow {
    /// Rows with the same key are one product (parent SKU, else the name, lower case).
    pub group: String,
    pub name: String,
    pub variant: String,
    pub sku: Option<String>,
    /// Normalised barcode values, the first is the primary.
    pub barcodes: Vec<String>,
    pub price_minor: i64,
    pub cost_minor: Option<i64>,
    pub unit: String,
    pub tax_category_id: TaxCategoryId,
    pub category_id: Option<CategoryId>,
    pub track_stock: bool,
    pub low_stock_milli: Option<i64>,
    pub scan_qty_milli: i64,
    /// `status: archived` rows are left out of the import (reported as skipped).
    pub archived: bool,
}

/// A plain decimal with `.` or `,` as the decimal mark, scaled to `scale` digits. Spaces and
/// a currency symbol around the number are ignored; thousands separators are not accepted
/// (`1,234.50` is ambiguous across locales).
fn decimal(input: &str, scale: u32) -> Result<i64, ErrorKind> {
    let invalid = || ErrorKind::InvalidNumber {
        value: input.to_owned(),
    };
    let s: String = input
        .chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, '$' | '€' | '£' | '¥' | '₹'))
        .collect();
    let (negative, body) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.as_str()),
    };
    let marks = body.chars().filter(|c| matches!(c, '.' | ',')).count();
    if marks > 1 {
        return Err(invalid());
    }
    let (int_part, frac) = body.split_once(['.', ',']).unwrap_or((body, ""));
    if (int_part.is_empty() && frac.is_empty())
        || !int_part.chars().all(|c| c.is_ascii_digit())
        || !frac.chars().all(|c| c.is_ascii_digit())
    {
        return Err(invalid());
    }
    if frac.len() > scale as usize {
        return Err(ErrorKind::TooManyDecimals { max: scale });
    }
    if int_part.trim_start_matches('0').len() > 15 {
        return Err(invalid());
    }
    let int_val: i64 = if int_part.is_empty() {
        0
    } else {
        int_part.parse().map_err(|_| invalid())?
    };
    let frac_val: i64 = if scale == 0 {
        0
    } else {
        format!("{frac:0<width$}", width = scale as usize)
            .parse()
            .map_err(|_| invalid())?
    };
    let magnitude = int_val
        .checked_mul(10_i64.pow(scale))
        .and_then(|v| v.checked_add(frac_val))
        .ok_or_else(invalid)?;
    if negative && magnitude != 0 {
        return Err(ErrorKind::Negative);
    }
    Ok(magnitude)
}

fn boolean(v: &str) -> Result<bool, ErrorKind> {
    match v.trim().to_lowercase().as_str() {
        "" | "yes" | "y" | "true" | "1" => Ok(true),
        "no" | "n" | "false" | "0" => Ok(false),
        _ => Err(ErrorKind::InvalidBoolean { value: v.to_owned() }),
    }
}

fn find_named<T: Copy>(list: &[(T, String)], name: &str) -> Option<T> {
    let n = name.trim().to_lowercase();
    list.iter()
        .find(|(_, x)| x.trim().to_lowercase() == n)
        .map(|(id, _)| *id)
}

/// Reads one row with the mapping. Every problem on the row is reported, not just the first.
pub fn parse_row(
    fields: &[String],
    mapping: &Mapping,
    ctx: &ImportContext,
) -> Result<ParsedRow, Vec<RowError>> {
    let mut errors = Vec::new();
    let get = |c: Column| {
        mapping
            .get(&c)
            .and_then(|&i| fields.get(i))
            .map(|s| s.trim())
            .unwrap_or("")
    };
    let mut err = |c: Column, k: ErrorKind| errors.push(RowError::new(Some(c), k));

    let name = get(Column::Name).to_owned();
    if name.is_empty() {
        err(Column::Name, ErrorKind::Required);
    } else if name.chars().count() > crate::catalog::product::MAX_NAME_LEN {
        err(
            Column::Name,
            ErrorKind::TooLong {
                max: crate::catalog::product::MAX_NAME_LEN,
            },
        );
    }
    let unit_text = get(Column::Unit).to_lowercase();
    let unit = if unit_text.is_empty() {
        Some(Unit::Ea)
    } else {
        let u = Unit::from_code(&unit_text);
        if u.is_none() {
            err(
                Column::Unit,
                ErrorKind::UnknownUnit {
                    value: unit_text.clone(),
                },
            );
        }
        u
    };
    let exp = u32::from(ctx.currency_exponent);
    let price = match get(Column::Price) {
        "" => {
            err(Column::Price, ErrorKind::Required);
            0
        }
        p => decimal(p, exp).unwrap_or_else(|k| {
            err(Column::Price, k);
            0
        }),
    };
    let cost = match get(Column::Cost) {
        "" => None,
        c => decimal(c, exp).map(Some).unwrap_or_else(|k| {
            err(Column::Cost, k);
            None
        }),
    };
    let mut quantity = |c: Column, allow_zero: bool| -> Option<i64> {
        let v = get(c);
        if v.is_empty() {
            return None;
        }
        match decimal(v, crate::quantity::QUANTITY_SCALE) {
            Err(k) => {
                err(c, k);
                None
            }
            Ok(0) if !allow_zero => {
                err(c, ErrorKind::NotPositive);
                None
            }
            Ok(q) => {
                if let Some(u) = unit
                    && !u.allows_fraction()
                    && q % 1000 != 0
                {
                    err(
                        c,
                        ErrorKind::FractionNotAllowed {
                            unit: u.code().to_owned(),
                        },
                    );
                }
                Some(q)
            }
        }
    };
    let low = quantity(Column::LowStockThreshold, true);
    let scan = quantity(Column::ScanQuantity, false).unwrap_or(1000);
    let tax_text = get(Column::TaxCategory);
    let tax = if tax_text.is_empty() {
        ctx.default_tax_category
    } else {
        find_named(&ctx.tax_categories, tax_text).unwrap_or_else(|| {
            err(
                Column::TaxCategory,
                ErrorKind::UnknownTaxCategory {
                    value: tax_text.to_owned(),
                },
            );
            ctx.default_tax_category
        })
    };
    let category_text = get(Column::Category);
    let category = if category_text.is_empty() {
        None
    } else {
        let c = find_named(&ctx.categories, category_text);
        if c.is_none() {
            err(
                Column::Category,
                ErrorKind::UnknownCategory {
                    value: category_text.to_owned(),
                },
            );
        }
        c
    };
    let track = boolean(get(Column::TrackStock)).unwrap_or_else(|k| {
        err(Column::TrackStock, k);
        true
    });
    let archived = match get(Column::Status).to_lowercase().as_str() {
        "" | "active" => false,
        "archived" | "inactive" => true,
        other => {
            err(
                Column::Status,
                ErrorKind::InvalidStatus {
                    value: other.to_owned(),
                },
            );
            false
        }
    };
    let sku = match get(Column::Sku) {
        "" => None,
        s if s.chars().count() > crate::catalog::product::MAX_SKU_LEN => {
            err(
                Column::Sku,
                ErrorKind::TooLong {
                    max: crate::catalog::product::MAX_SKU_LEN,
                },
            );
            None
        }
        s => Some(s.to_owned()),
    };
    let mut barcodes: Vec<String> = Vec::new();
    for raw in get(Column::Barcode)
        .split(['|', ';', ',', ' ', '\t'])
        .filter(|b| !b.is_empty())
    {
        match normalize_barcode(raw, None) {
            Ok(n) => {
                if !barcodes.contains(&n.value) {
                    barcodes.push(n.value);
                }
            }
            Err(e) => err(
                Column::Barcode,
                ErrorKind::InvalidBarcode {
                    value: raw.to_owned(),
                    reason: e.to_string(),
                },
            ),
        }
    }
    let variant = get(Column::Variant).to_owned();
    let parent = get(Column::ParentSku);
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(ParsedRow {
        group: if parent.is_empty() {
            format!("name:{}", name.to_lowercase())
        } else {
            format!("parent:{}", parent.to_lowercase())
        },
        name,
        variant,
        sku,
        barcodes,
        price_minor: price,
        cost_minor: cost,
        unit: unit.unwrap_or(Unit::Ea).code().to_owned(),
        tax_category_id: tax,
        category_id: category,
        track_stock: track,
        low_stock_milli: low,
        scan_qty_milli: scan,
        archived,
    })
}

/// Rows of one product, in file order (by line).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub key: String,
    pub lines: Vec<u32>,
}

/// Groups parsed rows into products and checks what can only be seen across rows: the
/// same barcode or SKU twice in the file, a product's rows disagreeing on unit, tax or
/// category, and variant names. Returns the groups (archived rows left out) and the errors
/// per line.
pub fn group_rows(rows: &[(u32, ParsedRow)]) -> (Vec<Group>, BTreeMap<u32, Vec<RowError>>) {
    let mut errors: BTreeMap<u32, Vec<RowError>> = BTreeMap::new();
    let mut add = |line: u32, e: RowError| errors.entry(line).or_default().push(e);
    let mut seen_codes: HashMap<&str, u32> = HashMap::new();
    let mut seen_skus: HashMap<String, u32> = HashMap::new();
    let mut groups: Vec<Group> = Vec::new();
    let mut index: HashMap<&str, usize> = HashMap::new();
    let by_line: HashMap<u32, &ParsedRow> = rows.iter().map(|(l, r)| (*l, r)).collect();
    for (line, r) in rows.iter().filter(|(_, r)| !r.archived) {
        for b in &r.barcodes {
            if let Some(first) = seen_codes.get(b.as_str()) {
                add(
                    *line,
                    RowError::new(
                        Some(Column::Barcode),
                        ErrorKind::DuplicateBarcodeInFile {
                            value: b.clone(),
                            line: *first,
                        },
                    ),
                );
            } else {
                seen_codes.insert(b, *line);
            }
        }
        if let Some(s) = &r.sku {
            let key = s.to_uppercase();
            if let Some(first) = seen_skus.get(&key) {
                add(
                    *line,
                    RowError::new(
                        Some(Column::Sku),
                        ErrorKind::DuplicateSkuInFile {
                            value: s.clone(),
                            line: *first,
                        },
                    ),
                );
            } else {
                seen_skus.insert(key, *line);
            }
        }
        match index.get(r.group.as_str()) {
            Some(&g) => groups[g].lines.push(*line),
            None => {
                index.insert(&r.group, groups.len());
                groups.push(Group {
                    key: r.group.clone(),
                    lines: vec![*line],
                });
            }
        }
    }
    for g in &groups {
        let first_line = g.lines[0];
        let first = by_line[&first_line];
        let mut variants: HashMap<String, u32> = HashMap::new();
        for &line in &g.lines {
            let r = by_line[&line];
            if line != first_line {
                for (col, differs) in [
                    (Column::Unit, r.unit != first.unit),
                    (Column::TaxCategory, r.tax_category_id != first.tax_category_id),
                    (Column::Category, r.category_id != first.category_id),
                ] {
                    if differs {
                        add(
                            line,
                            RowError::new(Some(col), ErrorKind::DiffersInGroup { line: first_line }),
                        );
                    }
                }
            }
            if g.lines.len() > 1 {
                let v = r.variant.trim().to_lowercase();
                if v.is_empty() {
                    add(
                        line,
                        RowError::new(Some(Column::Variant), ErrorKind::VariantRequired),
                    );
                } else if let Some(other) = variants.get(&v) {
                    add(
                        line,
                        RowError::new(
                            Some(Column::Variant),
                            ErrorKind::DuplicateVariant {
                                value: r.variant.clone(),
                                line: *other,
                            },
                        ),
                    );
                } else {
                    variants.insert(v, line);
                }
            }
        }
    }
    (groups, errors)
}

/// The product input for a group's rows (all valid). `existing` maps a variant to keep by
/// its label, SKU or barcode when updating a product (the caller decides the matches);
/// unmatched rows become new variants.
pub fn product_input(rows: &[&ParsedRow], existing_ids: &[Option<crate::VariantId>]) -> ProductInput {
    let first = rows[0];
    let several = rows.len() > 1;
    let variants = rows
        .iter()
        .zip(existing_ids.iter().chain(std::iter::repeat(&None)))
        .map(|(r, id)| {
            let mut options = BTreeMap::new();
            if several || !r.variant.trim().is_empty() {
                options.insert("Variant".to_owned(), r.variant.trim().to_owned());
            }
            VariantInput {
                id: *id,
                sku: r.sku.clone(),
                options,
                price: Money::from_minor(r.price_minor),
                cost: r.cost_minor.map(Money::from_minor),
                track_stock: r.track_stock,
                low_stock_threshold: r.low_stock_milli.map(Quantity::from_milli),
                scan_qty: Quantity::from_milli(r.scan_qty_milli),
                barcodes: r
                    .barcodes
                    .iter()
                    .enumerate()
                    .map(|(i, b)| {
                        let mut input = BarcodeInput::alias(b);
                        if i == 0 {
                            input.qualifier = crate::catalog::product::BarcodeQualifier::Primary;
                        }
                        input
                    })
                    .collect(),
            }
        })
        .collect();
    ProductInput {
        name: first.name.clone(),
        description: None,
        category_id: first.category_id,
        product_type: ProductType::Stock,
        unit: Unit::from_code(&first.unit).unwrap_or(Unit::Ea),
        tax_category_id: first.tax_category_id,
        quick_select: false,
        variants,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ImportContext {
        let ids = crate::UuidV7Generator;
        let std = TaxCategoryId::generate(&ids);
        ImportContext {
            currency_exponent: 2,
            tax_categories: vec![
                (std, "Standard".into()),
                (TaxCategoryId::generate(&ids), "Zero".into()),
            ],
            default_tax_category: std,
            categories: vec![(CategoryId::generate(&ids), "Drinks".into())],
        }
    }

    fn file(text: &str) -> CsvFile {
        read_csv(text).unwrap()
    }

    fn parse(text: &str) -> Vec<Result<ParsedRow, Vec<RowError>>> {
        let f = file(text);
        let m = suggest_mapping(&f.header);
        let c = ctx();
        f.rows
            .iter()
            .map(|r| match &r.problem {
                Some(p) => Err(vec![p.clone()]),
                None => parse_row(&r.fields, &m, &c),
            })
            .collect()
    }

    fn kinds(r: &Result<ParsedRow, Vec<RowError>>) -> Vec<ErrorKind> {
        r.as_ref().unwrap_err().iter().map(|e| e.kind.clone()).collect()
    }

    #[test]
    fn decodes_utf8_bom_and_windows_1252() {
        assert_eq!(decode("Crème".as_bytes()), ("Crème".into(), Encoding::Utf8));
        assert_eq!(decode(b"\xEF\xBB\xBFname"), ("name".into(), Encoding::Utf8Bom));
        // "Café €2" as Windows-1252.
        assert_eq!(
            decode(b"Caf\xE9 \x802"),
            ("Café €2".into(), Encoding::Windows1252)
        );
    }

    #[test]
    fn detects_the_delimiter_and_reads_quotes() {
        let f = file("name;price;sku\r\n\"Cola; 330ml\";1,20;\"C\"\"1\"\r\n\r\nTea;2;T\n");
        assert_eq!(f.delimiter, ';');
        assert_eq!(f.header, vec!["name", "price", "sku"]);
        assert_eq!(f.rows.len(), 2);
        assert_eq!(f.rows[0].fields, vec!["Cola; 330ml", "1,20", "C\"1"]);
        assert_eq!((f.rows[0].line, f.rows[1].line), (2, 4));
        let f = file("name\tprice\nA\t1\n");
        assert_eq!(f.delimiter, '\t');
        // A quoted field may hold a line break; the next row's line number counts it.
        let f = file("name,price\n\"Two\nlines\",1\nB,2\n");
        assert_eq!(f.rows[0].fields[0], "Two\nlines");
        assert_eq!(f.rows[1].line, 4);
    }

    #[test]
    fn malformed_rows_are_reported_not_guessed() {
        let f = file("name,price,sku\nA,1\nB,2,X,\nC,3,Y,extra\n\"D,4,Z\n");
        let problems: Vec<Option<ErrorKind>> =
            f.rows.iter().map(|r| r.problem.clone().map(|p| p.kind)).collect();
        assert_eq!(
            problems,
            vec![
                Some(ErrorKind::ColumnCount {
                    expected: 3,
                    found: 2
                }),
                None, // a trailing delimiter is fine
                Some(ErrorKind::ColumnCount {
                    expected: 3,
                    found: 4
                }),
                Some(ErrorKind::UnterminatedQuote),
            ]
        );
        assert_eq!(read_csv("  \n").unwrap_err(), FileError::Empty);
        assert_eq!(read_csv("\"name,price\n").unwrap_err(), FileError::BadHeader);
    }

    #[test]
    fn maps_headers_by_synonyms() {
        let h: Vec<String> = [
            "Product Name",
            "Selling_Price",
            "EAN",
            "VAT",
            "Units per scan",
            "Notes",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        let m = suggest_mapping(&h);
        assert_eq!(m.get(&Column::Name), Some(&0));
        assert_eq!(m.get(&Column::Price), Some(&1));
        assert_eq!(m.get(&Column::Barcode), Some(&2));
        assert_eq!(m.get(&Column::TaxCategory), Some(&3));
        assert_eq!(m.get(&Column::ScanQuantity), Some(&4));
        assert_eq!(m.len(), 5);
        // The products export's header maps completely.
        let export: Vec<String> = [
            "name",
            "variant",
            "sku",
            "barcode",
            "price",
            "cost",
            "unit",
            "tax_category",
            "track_stock",
            "low_stock_threshold",
            "scan_quantity",
            "status",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        assert_eq!(suggest_mapping(&export).len(), 12);
    }

    #[test]
    fn checks_each_value() {
        let rows = parse(
            "name,price,cost,unit,tax_category,category,track_stock,scan_quantity,barcode\n\
             Cola,1.20,€0.80,ea,standard,drinks,yes,1,4006381333931\n\
             ,,,,,,,,\n\
             Kettle,-5.00,,ea,,,,,\n\
             Rice,1.234,,sack,Luxury,Food,maybe,0.5,\n\
             Bolts,\"1,2,3\",,ea,,,,,ÄBC\n\
             Cheese,4,,kg,,,no,0.25,\n",
        );
        let cola = rows[0].as_ref().unwrap();
        assert_eq!(
            (cola.price_minor, cola.cost_minor, cola.unit.as_str()),
            (120, Some(80), "ea")
        );
        assert_eq!(cola.barcodes, vec!["4006381333931"]);
        assert!(cola.category_id.is_some());
        assert_eq!(kinds(&rows[1]), vec![ErrorKind::Required, ErrorKind::Required]);
        assert_eq!(kinds(&rows[2]), vec![ErrorKind::Negative]);
        assert_eq!(
            kinds(&rows[3]),
            vec![
                ErrorKind::UnknownUnit { value: "sack".into() },
                ErrorKind::TooManyDecimals { max: 2 },
                ErrorKind::UnknownTaxCategory {
                    value: "Luxury".into()
                },
                ErrorKind::UnknownCategory { value: "Food".into() },
                ErrorKind::InvalidBoolean {
                    value: "maybe".into()
                },
            ]
        );
        let bolts = kinds(&rows[4]);
        assert_eq!(
            bolts[0],
            ErrorKind::InvalidNumber {
                value: "1,2,3".into()
            }
        );
        assert!(matches!(bolts[1], ErrorKind::InvalidBarcode { .. }), "{bolts:?}");
        let cheese = rows[5].as_ref().unwrap();
        assert_eq!(
            (cheese.unit.as_str(), cheese.scan_qty_milli, cheese.track_stock),
            ("kg", 250, false)
        );
        // A fraction for "each".
        let r = parse("name,price,unit,scan_quantity\nEgg,0.30,ea,1.5\n");
        assert_eq!(
            kinds(&r[0]),
            vec![ErrorKind::FractionNotAllowed { unit: "ea".into() }]
        );
        // A decimal comma, as from a semicolon-separated file.
        let r = parse("name;price\nTea;2,50\n");
        assert_eq!(r[0].as_ref().unwrap().price_minor, 250);
    }

    #[test]
    fn groups_variants_and_finds_duplicates_in_the_file() {
        let rows: Vec<(u32, ParsedRow)> = parse(
            "name,variant,sku,barcode,price,unit\n\
             T-shirt,S,TS-S,,10,ea\n\
             T-shirt,M,TS-M,,10,ea\n\
             T-SHIRT,M,TS-M2,,10,ea\n\
             T-shirt,,TS-L,,10,ea\n\
             T-shirt,XL,TS-XL,,10,kg\n\
             Cola,,COLA,4006381333931,1.2,ea\n\
             Cola zero,,cola,4006381333931,1.2,ea\n",
        )
        .into_iter()
        .enumerate()
        .map(|(i, r)| (i as u32 + 2, r.unwrap()))
        .collect();
        let (groups, errors) = group_rows(&rows);
        assert_eq!(
            groups.iter().map(|g| g.lines.clone()).collect::<Vec<_>>(),
            vec![vec![2, 3, 4, 5, 6], vec![7], vec![8]]
        );
        let at = |l: u32| {
            errors
                .get(&l)
                .map(|e| e.iter().map(|x| x.kind.clone()).collect::<Vec<_>>())
                .unwrap_or_default()
        };
        assert!(at(2).is_empty() && at(3).is_empty() && at(7).is_empty());
        assert_eq!(
            at(4),
            vec![ErrorKind::DuplicateVariant {
                value: "M".into(),
                line: 3
            }]
        );
        assert_eq!(at(5), vec![ErrorKind::VariantRequired]);
        assert_eq!(at(6), vec![ErrorKind::DiffersInGroup { line: 2 }]);
        assert_eq!(
            at(8),
            vec![
                ErrorKind::DuplicateBarcodeInFile {
                    value: "4006381333931".into(),
                    line: 7
                },
                ErrorKind::DuplicateSkuInFile {
                    value: "cola".into(),
                    line: 7
                },
            ]
        );
        // Grouped rows become one product with one variant each.
        let shirt: Vec<&ParsedRow> = [2u32, 3].iter().map(|l| &rows[*l as usize - 2].1).collect();
        let input = product_input(&shirt, &[]);
        assert_eq!(input.variants.len(), 2);
        assert_eq!(
            input.variants[1].options.get("Variant").map(String::as_str),
            Some("M")
        );
        let ids = crate::UuidV7Generator;
        let p = crate::catalog::product::Product::create(input, &ids).unwrap();
        assert_eq!(p.variants[0].label(), "S");
    }

    #[test]
    fn parent_sku_groups_rows_with_different_names() {
        let rows: Vec<(u32, ParsedRow)> = parse(
            "name,variant,parent_sku,price\nShirt red,Red,SHIRT,10\nShirt blue,Blue,SHIRT,10\nShirt,,,10\n",
        )
        .into_iter()
        .enumerate()
        .map(|(i, r)| (i as u32 + 2, r.unwrap()))
        .collect();
        let (groups, errors) = group_rows(&rows);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(
            groups.iter().map(|g| g.lines.len()).collect::<Vec<_>>(),
            vec![2, 1]
        );
    }
}
