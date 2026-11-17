//! Sales history and sale detail (E12-T03). Reads only: a completed sale never changes.

use crate::context::{AppError, CommandContext};
use crate::queries::{SalesFilter, SalesQueries};
use crate::records::{SaleDetail, SalesCursor, SalesPage};
use pos_domain::{ErrorCode, Permission, SaleId};
use std::sync::Arc;

/// Rows per page when the caller does not say, and the most it may ask for.
pub const DEFAULT_PAGE: u32 = 50;
pub const MAX_PAGE: u32 = 200;

pub struct SalesService {
    pub sales: Arc<dyn SalesQueries>,
}

impl SalesService {
    pub async fn list(
        &self,
        ctx: &CommandContext,
        filter: &SalesFilter,
        after: Option<&str>,
        limit: Option<u32>,
    ) -> Result<SalesPage, AppError> {
        ctx.authorize(Permission::SalesView)?;
        if let (Some(from), Some(to)) = (filter.from, filter.to)
            && from > to
        {
            return Err(AppError::Validation(
                "the start date is after the end date".into(),
                ErrorCode::ValidationFailed,
            ));
        }
        let after = match after {
            Some(s) => Some(SalesCursor::decode(s).ok_or_else(|| {
                AppError::Validation("that page token is not valid".into(), ErrorCode::ValidationFailed)
            })?),
            None => None,
        };
        let limit = limit.unwrap_or(DEFAULT_PAGE).clamp(1, MAX_PAGE);
        Ok(self.sales.list_sales(filter, after.as_ref(), limit).await?)
    }

    pub async fn get(&self, ctx: &CommandContext, sale: SaleId) -> Result<SaleDetail, AppError> {
        ctx.authorize(Permission::SalesView)?;
        self.sales.sale_detail(sale).await?.ok_or_else(|| {
            AppError::Validation("no such sale on this device".into(), ErrorCode::ValidationFailed)
        })
    }
}
