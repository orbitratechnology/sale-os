//! Exchanges: a return and a new sale in one transaction (E12-T02 follow-up).
//!
//! The customer brings items back and takes others. Both records stay what they are: a
//! return against the original sale's lines (RET-003, RET-004) and a new sale of the cart.
//! Neither is rewritten later (RET-002).
//!
//! The returned value pays for the new items. It is recorded with the tenders the till already
//! has: the return refunds it as cash and the sale takes the same amount as cash, so the
//! drawer is unchanged by it. Only the difference moves:
//!
//! - the new items cost more: the customer pays the rest (`tenders`, any kind);
//! - they cost less: the rest comes back as cash change, or as the `refunds` given (any kind).
//!
//! The payment mix and Z summary therefore show the exchanged value as a cash refund and a
//! cash payment of the same amount.
//!
//! One write set holds both: the return, its refund receipt, its stock movements and
//! `return.completed`; the sale, its receipt, its movements, the closed cart and
//! `sale.completed`. Either both are recorded or neither is, and nothing is written until
//! the cashier confirms.

use crate::WriteSet;
use crate::checkout::{CheckoutService, CompletedSale};
use crate::context::{AppError, CommandContext};
use crate::returns::{CompletedReturn, CreateReturn, ReturnService};
use pos_domain::pricing::PricingPolicy;
use pos_domain::returns::settle_refunds;
use pos_domain::settlement::{TenderInput, TenderKind, settle};
use pos_domain::{CartId, ErrorCode, Money};

/// `exchange_complete`'s input.
#[derive(Clone, Debug)]
pub struct CreateExchange {
    /// What comes back. `ret.refunds`: the part of the returned value paid back rather than
    /// spent on the new items (empty: all of it is spent, and any rest is cash change).
    pub ret: CreateReturn,
    /// The cart with the new items, at the revision the cashier saw.
    pub cart_id: CartId,
    pub revision: u64,
    /// What the customer pays on top of the returned value (empty when it covers the cart).
    pub tenders: Vec<TenderInput>,
}

#[derive(Clone, Debug)]
pub struct CompletedExchange {
    pub ret: CompletedReturn,
    pub sale: CompletedSale,
}

fn invalid(msg: &str) -> AppError {
    AppError::Validation(msg.into(), ErrorCode::ValidationFailed)
}

/// Records the return and the new sale together. See the module documentation.
pub async fn complete(
    returns: &ReturnService,
    checkout: &CheckoutService,
    ctx: &CommandContext,
    input: CreateExchange,
) -> Result<CompletedExchange, AppError> {
    checkout.authorize_tenders(ctx, &input.tenders).await?;
    let planned = returns.plan(ctx, &input.ret).await?;
    let returned = planned.plan.total;
    let paid_back = input
        .ret
        .refunds
        .iter()
        .try_fold(Money::ZERO, |a, r| a.checked_add(r.amount))
        .map_err(|_| invalid("the refund amounts are too large"))?;
    // The returned value spent on the new items.
    let credit = returned
        .checked_sub(paid_back)
        .map_err(|_| invalid("the refund amounts are too large"))?;
    if credit.minor() <= 0 {
        return Err(invalid(
            "nothing of the returned value is left for the new items; make a return instead",
        ));
    }
    let credit_tender = TenderInput {
        kind: TenderKind::Cash,
        amount: credit,
        external_reference: None,
    };

    // Hold the cart lock: no scan or edit can slip in between pricing and commit.
    let _guard = checkout.carts.exclusive().await;
    let view = checkout.priced_cart(ctx, input.cart_id, input.revision).await?;
    if !input.tenders.is_empty() && credit >= view.priced.total {
        return Err(invalid(
            "the returned value already covers the new items; there is nothing more to pay",
        ));
    }
    let policy = PricingPolicy::new(view.settings.tax_mode);
    // The credit goes first, so a customer's cash on top gives the change.
    let mut tenders = vec![credit_tender.clone()];
    tenders.extend(input.tenders.iter().cloned());
    let settlement = settle(view.priced.total, &tenders, &policy)?;

    let mut refund_inputs = vec![credit_tender];
    refund_inputs.extend(input.ret.refunds.iter().cloned());
    let refunds = settle_refunds(returned, &refund_inputs)?;

    // Each record names the other in its audit row.
    let return_id = planned.id;
    let set = WriteSet::builder(ctx.write_context());
    let (set, staged) = checkout
        .stage_sale(ctx, view, settlement, set, Some(return_id), &[])
        .await?;
    let sale_id = staged.sale.id;
    let (set, ret) = returns
        .stage(ctx, planned, input.ret, refunds, set, Some(sale_id))
        .await?;
    let applied = checkout.store.apply(set.build()?).await?;
    let return_number = applied
        .receipt_numbers
        .get(&crate::returns::RETURN_NUMBER)
        .cloned()
        .ok_or_else(|| AppError::Internal("the store did not report the return number".into()))?;
    // The sale's event comes first in the write set, the return's second.
    let sale = staged.completed(&applied, 0)?;
    Ok(CompletedExchange {
        ret: CompletedReturn { ret, return_number },
        sale,
    })
}
