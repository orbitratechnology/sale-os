//! RFC 9457 problem+json errors carrying a catalogued `code` (docs/backlog/07-contracts.md §2).

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use pos_domain::ErrorCode;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct Problem {
    #[serde(rename = "type")]
    pub type_uri: String,
    pub title: String,
    pub status: u16,
    pub code: ErrorCode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl Problem {
    pub fn new(status: StatusCode, code: ErrorCode, title: &str, detail: Option<String>) -> Self {
        Self {
            type_uri: format!("https://errors.sale-os.dev/{}", code.as_str()),
            title: title.to_owned(),
            status: status.as_u16(),
            code,
            detail,
        }
    }

    pub fn validation(detail: impl Into<String>) -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            ErrorCode::ValidationFailed,
            "Validation failed",
            Some(detail.into()),
        )
    }
}

impl IntoResponse for Problem {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let mut res = (status, axum::Json(self)).into_response();
        res.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/problem+json"),
        );
        res
    }
}
