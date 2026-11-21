//! Catalogue management (E06-T02, CAT-001..007).

use crate::audit::{self, AuditEntry};
use crate::context::{AppError, CommandContext};
use crate::queries::{CatalogHit, CatalogQueries, ProductFilter, ProductPage, TaxCategorySummary};
use crate::{LocalStore, OutboxDraft, WriteSet};
use pos_domain::catalog::{CatalogError, Product, ProductInput};
use pos_domain::{Permission, ProductId};
use std::sync::Arc;

pub struct CatalogService {
    pub store: Arc<dyn LocalStore>,
    pub catalog: Arc<dyn CatalogQueries>,
}

/// Most search results returned at once.
pub const MAX_SEARCH_RESULTS: u32 = 100;
/// Most product list rows returned at once.
pub const MAX_PAGE: u32 = 500;

impl CatalogService {
    pub async fn get(&self, ctx: &CommandContext, id: ProductId) -> Result<Product, AppError> {
        ctx.authorize(Permission::CatalogView)?;
        Ok(self.catalog.product(id).await?.ok_or(CatalogError::NotFound)?)
    }

    /// Barcode, then SKU (BAR-004). `None` when nothing matches.
    pub async fn lookup_code(
        &self,
        ctx: &CommandContext,
        code: &str,
    ) -> Result<Option<CatalogHit>, AppError> {
        ctx.authorize(Permission::CatalogView)?;
        Ok(self.catalog.lookup_code(code).await?)
    }

    pub async fn search(
        &self,
        ctx: &CommandContext,
        query: &str,
        limit: u32,
    ) -> Result<Vec<CatalogHit>, AppError> {
        ctx.authorize(Permission::CatalogView)?;
        Ok(self
            .catalog
            .search(query, limit.clamp(1, MAX_SEARCH_RESULTS))
            .await?)
    }

    /// The quick-select grid (E09-T04, POS-002): at most 48 products per page.
    pub async fn quick_select(
        &self,
        ctx: &CommandContext,
        category: Option<pos_domain::CategoryId>,
        offset: u32,
        limit: u32,
    ) -> Result<crate::queries::QuickSelectPage, AppError> {
        ctx.authorize(Permission::CatalogView)?;
        Ok(self
            .catalog
            .quick_select(category, offset, limit.clamp(1, 48))
            .await?)
    }

    /// The product list (E06-T06a). At most [`MAX_PAGE`] rows per call.
    pub async fn list(
        &self,
        ctx: &CommandContext,
        filter: &ProductFilter,
        offset: u32,
        limit: u32,
    ) -> Result<ProductPage, AppError> {
        ctx.authorize(Permission::CatalogView)?;
        Ok(self
            .catalog
            .list_products(filter, offset, limit.clamp(1, MAX_PAGE))
            .await?)
    }

    /// What a new product starts as: the editor's blank product and CSV rows without a unit.
    pub async fn new_product_defaults(
        &self,
        ctx: &CommandContext,
    ) -> Result<crate::queries::NewProductDefaults, AppError> {
        ctx.authorize(Permission::CatalogView)?;
        Ok(self.catalog.new_product_defaults().await?)
    }

    pub async fn tax_categories(&self, ctx: &CommandContext) -> Result<Vec<TaxCategorySummary>, AppError> {
        ctx.authorize(Permission::CatalogView)?;
        Ok(self.catalog.tax_categories().await?)
    }

    pub async fn create(&self, ctx: &CommandContext, input: ProductInput) -> Result<Product, AppError> {
        ctx.authorize(Permission::CatalogEdit)?;
        let product = Product::create(input, ctx.ids.as_ref())?;
        self.ensure_unique_across_products(&product).await?;
        self.save(ctx, product.clone(), None, audit::CATALOG_PRODUCT_CREATED)
            .await?;
        Ok(product)
    }

    /// E07-T04 (BAR-006): a product created at the till for a barcode nobody knew. Managers
    /// and owners may; a cashier needs a manager's approval for exactly this barcode, and the
    /// product must carry it. The audit entry names the approver.
    pub async fn create_from_scan(
        &self,
        ctx: &CommandContext,
        input: ProductInput,
        barcode: &str,
    ) -> Result<Product, AppError> {
        ctx.authorize(Permission::CatalogView)?;
        let approver = if pos_domain::role_has(ctx.actor()?.role, Permission::CatalogEdit) {
            None
        } else {
            ctx.authorize_with_approval(Permission::BarcodeCreateFromScan, barcode.trim(), None)?
        };
        let carries = input
            .variants
            .iter()
            .flat_map(|v| &v.barcodes)
            .any(|b| b.raw.trim() == barcode.trim());
        if !carries {
            return Err(AppError::Validation(
                "the product must carry the scanned barcode".into(),
                pos_domain::ErrorCode::ValidationFailed,
            ));
        }
        let product = Product::create(input, ctx.ids.as_ref())?;
        self.ensure_unique_across_products(&product).await?;
        self.save_approved(
            ctx,
            product.clone(),
            None,
            audit::CATALOG_PRODUCT_CREATED,
            approver,
        )
        .await?;
        Ok(product)
    }

    /// Replaces the product's state. `base_version` is the version the edit started from.
    pub async fn update(
        &self,
        ctx: &CommandContext,
        id: ProductId,
        base_version: u64,
        input: ProductInput,
    ) -> Result<Product, AppError> {
        ctx.authorize(Permission::CatalogEdit)?;
        let mut product = self.catalog.product(id).await?.ok_or(CatalogError::NotFound)?;
        // Edit from the version the user saw; the store rejects it if someone changed it since.
        product.version = base_version;
        product.edit(input, ctx.ids.as_ref())?;
        self.ensure_unique_across_products(&product).await?;
        self.save(
            ctx,
            product.clone(),
            Some(base_version),
            audit::CATALOG_PRODUCT_UPDATED,
        )
        .await?;
        Ok(product)
    }

    /// Archives the product: hidden from search and lookup, kept for history (CAT-001).
    pub async fn archive(
        &self,
        ctx: &CommandContext,
        id: ProductId,
        base_version: u64,
    ) -> Result<Product, AppError> {
        ctx.authorize(Permission::CatalogEdit)?;
        let mut product = self.catalog.product(id).await?.ok_or(CatalogError::NotFound)?;
        product.version = base_version;
        product.archive();
        self.save(
            ctx,
            product.clone(),
            Some(base_version),
            audit::CATALOG_PRODUCT_ARCHIVED,
        )
        .await?;
        Ok(product)
    }

    /// CAT-004: a barcode or SKU may belong to only one active product.
    async fn ensure_unique_across_products(&self, p: &Product) -> Result<(), AppError> {
        for v in p.active_variants() {
            for b in &v.barcodes {
                if let Some(owner) = self.catalog.barcode_owner(&b.value).await?
                    && owner != p.id
                {
                    return Err(CatalogError::BarcodeTaken {
                        value: b.value.clone(),
                        owner,
                    }
                    .into());
                }
            }
            if let Some(sku) = &v.sku
                && let Some(owner) = self.catalog.sku_owner(sku).await?
                && owner != p.id
            {
                return Err(CatalogError::SkuTaken {
                    sku: sku.clone(),
                    owner,
                }
                .into());
            }
        }
        Ok(())
    }

    async fn save(
        &self,
        ctx: &CommandContext,
        product: Product,
        expected: Option<u64>,
        action: audit::AuditAction,
    ) -> Result<(), AppError> {
        self.save_approved(ctx, product, expected, action, None).await
    }

    async fn save_approved(
        &self,
        ctx: &CommandContext,
        product: Product,
        expected: Option<u64>,
        action: audit::AuditAction,
        approver: Option<pos_domain::UserId>,
    ) -> Result<(), AppError> {
        let ids = ctx.ids.as_ref();
        let event = OutboxDraft::product_upserted(ids, &product)?;
        let mut entry = AuditEntry::new(ids, action, "product", product.id)
            .with_metadata(serde_json::json!({ "name": product.name, "version": product.version }))
            .linked_to(event.event_id);
        if let Some(a) = approver {
            entry = entry.with_approver(a);
        }
        let set = WriteSet::builder(ctx.write_context())
            .put_product(product, expected)
            .outbox(event)
            .audit(entry)
            .build()?;
        self.store.apply(set).await?;
        Ok(())
    }
}
