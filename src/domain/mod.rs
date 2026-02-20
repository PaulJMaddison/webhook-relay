use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct StoredEvent {
    pub id: Uuid,
    pub source: String,
    pub method: String,
    pub path: String,
    pub query: Option<String>,
    pub headers: serde_json::Value,
    pub body: Vec<u8>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewEvent {
    pub source: String,
    pub method: String,
    pub path: String,
    pub query: Option<String>,
    pub headers: serde_json::Value,
    pub body: Vec<u8>,
}

#[derive(Debug, Deserialize)]
pub struct ReplayRequest {
    pub target_url: String,
}

#[derive(Debug, Serialize)]
pub struct ReplayResponse {
    pub status_code: u16,
}

#[derive(Debug, Serialize)]
pub struct IngestResponse {
    pub id: Uuid,
}
