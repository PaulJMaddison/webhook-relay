use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("database error")]
    Database(#[from] sqlx::Error),
    #[error("event not found")]
    NotFound,
    #[error("invalid request: {0}")]
    BadRequest(String),
    #[error("internal error: {0}")]
    Internal(String),
    #[error("replay request failed")]
    ReplayRequest(#[from] reqwest::Error),
}

#[derive(Debug, Serialize)]
struct ErrorBody<'a> {
    error: ErrorDetail<'a>,
}

#[derive(Debug, Serialize)]
struct ErrorDetail<'a> {
    code: &'a str,
    message: String,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, code, msg) = match &self {
            AppError::Database(err) => {
                tracing::error!(error = ?err, "database operation failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "database_error",
                    "A database error occurred".to_owned(),
                )
            }
            AppError::NotFound => (
                StatusCode::NOT_FOUND,
                "not_found",
                "Event not found".to_owned(),
            ),
            AppError::BadRequest(message) => {
                (StatusCode::BAD_REQUEST, "bad_request", message.to_owned())
            }
            AppError::Internal(message) => {
                tracing::error!(message, "internal error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    message.to_owned(),
                )
            }
            AppError::ReplayRequest(err) => {
                tracing::error!(error = ?err, "replay request failed");
                (
                    StatusCode::BAD_GATEWAY,
                    "replay_failed",
                    "Failed to deliver replay request".to_owned(),
                )
            }
        };

        (
            status,
            Json(ErrorBody {
                error: ErrorDetail { code, message: msg },
            }),
        )
            .into_response()
    }
}
