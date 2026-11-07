//! Device-local reports (E14-T01/T02, REP-001..005). Definitions: docs/reporting-definitions.md.

use crate::context::{AppError, CommandContext};
use crate::queries::QueryError;
use pos_domain::settlement::TenderKind;
use pos_domain::time::BusinessDate;
use pos_domain::{ErrorCode, Money, Permission, Quantity, VariantId};
use std::sync::Arc;

/// The longest range a report covers in one go (a year and a day).
pub const MAX_RANGE_DAYS: i64 = 366;

/// One day's (or the whole range's) sales figures.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SalesFigures {
    pub sales_count: u32,
    pub gross: Money,
    pub line_discounts: Money,
    pub order_discounts: Money,
    pub tax: Money,
    pub rounding: Money,
    pub sales_total: Money,
    pub returns_count: u32,
    pub refunds: Money,
    pub refunded_tax: Money,
}

impl SalesFigures {
    pub fn discounts(&self) -> Money {
        Money::from_minor(self.line_discounts.minor() + self.order_discounts.minor())
    }
    /// Sales total − rounding − refunds.
    pub fn net_incl_tax(&self) -> Money {
        Money::from_minor(self.sales_total.minor() - self.rounding.minor() - self.refunds.minor())
    }
    /// Net (incl. tax) − (tax − refunded tax).
    pub fn net_excl_tax(&self) -> Money {
        Money::from_minor(self.net_incl_tax().minor() - (self.tax.minor() - self.refunded_tax.minor()))
    }

    fn add(&mut self, o: &SalesFigures) {
        let m = |a: &mut Money, b: Money| *a = Money::from_minor(a.minor() + b.minor());
        self.sales_count += o.sales_count;
        m(&mut self.gross, o.gross);
        m(&mut self.line_discounts, o.line_discounts);
        m(&mut self.order_discounts, o.order_discounts);
        m(&mut self.tax, o.tax);
        m(&mut self.rounding, o.rounding);
        m(&mut self.sales_total, o.sales_total);
        self.returns_count += o.returns_count;
        m(&mut self.refunds, o.refunds);
        m(&mut self.refunded_tax, o.refunded_tax);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DayFigures {
    pub date: BusinessDate,
    pub figures: SalesFigures,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SalesSummary {
    /// Days with any sale or return, oldest first.
    pub days: Vec<DayFigures>,
    pub total: SalesFigures,
    /// Whether the business sells with prices including tax (gross is then tax-inclusive).
    pub prices_include_tax: bool,
    pub currency_exponent: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TenderMixRow {
    pub kind: TenderKind,
    /// `operator` or `provider`.
    pub confirmation_source: String,
    pub count: u32,
    pub taken: Money,
    pub refund_count: u32,
    pub refunded: Money,
}

impl TenderMixRow {
    pub fn net(&self) -> Money {
        Money::from_minor(self.taken.minor() - self.refunded.minor())
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TopBy {
    Quantity,
    Value,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TopProductRow {
    pub variant_id: VariantId,
    pub name: String,
    pub label: String,
    pub quantity: Quantity,
    pub value: Money,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum StockStatus {
    Ok,
    Low,
    Negative,
}

impl StockStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            StockStatus::Ok => "ok",
            StockStatus::Low => "low",
            StockStatus::Negative => "negative",
        }
    }

    /// docs/reporting-definitions.md, "Stock".
    pub fn of(sellable: Quantity, threshold: Option<Quantity>) -> Self {
        if sellable.milli() < 0 {
            StockStatus::Negative
        } else if threshold.is_some_and(|t| sellable.milli() <= t.milli()) {
            StockStatus::Low
        } else {
            StockStatus::Ok
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StockRow {
    pub variant_id: VariantId,
    pub name: String,
    pub label: String,
    pub sku: Option<String>,
    pub unit: pos_domain::catalog::Unit,
    pub sellable: Quantity,
    pub damaged: Quantity,
    pub low_stock_threshold: Option<Quantity>,
    pub status: StockStatus,
    pub last_movement_at: Option<pos_domain::Timestamp>,
}

#[async_trait::async_trait]
pub trait ReportQueries: Send + Sync {
    /// Per business day in `from..=to`, live sales and returns only.
    async fn daily_figures(
        &self,
        from: BusinessDate,
        to: BusinessDate,
    ) -> Result<Vec<DayFigures>, QueryError>;
    async fn tender_mix(&self, from: BusinessDate, to: BusinessDate)
    -> Result<Vec<TenderMixRow>, QueryError>;
    async fn top_products(
        &self,
        from: BusinessDate,
        to: BusinessDate,
        by: TopBy,
        limit: u32,
    ) -> Result<Vec<TopProductRow>, QueryError>;
    /// Every stock-tracked, active variant with its balances.
    async fn stock_rows(&self) -> Result<Vec<StockRow>, QueryError>;
    /// (prices include tax, currency exponent)
    async fn report_settings(&self) -> Result<(bool, u8), QueryError>;
}

pub struct ReportService {
    pub reports: Arc<dyn ReportQueries>,
}

fn check_range(from: BusinessDate, to: BusinessDate) -> Result<(), AppError> {
    let days = (to.as_naive() - from.as_naive()).num_days();
    if days < 0 {
        return Err(AppError::Validation(
            "the start date is after the end date".into(),
            ErrorCode::ValidationFailed,
        ));
    }
    if days >= MAX_RANGE_DAYS {
        return Err(AppError::Validation(
            "a report covers at most a year".into(),
            ErrorCode::ValidationFailed,
        ));
    }
    Ok(())
}

impl ReportService {
    pub async fn sales_summary(
        &self,
        ctx: &CommandContext,
        from: BusinessDate,
        to: BusinessDate,
    ) -> Result<SalesSummary, AppError> {
        ctx.authorize(Permission::ReportsView)?;
        check_range(from, to)?;
        let days = self.reports.daily_figures(from, to).await?;
        let mut total = SalesFigures::default();
        for d in &days {
            total.add(&d.figures);
        }
        let (prices_include_tax, currency_exponent) = self.reports.report_settings().await?;
        Ok(SalesSummary {
            days,
            total,
            prices_include_tax,
            currency_exponent,
        })
    }

    pub async fn tender_mix(
        &self,
        ctx: &CommandContext,
        from: BusinessDate,
        to: BusinessDate,
    ) -> Result<Vec<TenderMixRow>, AppError> {
        ctx.authorize(Permission::ReportsView)?;
        check_range(from, to)?;
        Ok(self.reports.tender_mix(from, to).await?)
    }

    pub async fn top_products(
        &self,
        ctx: &CommandContext,
        from: BusinessDate,
        to: BusinessDate,
        by: TopBy,
        limit: u32,
    ) -> Result<Vec<TopProductRow>, AppError> {
        ctx.authorize(Permission::ReportsView)?;
        check_range(from, to)?;
        Ok(self
            .reports
            .top_products(from, to, by, limit.clamp(1, 100))
            .await?)
    }

    /// Current stock; `low_only` keeps the Low and Negative rows (the low-stock report).
    pub async fn stock(&self, ctx: &CommandContext, low_only: bool) -> Result<Vec<StockRow>, AppError> {
        ctx.authorize(Permission::ReportsView)?;
        let rows = self.reports.stock_rows().await?;
        Ok(if low_only {
            rows.into_iter().filter(|r| r.status != StockStatus::Ok).collect()
        } else {
            rows
        })
    }
}
