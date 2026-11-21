//! Request extractors that refuse bad input as problem+json (E16-T06). axum's own `Json`,
//! `Query` and `Path` answer a missing content type, malformed JSON or a wrong field type in
//! `text/plain`; every error this API returns is a problem with a catalogued code
//! (07-contracts §2), so these wrap them and keep axum's status (400, 415 or 422) and message.

use crate::Problem;
use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::extract::{FromRequest, FromRequestParts, Request};
use axum::http::StatusCode;
use axum::http::request::Parts;
use pos_domain::ErrorCode;
use serde::de::DeserializeOwned;

fn refused(status: StatusCode, detail: String) -> Problem {
    let title = match status {
        StatusCode::UNSUPPORTED_MEDIA_TYPE => "Send JSON",
        StatusCode::PAYLOAD_TOO_LARGE => "Request too large",
        _ => "Validation failed",
    };
    Problem::new(status, ErrorCode::ValidationFailed, title, Some(detail))
}

/// A JSON body.
pub struct ApiJson<T>(pub T);

impl<S, T> FromRequest<S> for ApiJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = Problem;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        axum::Json::<T>::from_request(req, state)
            .await
            .map(|axum::Json(v)| Self(v))
            .map_err(|e: JsonRejection| refused(e.status(), e.body_text()))
    }
}

/// Query parameters.
pub struct ApiQuery<T>(pub T);

impl<S, T> FromRequestParts<S> for ApiQuery<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = Problem;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        axum::extract::Query::<T>::from_request_parts(parts, state)
            .await
            .map(|axum::extract::Query(v)| Self(v))
            // A query that does not parse is invalid input, as a body would be.
            .map_err(|e: QueryRejection| refused(StatusCode::UNPROCESSABLE_ENTITY, e.body_text()))
    }
}

/// Path parameters.
pub struct ApiPath<T>(pub T);

impl<S, T> FromRequestParts<S> for ApiPath<T>
where
    S: Send + Sync,
    T: DeserializeOwned + Send,
{
    type Rejection = Problem;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        axum::extract::Path::<T>::from_request_parts(parts, state)
            .await
            .map(|axum::extract::Path(v)| Self(v))
            .map_err(|e: PathRejection| refused(StatusCode::UNPROCESSABLE_ENTITY, e.body_text()))
    }
}
