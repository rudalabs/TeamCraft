//! Application error type shared by services, the JSON API and the web UI.
//!
//! [`AppError`] renders as a JSON error body. The web UI router installs a
//! middleware that picks up the [`ErrorInfo`] response extension and replaces
//! the body with an HTML error page (or a redirect to the login page).

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Forbidden(String),
    #[error("Authentication required.")]
    Unauthorized,
    #[error("{0}")]
    PayloadTooLarge(String),
    /// An external service the request depends on failed or is busy
    /// (`503`); the message is shown to the user, nothing is reported.
    #[error("{0}")]
    Unavailable(String),
    /// A brake (`ratelimit`) refused the request (`429`).
    #[error("{0}")]
    TooManyRequests(String),
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("template error: {0}")]
    Template(#[from] minijinja::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type AppResult<T> = Result<T, AppError>;

/// Error details attached to error responses as a response extension.
#[derive(Clone, Debug)]
pub struct ErrorInfo {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}

impl AppError {
    pub fn not_found(what: impl Into<String>) -> Self {
        AppError::NotFound(what.into())
    }

    pub fn status(&self) -> StatusCode {
        match self {
            AppError::NotFound(_) => StatusCode::NOT_FOUND,
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AppError::Conflict(_) => StatusCode::CONFLICT,
            AppError::Forbidden(_) => StatusCode::FORBIDDEN,
            AppError::Unauthorized => StatusCode::UNAUTHORIZED,
            AppError::PayloadTooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
            AppError::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            AppError::TooManyRequests(_) => StatusCode::TOO_MANY_REQUESTS,
            AppError::Database(sqlx::Error::RowNotFound) => StatusCode::NOT_FOUND,
            AppError::Database(_)
            | AppError::Io(_)
            | AppError::Template(_)
            | AppError::Other(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            AppError::NotFound(_) => "not_found",
            AppError::BadRequest(_) => "bad_request",
            AppError::Conflict(_) => "conflict",
            AppError::Forbidden(_) => "forbidden",
            AppError::Unauthorized => "unauthorized",
            AppError::PayloadTooLarge(_) => "payload_too_large",
            AppError::Unavailable(_) => "unavailable",
            AppError::TooManyRequests(_) => "rate_limited",
            AppError::Database(sqlx::Error::RowNotFound) => "not_found",
            AppError::Database(_)
            | AppError::Io(_)
            | AppError::Template(_)
            | AppError::Other(_) => "internal",
        }
    }

    /// Message that is safe to show to end users (internal details are hidden).
    pub fn public_message(&self) -> String {
        match self {
            AppError::Database(sqlx::Error::RowNotFound) => "Not found.".to_string(),
            AppError::Database(_)
            | AppError::Io(_)
            | AppError::Template(_)
            | AppError::Other(_) => {
                "Internal server error. The incident has been logged.".to_string()
            }
            other => other.to_string(),
        }
    }

    /// Maps unique-constraint violations to a friendly conflict message.
    pub fn unique_violation(err: sqlx::Error, message: impl Into<String>) -> Self {
        if let sqlx::Error::Database(db) = &err {
            if db.is_unique_violation() {
                return AppError::Conflict(message.into());
            }
        }
        AppError::Database(err)
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status();
        if status.is_server_error() && !matches!(self, AppError::Unavailable(_)) {
            tracing::error!(error = %self, "request failed");
            crate::notify::report_error(&self.to_string());
        }
        let info = ErrorInfo {
            status,
            code: self.code(),
            message: self.public_message(),
        };
        let body = json!({ "error": { "code": info.code, "message": info.message } });
        let mut response = (status, Json(body)).into_response();
        response.extensions_mut().insert(info);
        response
    }
}
