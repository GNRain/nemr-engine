//! The API error type and its HTTP rendering.
//!
//! Errors that reach a client are deliberately terse: authentication failures
//! never say whether the email or the password was the problem, so the response
//! cannot be used to enumerate accounts.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

pub type ApiResult<T> = Result<T, ApiError>;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("{0}")]
    BadRequest(String),
    /// Authentication failed. The message is intentionally uniform.
    #[error("invalid credentials")]
    Unauthorized,
    #[error("{0}")]
    Forbidden(String),
    #[error("{0}")]
    NotFound(String),
    /// A precondition failed — a name already taken, or a lost lease.
    #[error("{0}")]
    Conflict(String),

    /// F-16: the bundle is larger than this server accepts. A product ceiling,
    /// answered as 413 with the limit named, so the client can say why.
    #[error("{0}")]
    PayloadTooLarge(String),
    #[error("too many attempts; try again later")]
    TooManyRequests,
    /// Anything the client cannot act on. The detail is logged, never returned.
    #[error("internal error")]
    Internal(#[from] anyhow::Error),
}

impl ApiError {
    fn status(&self) -> StatusCode {
        match self {
            ApiError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ApiError::Unauthorized => StatusCode::UNAUTHORIZED,
            ApiError::Forbidden(_) => StatusCode::FORBIDDEN,
            ApiError::NotFound(_) => StatusCode::NOT_FOUND,
            ApiError::Conflict(_) => StatusCode::CONFLICT,
            ApiError::PayloadTooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
            ApiError::TooManyRequests => StatusCode::TOO_MANY_REQUESTS,
            ApiError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        // A database error is never actionable by the client; log it and return
        // a generic 500. Handlers that want to turn a specific constraint into a
        // 409 do so before this blanket conversion.
        ApiError::Internal(anyhow::Error::from(e).context("database"))
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        if let ApiError::Internal(ref e) = self {
            tracing::error!(error = format!("{e:#}"), "request failed");
        }
        let body = Json(json!({ "error": self.to_string() }));
        (self.status(), body).into_response()
    }
}
