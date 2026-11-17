//! Products CSV import (E06-T07, CAT-008): start (read and stage the file), validate (map
//! columns, check every row, match the existing catalogue under a duplicate policy),
//! commit (in chunks of 500 products, resumable), the error report, and cancel. The rules
//! for reading and checking rows are pure: `pos_domain::catalog::import`.
//!
//! Staging lives in `import_jobs` / `import_rows` (local only). A commit chunk is one
//! transaction: the products, their outbox events, one `catalog.import_committed` audit
//! row, and the rows marked `committed` with the product they became. A crash between
//! chunks leaves whole chunks and the job `committing`; committing again continues with
//! the rows not yet committed, so nothing is created twice.

use crate::audit::{self, AuditEntry};
use crate::config::ConfigQueries;
use crate::context::{AppError, CommandContext};
use crate::queries::{CatalogQueries, QueryError, SetupQueries};
use crate::records::{ImportJobRecord, ImportRowRecord, ImportRowStatus, ImportStatus};
use crate::{LocalStore, OutboxDraft, WriteSet};
use pos_domain::catalog::import::{
    Column, ErrorKind, FileError, ImportContext, MAX_FILE_BYTES, Mapping, ParsedRow, RowError, decode,
    group_rows, parse_row, product_input, read_csv, suggest_mapping,
};
use pos_domain::catalog::product::{BarcodeInput, Product, Variant, VariantInput};
use pos_domain::{ErrorCode, ImportJobId, Permission, ProductId, VariantId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

/// Products per commit transaction.
pub const CHUNK: usize = 500;
/// Problems listed in a validation summary (the error report has all of them).
const SUMMARY_PROBLEMS: usize = 200;

/// The active catalogue, for matching: normalised barcode → product, upper-case SKU →
/// product, lower-case name → products.
#[derive(Clone, Debug, Default)]
pub struct CatalogIndex {
    pub barcodes: HashMap<String, ProductId>,
    pub skus: HashMap<String, ProductId>,
    pub names: HashMap<String, Vec<ProductId>>,
    pub product_names: HashMap<ProductId, String>,
}

#[async_trait::async_trait]
pub trait ImportQueries: Send + Sync {
    async fn import_job(&self, id: ImportJobId) -> Result<Option<ImportJobRecord>, QueryError>;
    async fn import_rows(&self, id: ImportJobId) -> Result<Vec<ImportRowRecord>, QueryError>;
    async fn catalog_index(&self) -> Result<CatalogIndex, QueryError>;
}

/// What to do with a product that already exists (same name, or a barcode or SKU that
/// belongs to it).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnDuplicate {
    /// Leave the existing product alone; the rows are skipped.
    Skip,
    /// Update the existing product with the columns the file has.
    Update,
    /// Report the rows as errors.
    Error,
}

/// The file as read, before validation.
#[derive(Clone, Debug)]
pub struct ImportPreview {
    pub job_id: ImportJobId,
    pub file_name: String,
    pub encoding: pos_domain::catalog::import::Encoding,
    pub delimiter: char,
    pub header: Vec<String>,
    /// The first rows as read (for the preview table).
    pub sample: Vec<Vec<String>>,
    pub total_rows: u32,
    pub mapping: Mapping,
}

/// One row the report lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowProblem {
    pub line: u32,
    pub status: ImportRowStatus,
    pub errors: Vec<RowError>,
}

/// The outcome of validation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportSummary {
    pub total_rows: u32,
    pub valid_rows: u32,
    pub error_rows: u32,
    pub duplicate_rows: u32,
    pub skipped_rows: u32,
    pub products_to_create: u32,
    pub products_to_update: u32,
    /// The first problems (all are in the error report).
    pub problems: Vec<RowProblem>,
}

/// The outcome of a commit.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommitSummary {
    pub products_created: u32,
    pub products_updated: u32,
    pub rows_committed: u32,
    pub chunks: u32,
}

/// What a valid row will do, stored in `parsed_json`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Plan {
    row: ParsedRow,
    /// Rows of one product share this.
    group: String,
    /// The product to update (`None`: create), with the version validation saw.
    update: Option<(ProductId, u64)>,
    /// The existing variant this row updates.
    variant_id: Option<VariantId>,
    /// Set when committed.
    product_id: Option<ProductId>,
}

pub struct ImportService {
    pub store: Arc<dyn LocalStore>,
    pub imports: Arc<dyn ImportQueries>,
    pub catalog: Arc<dyn CatalogQueries>,
    pub config: Arc<dyn ConfigQueries>,
    pub setup: Arc<dyn SetupQueries>,
}

fn invalid(msg: impl Into<String>) -> AppError {
    AppError::Validation(msg.into(), ErrorCode::ValidationFailed)
}

fn file_error(e: FileError) -> AppError {
    invalid(e.to_string())
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

impl ImportService {
    async fn job(&self, id: ImportJobId) -> Result<ImportJobRecord, AppError> {
        self.imports
            .import_job(id)
            .await?
            .ok_or_else(|| invalid("that import does not exist"))
    }

    /// Reads and stages the file. Nothing in the catalogue changes.
    pub async fn start(
        &self,
        ctx: &CommandContext,
        file_name: &str,
        bytes: &[u8],
    ) -> Result<ImportPreview, AppError> {
        ctx.authorize(Permission::CatalogImport)?;
        let actor = ctx.actor()?;
        if bytes.len() > MAX_FILE_BYTES {
            return Err(file_error(FileError::TooLarge));
        }
        let (text, encoding) = decode(bytes);
        let csv = read_csv(&text).map_err(file_error)?;
        let mapping = suggest_mapping(&csv.header);
        let job_id = ImportJobId::generate(ctx.ids.as_ref());
        let delimiter = csv.delimiter;
        let job = ImportJobRecord {
            job_id,
            file_name: file_name.to_owned(),
            file_sha256: sha256(bytes),
            mapping: Some(serde_json::to_value(&mapping).map_err(|e| AppError::Internal(e.to_string()))?),
            options: serde_json::json!({
                "header": csv.header,
                "encoding": encoding,
                "delimiter": delimiter.to_string(),
            }),
            status: ImportStatus::Parsed,
            total_rows: u32::try_from(csv.rows.len()).unwrap_or(u32::MAX),
            valid_rows: 0,
            error_rows: 0,
            created_by: actor.user_id,
            created_at: ctx.now(),
            committed_at: None,
        };
        let rows: Vec<ImportRowRecord> = csv
            .rows
            .iter()
            .map(|r| ImportRowRecord {
                line: r.line,
                raw: r.fields.clone(),
                parsed: None,
                status: if r.problem.is_some() {
                    ImportRowStatus::Error
                } else {
                    ImportRowStatus::Pending
                },
                errors: r.problem.clone().into_iter().collect(),
            })
            .collect();
        let set = WriteSet::builder(ctx.write_context())
            .put_import_job(job.clone())
            .put_import_rows(job_id, rows)
            .build()?;
        self.store.apply(set).await?;
        Ok(ImportPreview {
            job_id,
            file_name: job.file_name,
            encoding,
            delimiter,
            sample: csv.rows.iter().take(10).map(|r| r.fields.clone()).collect(),
            header: csv.header,
            total_rows: job.total_rows,
            mapping,
        })
    }

    async fn context(&self) -> Result<ImportContext, AppError> {
        let summary = self
            .setup
            .workspace_summary()
            .await?
            .ok_or_else(|| invalid("set up the business first"))?;
        let taxes: Vec<_> = self
            .config
            .all_tax_categories()
            .await?
            .into_iter()
            .filter(|t| !t.archived)
            .collect();
        let default = taxes
            .iter()
            .find(|t| t.is_default)
            .or(taxes.first())
            .map(|t| t.tax_category_id)
            .ok_or_else(|| invalid("there is no tax category"))?;
        Ok(ImportContext {
            currency_exponent: summary.currency_exponent,
            tax_categories: taxes
                .iter()
                .map(|t| (t.tax_category_id, t.name.clone()))
                .collect(),
            default_tax_category: default,
            categories: self
                .config
                .all_categories()
                .await?
                .into_iter()
                .filter(|c| !c.archived)
                .map(|c| (c.category_id, c.name))
                .collect(),
        })
    }

    /// Checks every row with `mapping` and matches products that already exist under
    /// `on_duplicate`. Can be repeated with another mapping or policy until committed.
    pub async fn validate(
        &self,
        ctx: &CommandContext,
        job_id: ImportJobId,
        mapping: Mapping,
        on_duplicate: OnDuplicate,
    ) -> Result<ImportSummary, AppError> {
        ctx.authorize(Permission::CatalogImport)?;
        let mut job = self.job(job_id).await?;
        if !matches!(job.status, ImportStatus::Parsed | ImportStatus::Validated) {
            return Err(invalid("this import can no longer be changed"));
        }
        let width = job.options["header"].as_array().map_or(0, Vec::len);
        for col in Column::REQUIRED {
            if !mapping.contains_key(&col) {
                return Err(invalid(format!("choose the column for {}", col.label())));
            }
        }
        let mut used = BTreeSet::new();
        for (col, &i) in &mapping {
            if i >= width || !used.insert(i) {
                return Err(invalid(format!(
                    "the column chosen for {} is not in the file or is used twice",
                    col.label()
                )));
            }
        }
        let ictx = self.context().await?;
        let index = self.imports.catalog_index().await?;
        let mut rows = self.imports.import_rows(job_id).await?;

        // 1. Each row on its own.
        let mut errors: BTreeMap<u32, Vec<RowError>> = BTreeMap::new();
        let mut parsed: Vec<(u32, ParsedRow)> = Vec::new();
        for r in &rows {
            if let Some(read_problem) = r.errors.iter().find(|e| {
                matches!(
                    e.kind,
                    ErrorKind::UnterminatedQuote | ErrorKind::ColumnCount { .. }
                )
            }) {
                errors.insert(r.line, vec![read_problem.clone()]);
                continue;
            }
            match parse_row(&r.raw, &mapping, &ictx) {
                Ok(p) => parsed.push((r.line, p)),
                Err(e) => {
                    errors.insert(r.line, e);
                }
            }
        }
        // 2. Across rows: groups and duplicates inside the file.
        let (groups, cross) = group_rows(&parsed);
        for (line, e) in cross {
            errors.entry(line).or_default().extend(e);
        }
        let by_line: HashMap<u32, &ParsedRow> = parsed.iter().map(|(l, r)| (*l, r)).collect();

        // 3. Against the catalogue, product by product.
        let mut status: HashMap<u32, ImportRowStatus> = HashMap::new();
        let mut plans: HashMap<u32, Plan> = HashMap::new();
        let mut summary = ImportSummary::default();
        let name_of = |id: &ProductId| index.product_names.get(id).cloned().unwrap_or_default();
        for g in &groups {
            let rows_here: Vec<&ParsedRow> = g.lines.iter().map(|l| by_line[l]).collect();
            // Products this group touches: the same name, or a code that belongs to one.
            let mut matched: BTreeSet<ProductId> = BTreeSet::new();
            if let Some(ids) = index.names.get(&rows_here[0].name.trim().to_lowercase()) {
                matched.extend(ids.iter().copied());
            }
            for r in &rows_here {
                matched.extend(r.barcodes.iter().filter_map(|b| index.barcodes.get(b)).copied());
                if let Some(s) = &r.sku {
                    matched.extend(index.skus.get(&s.to_uppercase()).copied());
                }
            }
            let group_errors = g.lines.iter().any(|l| errors.contains_key(l));
            let mut update: Option<(ProductId, u64)> = None;
            let mut variant_ids: Vec<Option<VariantId>> = vec![None; g.lines.len()];
            if !group_errors && !matched.is_empty() {
                let existing = || ErrorKind::ProductExists {
                    product: matched.iter().map(name_of).collect::<Vec<_>>().join(", "),
                };
                match on_duplicate {
                    OnDuplicate::Skip => {
                        for l in &g.lines {
                            status.insert(*l, ImportRowStatus::Skipped);
                            errors.insert(*l, vec![RowError::new(None, existing())]);
                        }
                        continue;
                    }
                    OnDuplicate::Error => {
                        for l in &g.lines {
                            status.insert(*l, ImportRowStatus::DuplicateExisting);
                            errors.insert(*l, vec![RowError::new(None, existing())]);
                        }
                        continue;
                    }
                    OnDuplicate::Update if matched.len() > 1 => {
                        for l in &g.lines {
                            status.insert(*l, ImportRowStatus::DuplicateExisting);
                            errors.insert(
                                *l,
                                vec![RowError::new(
                                    None,
                                    ErrorKind::AmbiguousMatch {
                                        products: matched.iter().map(name_of).collect::<Vec<_>>().join(", "),
                                    },
                                )],
                            );
                        }
                        continue;
                    }
                    OnDuplicate::Update => {
                        let id = *matched.iter().next().expect("one match");
                        let product = self
                            .catalog
                            .product(id)
                            .await?
                            .ok_or_else(|| invalid("product not found"))?;
                        variant_ids = match_variants(&product, &rows_here);
                        let input = update_input(&product, &rows_here, &variant_ids, &mapping);
                        let mut check = product.clone();
                        if let Err(e) = check.edit(input, ctx.ids.as_ref()) {
                            errors.insert(
                                g.lines[0],
                                vec![RowError::new(
                                    None,
                                    ErrorKind::Product {
                                        reason: e.to_string(),
                                    },
                                )],
                            );
                        } else {
                            update = Some((id, product.version));
                        }
                    }
                }
            }
            if !errors.keys().any(|l| g.lines.contains(l)) && update.is_none() {
                // A new product: let the product rules have the last word.
                if let Err(e) = Product::create(product_input(&rows_here, &[]), ctx.ids.as_ref()) {
                    errors.insert(
                        g.lines[0],
                        vec![RowError::new(
                            None,
                            ErrorKind::Product {
                                reason: e.to_string(),
                            },
                        )],
                    );
                }
            }
            if g.lines.iter().any(|l| errors.contains_key(l)) {
                for l in &g.lines {
                    errors
                        .entry(*l)
                        .or_insert_with(|| vec![RowError::new(None, ErrorKind::GroupHasErrors)]);
                }
                continue;
            }
            if update.is_some() {
                summary.products_to_update += 1;
            } else {
                summary.products_to_create += 1;
            }
            for (i, l) in g.lines.iter().enumerate() {
                status.insert(*l, ImportRowStatus::Valid);
                plans.insert(
                    *l,
                    Plan {
                        row: by_line[l].clone(),
                        group: g.key.clone(),
                        update,
                        variant_id: variant_ids[i],
                        product_id: None,
                    },
                );
            }
        }

        // 4. Every row's outcome.
        for r in &mut rows {
            let errs = errors.remove(&r.line).unwrap_or_default();
            let archived = by_line.get(&r.line).is_some_and(|p| p.archived);
            r.status = if archived {
                ImportRowStatus::Skipped
            } else if let Some(s) = status.get(&r.line) {
                *s
            } else if errs.iter().any(|e| {
                matches!(
                    e.kind,
                    ErrorKind::DuplicateBarcodeInFile { .. } | ErrorKind::DuplicateSkuInFile { .. }
                )
            }) {
                ImportRowStatus::DuplicateInFile
            } else if errs.is_empty() {
                ImportRowStatus::Valid
            } else {
                ImportRowStatus::Error
            };
            r.errors = errs;
            r.parsed = plans
                .remove(&r.line)
                .map(|p| serde_json::to_value(p).map_err(|e| AppError::Internal(e.to_string())))
                .transpose()?;
            match r.status {
                ImportRowStatus::Valid => summary.valid_rows += 1,
                ImportRowStatus::Skipped => summary.skipped_rows += 1,
                ImportRowStatus::DuplicateInFile | ImportRowStatus::DuplicateExisting => {
                    summary.duplicate_rows += 1;
                }
                _ => summary.error_rows += 1,
            }
            if (r.status.is_problem()) && summary.problems.len() < SUMMARY_PROBLEMS {
                summary.problems.push(RowProblem {
                    line: r.line,
                    status: r.status,
                    errors: r.errors.clone(),
                });
            }
        }
        summary.total_rows = u32::try_from(rows.len()).unwrap_or(u32::MAX);
        job.mapping = Some(serde_json::to_value(&mapping).map_err(|e| AppError::Internal(e.to_string()))?);
        job.options["onDuplicate"] =
            serde_json::to_value(on_duplicate).map_err(|e| AppError::Internal(e.to_string()))?;
        job.status = ImportStatus::Validated;
        job.valid_rows = summary.valid_rows;
        job.error_rows = summary.error_rows + summary.duplicate_rows;
        let set = WriteSet::builder(ctx.write_context())
            .put_import_job(job)
            .put_import_rows(job_id, rows)
            .build()?;
        self.store.apply(set).await?;
        Ok(summary)
    }

    /// Creates and updates the products, `CHUNK` at a time. Run again after a crash to
    /// finish: committed rows are not imported twice.
    pub async fn commit(&self, ctx: &CommandContext, job_id: ImportJobId) -> Result<CommitSummary, AppError> {
        ctx.authorize(Permission::CatalogImport)?;
        let mut job = self.job(job_id).await?;
        match job.status {
            ImportStatus::Validated => {
                job.status = ImportStatus::Committing;
                let set = WriteSet::builder(ctx.write_context())
                    .put_import_job(job.clone())
                    .build()?;
                self.store.apply(set).await?;
            }
            ImportStatus::Committing => {}
            _ => return Err(invalid("validate the import before committing it")),
        }
        let mapping: Mapping = job
            .mapping
            .clone()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| AppError::Internal(e.to_string()))?
            .unwrap_or_default();
        let rows = self.imports.import_rows(job_id).await?;
        // Pending products in file order: the group key → its valid rows.
        let mut order: Vec<String> = Vec::new();
        let mut groups: HashMap<String, Vec<(ImportRowRecord, Plan)>> = HashMap::new();
        for r in rows.into_iter().filter(|r| r.status == ImportRowStatus::Valid) {
            let plan: Plan = serde_json::from_value(r.parsed.clone().unwrap_or_default())
                .map_err(|e| AppError::Internal(format!("import row {}: {e}", r.line)))?;
            if !groups.contains_key(&plan.group) {
                order.push(plan.group.clone());
            }
            groups.entry(plan.group.clone()).or_default().push((r, plan));
        }
        let mut summary = CommitSummary::default();
        let ids = ctx.ids.as_ref();
        for (n, chunk) in order.chunks(CHUNK).enumerate() {
            let mut set = WriteSet::builder(ctx.write_context());
            let mut marked = Vec::new();
            let (mut created, mut updated) = (0u32, 0u32);
            for key in chunk {
                let members = &groups[key];
                let parsed: Vec<&ParsedRow> = members.iter().map(|(_, p)| &p.row).collect();
                let (product, expected) = match members[0].1.update {
                    None => {
                        created += 1;
                        (Product::create(product_input(&parsed, &[]), ids)?, None)
                    }
                    Some((id, version)) => {
                        let mut product = self
                            .catalog
                            .product(id)
                            .await?
                            .ok_or_else(|| invalid("product not found"))?;
                        if product.version != version {
                            return Err(AppError::Validation(
                                format!(
                                    "{} was changed after the import was checked; check it again",
                                    product.name
                                ),
                                ErrorCode::ConflictVersion,
                            ));
                        }
                        let variant_ids: Vec<Option<VariantId>> =
                            members.iter().map(|(_, p)| p.variant_id).collect();
                        let input = update_input(&product, &parsed, &variant_ids, &mapping);
                        product.edit(input, ids)?;
                        updated += 1;
                        (product, Some(version))
                    }
                };
                for (row, plan) in members {
                    let mut plan = plan.clone();
                    plan.product_id = Some(product.id);
                    marked.push(ImportRowRecord {
                        status: ImportRowStatus::Committed,
                        parsed: Some(
                            serde_json::to_value(plan).map_err(|e| AppError::Internal(e.to_string()))?,
                        ),
                        ..row.clone()
                    });
                }
                set = set
                    .outbox(OutboxDraft::product_upserted(ids, &product)?)
                    .put_product(product, expected);
            }
            let rows_in_chunk = u32::try_from(marked.len()).unwrap_or(u32::MAX);
            let entry = AuditEntry::new(ids, audit::CATALOG_IMPORT_COMMITTED, "import", job_id).with_metadata(
                serde_json::json!({
                    "file": job.file_name, "chunk": n + 1, "created": created, "updated": updated, "rows": rows_in_chunk,
                }),
            );
            let built = set.put_import_rows(job_id, marked).audit(entry).build()?;
            self.store.apply(built).await?;
            summary.products_created += created;
            summary.products_updated += updated;
            summary.rows_committed += rows_in_chunk;
            summary.chunks += 1;
        }
        job.status = ImportStatus::Committed;
        job.committed_at = Some(ctx.now());
        let set = WriteSet::builder(ctx.write_context())
            .put_import_job(job)
            .build()?;
        self.store.apply(set).await?;
        Ok(summary)
    }

    /// The rows that were not imported, as CSV: the original columns and an `error` column.
    /// UTF-8 with a byte-order mark, CRLF, every field quoted when needed.
    pub async fn error_report(
        &self,
        ctx: &CommandContext,
        job_id: ImportJobId,
    ) -> Result<(String, Vec<u8>), AppError> {
        ctx.authorize(Permission::CatalogImport)?;
        let job = self.job(job_id).await?;
        let header: Vec<String> = job.options["header"]
            .as_array()
            .map(|h| h.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect())
            .unwrap_or_default();
        let mut out = String::from("\u{feff}");
        let line =
            |fields: Vec<String>| fields.iter().map(|f| csv_field(f)).collect::<Vec<_>>().join(",") + "\r\n";
        let mut head = vec!["row".to_owned()];
        head.extend(header);
        head.push("error".into());
        out.push_str(&line(head));
        for r in self.imports.import_rows(job_id).await? {
            if !(r.status.is_problem() || r.status == ImportRowStatus::Skipped) {
                continue;
            }
            let mut fields = vec![r.line.to_string()];
            fields.extend(r.raw.clone());
            let why = if r.errors.is_empty() {
                "skipped: archived in the file".to_owned()
            } else {
                r.errors
                    .iter()
                    .map(RowError::message)
                    .collect::<Vec<_>>()
                    .join("; ")
            };
            fields.push(why);
            out.push_str(&line(fields));
        }
        let stem = job
            .file_name
            .rsplit_once('.')
            .map_or(job.file_name.as_str(), |(s, _)| s);
        Ok((format!("{stem}-errors.csv"), out.into_bytes()))
    }

    /// Abandons an import before it is committed. The catalogue is untouched.
    pub async fn cancel(&self, ctx: &CommandContext, job_id: ImportJobId) -> Result<(), AppError> {
        ctx.authorize(Permission::CatalogImport)?;
        let mut job = self.job(job_id).await?;
        if !matches!(job.status, ImportStatus::Parsed | ImportStatus::Validated) {
            return Err(invalid("this import has started committing; finish it instead"));
        }
        job.status = ImportStatus::Cancelled;
        let set = WriteSet::builder(ctx.write_context())
            .put_import_job(job)
            .build()?;
        self.store.apply(set).await?;
        Ok(())
    }
}

/// RFC 4180 quoting, with a leading `'` on text that a spreadsheet would run as a formula.
fn csv_field(s: &str) -> String {
    let numeric = {
        let body = s.strip_prefix(['-', '+']).unwrap_or(s);
        let (int, frac) = body.split_once(['.', ',']).unwrap_or((body, ""));
        !int.is_empty() && int.chars().all(|c| c.is_ascii_digit()) && frac.chars().all(|c| c.is_ascii_digit())
    };
    let s = if s.starts_with(['=', '+', '-', '@']) && !numeric {
        format!("'{s}")
    } else {
        s.to_owned()
    };
    if s.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s
    }
}

/// The existing variant each row updates: by SKU, then a barcode, then the variant name.
fn match_variants(product: &Product, rows: &[&ParsedRow]) -> Vec<Option<VariantId>> {
    let mut taken: BTreeSet<VariantId> = BTreeSet::new();
    rows.iter()
        .map(|r| {
            let found = product.active_variants().find(|v| {
                !taken.contains(&v.id)
                    && (r
                        .sku
                        .as_ref()
                        .is_some_and(|s| v.sku.as_ref().is_some_and(|x| x.eq_ignore_ascii_case(s)))
                        || r.barcodes
                            .iter()
                            .any(|b| v.barcodes.iter().any(|x| &x.value == b))
                        || (!r.variant.trim().is_empty() && v.label().eq_ignore_ascii_case(r.variant.trim()))
                        || (rows.len() == 1 && product.active_variants().count() == 1))
            });
            found.map(|v| {
                taken.insert(v.id);
                v.id
            })
        })
        .collect()
}

fn variant_input(v: &Variant) -> VariantInput {
    VariantInput {
        id: Some(v.id),
        sku: v.sku.clone(),
        options: v.options.clone(),
        price: v.price,
        cost: v.cost,
        track_stock: v.track_stock,
        low_stock_threshold: v.low_stock_threshold,
        scan_qty: v.scan_qty,
        barcodes: v
            .barcodes
            .iter()
            .map(|b| BarcodeInput {
                raw: b.raw.clone(),
                symbology_hint: Some(b.symbology),
                qualifier: b.qualifier,
                pack_qty: b.pack_qty,
            })
            .collect(),
    }
}

/// The updated product: rows change only the columns the file has; variants the file does
/// not mention are kept as they are; new rows add variants.
fn update_input(
    product: &Product,
    rows: &[&ParsedRow],
    variant_ids: &[Option<VariantId>],
    mapping: &Mapping,
) -> pos_domain::catalog::product::ProductInput {
    let fresh = product_input(rows, variant_ids);
    let has = |c: Column| mapping.contains_key(&c);
    let mut variants = Vec::new();
    for (row_input, id) in fresh.variants.into_iter().zip(variant_ids) {
        let Some(existing) = id.and_then(|id| product.variants.iter().find(|v| v.id == id)) else {
            variants.push(row_input);
            continue;
        };
        let mut v = variant_input(existing);
        v.price = row_input.price;
        if has(Column::Sku) && row_input.sku.is_some() {
            v.sku = row_input.sku;
        }
        if has(Column::Cost) && row_input.cost.is_some() {
            v.cost = row_input.cost;
        }
        if has(Column::TrackStock) {
            v.track_stock = row_input.track_stock;
        }
        if has(Column::LowStockThreshold) && row_input.low_stock_threshold.is_some() {
            v.low_stock_threshold = row_input.low_stock_threshold;
        }
        if has(Column::ScanQuantity) {
            v.scan_qty = row_input.scan_qty;
        }
        // Barcodes from the file are added; existing ones are kept.
        for b in row_input.barcodes {
            if !existing.barcodes.iter().any(|x| x.value == b.raw) {
                v.barcodes.push(BarcodeInput::alias(&b.raw));
            }
        }
        variants.push(v);
    }
    let mentioned: BTreeSet<VariantId> = variant_ids.iter().flatten().copied().collect();
    variants.extend(
        product
            .active_variants()
            .filter(|v| !mentioned.contains(&v.id))
            .map(variant_input),
    );
    pos_domain::catalog::product::ProductInput {
        name: product.name.clone(),
        description: product.description.clone(),
        category_id: if has(Column::Category) && fresh.category_id.is_some() {
            fresh.category_id
        } else {
            product.category_id
        },
        product_type: product.product_type,
        unit: if has(Column::Unit) {
            fresh.unit
        } else {
            product.unit
        },
        tax_category_id: if has(Column::TaxCategory) {
            fresh.tax_category_id
        } else {
            product.tax_category_id
        },
        quick_select: product.quick_select,
        variants,
    }
}

#[cfg(test)]
mod tests {
    use super::csv_field;

    #[test]
    fn report_fields_are_quoted_and_formulas_defused() {
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_field("=HYPERLINK(1)"), "'=HYPERLINK(1)");
        assert_eq!(csv_field("@SUM(A1)"), "'@SUM(A1)");
        assert_eq!(csv_field("+44 113"), "'+44 113");
        assert_eq!(csv_field("-1+2"), "'-1+2");
        // Numbers are values, not formulas.
        assert_eq!(csv_field("-2.00"), "-2.00");
    }
}
