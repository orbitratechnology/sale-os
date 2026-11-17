//! The catalogue in the cloud. Edits (E17-T06, `POST /v1/catalog/products`) go through the
//! same version check, uniqueness rules and change feed as a register's `product.upserted`,
//! with the cloud as their origin, so registers pull them like any other change. The list
//! (E16-T06, `GET /v1/catalog/products`) is read-only for the owner in the MVP.

use crate::auth::UserPrincipal;
use crate::devices::{internal, pool};
use crate::extract::{ApiJson, ApiQuery};
use crate::sync::master::{Origin, upsert};
use crate::sync::{ApplyError, ApplyOutcome};
use crate::{AppState, Problem};
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use pos_cloud_db::devices::is_member;
use pos_cloud_db::master::list_products;
use pos_cloud_db::{Principal, TenantTx};
use pos_domain::{BusinessId, ErrorCode};
use pos_sync::{Payload, codes};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub fn routes() -> Router<AppState> {
    Router::new().route("/catalog/products", post(save_product).get(products))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SaveProduct {
    business_id: Uuid,
    /// The whole product, as in `product.upserted` v1, with `version = expectedVersion + 1`.
    product: serde_json::Value,
    /// The version the edit was based on (0 to create).
    expected_version: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SavedProduct {
    product_id: Uuid,
    version: u64,
    product: serde_json::Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProductsQuery {
    business_id: Uuid,
    #[serde(default)]
    query: String,
    /// 1-based.
    page: Option<i64>,
    page_size: Option<i64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProductPage {
    items: Vec<SavedProduct>,
    page: i64,
    page_size: i64,
    /// Products matching the query, across all pages.
    total: i64,
}

const PAGE_SIZE_DEFAULT: i64 = 50;
const PAGE_SIZE_MAX: i64 = 200;

fn not_found() -> Problem {
    // Someone else's business, or none: the same answer.
    Problem::new(StatusCode::NOT_FOUND, ErrorCode::NotFound, "Not found", None)
}

async fn products(
    State(state): State<AppState>,
    user: UserPrincipal,
    ApiQuery(q): ApiQuery<ProductsQuery>,
) -> Result<Json<ProductPage>, Problem> {
    let page = q.page.unwrap_or(1);
    let page_size = q.page_size.unwrap_or(PAGE_SIZE_DEFAULT);
    if page < 1 || !(1..=PAGE_SIZE_MAX).contains(&page_size) {
        return Err(Problem::validation(format!(
            "page starts at 1 and pageSize is 1 to {PAGE_SIZE_MAX}"
        )));
    }
    let query = q.query.trim();
    if query.chars().count() > 200 {
        return Err(Problem::validation("query is longer than 200 characters"));
    }
    let mut tx = TenantTx::begin(
        pool(&state)?,
        &Principal {
            business_id: BusinessId::from_uuid(q.business_id),
        },
    )
    .await
    .map_err(internal)?;
    if !is_member(&mut tx, user.account_id).await.map_err(internal)? {
        return Err(not_found());
    }
    let offset = (page - 1).saturating_mul(page_size);
    let (rows, total) = list_products(&mut tx, query, page_size, offset)
        .await
        .map_err(internal)?;
    tx.commit().await.map_err(internal)?;
    Ok(Json(ProductPage {
        items: rows
            .into_iter()
            .map(|r| SavedProduct {
                product_id: r.product_id,
                version: u64::try_from(r.version).unwrap_or(0),
                product: r.snapshot,
            })
            .collect(),
        page,
        page_size,
        total,
    }))
}

fn conflict(code: ErrorCode, title: &str) -> Problem {
    Problem::new(StatusCode::CONFLICT, code, title, None)
}

async fn save_product(
    State(state): State<AppState>,
    user: UserPrincipal,
    ApiJson(req): ApiJson<SaveProduct>,
) -> Result<Json<SavedProduct>, Problem> {
    let payload = pos_sync::parse_payload("product.upserted", 1, &req.product)
        .map_err(|e| Problem::validation(e.to_string()))?;
    let Payload::ProductUpsertedV1(product) = &payload else {
        return Err(Problem::validation("not a product"));
    };
    let pool = pool(&state)?;
    let mut tx = TenantTx::begin(
        pool,
        &Principal {
            business_id: BusinessId::from_uuid(req.business_id),
        },
    )
    .await
    .map_err(internal)?;
    if !is_member(&mut tx, user.account_id).await.map_err(internal)? {
        return Err(not_found());
    }
    let id = product.id.to_string();
    let origin = Origin {
        register: None,
        event: None,
    };
    match upsert(
        &mut tx,
        origin,
        "product",
        &id,
        req.expected_version,
        &payload,
        &req.product,
    )
    .await
    {
        Ok(ApplyOutcome::Applied { new_version, .. }) => {
            tx.commit().await.map_err(internal)?;
            Ok(Json(SavedProduct {
                product_id: product.id,
                version: new_version.unwrap_or(product.version),
                product: req.product,
            }))
        }
        Ok(ApplyOutcome::Conflict { reason, .. }) => Err(match reason {
            codes::BARCODE_TAKEN => conflict(ErrorCode::CatalogDuplicateBarcode, "Barcode already in use"),
            codes::SKU_TAKEN => conflict(ErrorCode::CatalogDuplicateSku, "SKU already in use"),
            _ => conflict(ErrorCode::ConflictVersion, "Changed since it was loaded"),
        }),
        Err(ApplyError::Rejected { detail, .. }) => Err(Problem::validation(detail)),
        Err(ApplyError::Transient { detail, .. }) => Err(Problem::new(
            StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::Internal,
            "Try again",
            Some(detail),
        )),
    }
}
