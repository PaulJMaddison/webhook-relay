use std::{collections::HashMap, sync::Arc};

use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode, Uri},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::Serialize;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::errors::AppError;

pub fn router(pool: PgPool) -> Router {
    let state = AppState {
        store: Arc::new(PgEventStore { pool }),
    };

    router_with_state(state)
}

fn router_with_state(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/hooks/:source", post(ingest_hook))
        .with_state(state)
}

#[derive(Clone)]
struct AppState {
    store: Arc<dyn EventStore>,
}

#[derive(Debug, Serialize)]
struct HealthzResponse {
    status: &'static str,
}

async fn healthz() -> Json<HealthzResponse> {
    Json(HealthzResponse { status: "ok" })
}

#[derive(Debug, Serialize)]
struct IngestResponse {
    id: Uuid,
    source: String,
    received_at: OffsetDateTime,
}

#[derive(Debug)]
struct NewEvent {
    id: Uuid,
    source: String,
    method: String,
    path: String,
    query: Option<String>,
    headers: serde_json::Value,
    body: Vec<u8>,
    content_type: Option<String>,
}

#[derive(Debug)]
struct StoredEvent {
    id: Uuid,
    source: String,
    received_at: OffsetDateTime,
}

#[derive(Clone)]
struct PgEventStore {
    pool: PgPool,
}

#[axum::async_trait]
trait EventStore: Send + Sync {
    async fn create_event(&self, event: NewEvent) -> Result<StoredEvent, AppError>;
}

#[axum::async_trait]
impl EventStore for PgEventStore {
    async fn create_event(&self, event: NewEvent) -> Result<StoredEvent, AppError> {
        let stored = sqlx::query_as::<_, StoredEvent>(
            r#"
            INSERT INTO events (id, source, method, path, query, headers, body, content_type)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            RETURNING id, source, received_at
            "#,
        )
        .bind(event.id)
        .bind(event.source)
        .bind(event.method)
        .bind(event.path)
        .bind(event.query)
        .bind(event.headers)
        .bind(event.body)
        .bind(event.content_type)
        .fetch_one(&self.pool)
        .await?;

        Ok(stored)
    }
}

async fn ingest_hook(
    State(state): State<AppState>,
    Path(source): Path<String>,
    method: Method,
    headers: HeaderMap,
    Query(_query): Query<HashMap<String, String>>,
    uri: Uri,
    body: Bytes,
) -> Result<impl IntoResponse, AppError> {
    let event_id = Uuid::new_v4();
    let path = uri.path().to_owned();
    let query = uri.query().map(ToOwned::to_owned);
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);

    let event = NewEvent {
        id: event_id,
        source,
        method: method.to_string(),
        path,
        query,
        headers: headers_to_json(&headers),
        body: body.to_vec(),
        content_type,
    };

    let stored = state.store.create_event(event).await?;

    let response = IngestResponse {
        id: stored.id,
        source: stored.source,
        received_at: stored.received_at,
    };

    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        "X-Event-Id",
        HeaderValue::from_str(&stored.id.to_string()).expect("valid UUID header value"),
    );

    Ok((StatusCode::CREATED, response_headers, Json(response)))
}

fn headers_to_json(headers: &HeaderMap) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    for name in headers.keys() {
        let values = headers
            .get_all(name)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        out.insert(name.to_string(), serde_json::json!(values));
    }

    serde_json::Value::Object(out)
}

impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for StoredEvent {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        use sqlx::Row;

        Ok(Self {
            id: row.try_get("id")?,
            source: row.try_get("source")?,
            received_at: row.try_get("received_at")?,
        })
    }
}

impl From<sqlx::Error> for AppError {
    fn from(value: sqlx::Error) -> Self {
        AppError {
            message: value.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use serde_json::Value;
    use tokio::sync::RwLock;
    use tower::ServiceExt;

    #[derive(Clone, Default)]
    struct MemoryStore {
        events: Arc<RwLock<Vec<NewEvent>>>,
    }

    #[axum::async_trait]
    impl EventStore for MemoryStore {
        async fn create_event(&self, event: NewEvent) -> Result<StoredEvent, AppError> {
            self.events.write().await.push(NewEvent {
                id: event.id,
                source: event.source.clone(),
                method: event.method,
                path: event.path,
                query: event.query,
                headers: event.headers,
                body: event.body,
                content_type: event.content_type,
            });

            Ok(StoredEvent {
                id: event.id,
                source: event.source,
                received_at: OffsetDateTime::now_utc(),
            })
        }
    }

    #[tokio::test]
    async fn ingest_json_payload_stores_exact_bytes() {
        let store = MemoryStore::default();
        let app = router_with_state(AppState {
            store: Arc::new(store.clone()),
        });

        let payload = br#"{"type":"invoice.paid"}"#.to_vec();
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/hooks/stripe?attempt=1")
                    .header("content-type", "application/json")
                    .header("x-signature", "abc123")
                    .body(Body::from(payload.clone()))
                    .expect("valid request"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), StatusCode::CREATED);
        assert!(response.headers().contains_key("X-Event-Id"));

        let stored = store.events.read().await;
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].source, "stripe");
        assert_eq!(stored[0].method, "POST");
        assert_eq!(stored[0].path, "/hooks/stripe");
        assert_eq!(stored[0].query.as_deref(), Some("attempt=1"));
        assert_eq!(stored[0].body, payload);

        let sig_headers = stored[0]
            .headers
            .get("x-signature")
            .and_then(Value::as_array)
            .expect("x-signature header should be captured");
        assert_eq!(sig_headers[0], "abc123");
    }

    #[tokio::test]
    async fn ingest_binary_payload_stores_exact_bytes() {
        let store = MemoryStore::default();
        let app = router_with_state(AppState {
            store: Arc::new(store.clone()),
        });

        let payload = vec![0x00, 0x01, 0x02, 0xFF, 0x7F, 0x80, 0x10];
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/hooks/github")
                    .header("content-type", "application/octet-stream")
                    .body(Body::from(payload.clone()))
                    .expect("valid request"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), StatusCode::CREATED);

        let stored = store.events.read().await;
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].source, "github");
        assert_eq!(stored[0].body, payload);
    }
}
