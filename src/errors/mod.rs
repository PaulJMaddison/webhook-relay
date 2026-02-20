use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone)]
pub enum AppError {
    Validation {
        message: String,
        details: Option<Value>,
    },
    Auth {
        message: String,
        details: Option<Value>,
    },
    NotFound {
        message: String,
        details: Option<Value>,
    },
    Db {
        message: String,
        details: Option<Value>,
    },
    Upstream {
        message: String,
        details: Option<Value>,
    },
    Internal {
        message: String,
        details: Option<Value>,
    },
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    code: &'static str,
    message: String,
    details: Option<Value>,
}

impl AppError {
    pub fn validation(message: impl Into<String>) -> Self {
        Self::Validation {
            message: message.into(),
            details: None,
        }
    }

    pub fn auth(message: impl Into<String>) -> Self {
        Self::Auth {
            message: message.into(),
            details: None,
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::NotFound {
            message: message.into(),
            details: None,
        }
    }

    pub fn db(message: impl Into<String>) -> Self {
        Self::Db {
            message: message.into(),
            details: None,
        }
    }

    pub fn upstream(message: impl Into<String>) -> Self {
        Self::Upstream {
            message: message.into(),
            details: None,
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal {
            message: message.into(),
            details: None,
        }
    }

    pub fn with_details(self, details: Value) -> Self {
        match self {
            Self::Validation { message, .. } => Self::Validation {
                message,
                details: Some(details),
            },
            Self::Auth { message, .. } => Self::Auth {
                message,
                details: Some(details),
            },
            Self::NotFound { message, .. } => Self::NotFound {
                message,
                details: Some(details),
            },
            Self::Db { message, .. } => Self::Db {
                message,
                details: Some(details),
            },
            Self::Upstream { message, .. } => Self::Upstream {
                message,
                details: Some(details),
            },
            Self::Internal { message, .. } => Self::Internal {
                message,
                details: Some(details),
            },
        }
    }

    fn status_code(&self) -> StatusCode {
        match self {
            Self::Validation { .. } => StatusCode::BAD_REQUEST,
            Self::Auth { .. } => StatusCode::UNAUTHORIZED,
            Self::NotFound { .. } => StatusCode::NOT_FOUND,
            Self::Db { .. } => StatusCode::SERVICE_UNAVAILABLE,
            Self::Upstream { .. } => StatusCode::BAD_GATEWAY,
            Self::Internal { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Self::Validation { .. } => "validation",
            Self::Auth { .. } => "auth",
            Self::NotFound { .. } => "not_found",
            Self::Db { .. } => "db",
            Self::Upstream { .. } => "upstream",
            Self::Internal { .. } => "internal",
        }
    }

    fn message_and_details(&self) -> (&String, &Option<Value>) {
        match self {
            Self::Validation { message, details }
            | Self::Auth { message, details }
            | Self::NotFound { message, details }
            | Self::Db { message, details }
            | Self::Upstream { message, details }
            | Self::Internal { message, details } => (message, details),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let code = self.code();
        let (message, details) = self.message_and_details();

        (
            status,
            Json(ErrorResponse {
                code,
                message: message.clone(),
                details: details.clone(),
            }),
        )
            .into_response()
    }
}
