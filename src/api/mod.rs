use std::{collections::HashMap, sync::Arc};

use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, Method, Request, StatusCode, Uri},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Serialize;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::errors::AppError;

pub fn router(pool: PgPool, admin_basic_user: String, admin_basic_pass: String) -> Router {
    let state = AppState {
        store: Arc::new(PgEventStore { pool }),
    };

    router_with_state(state, admin_basic_user, admin_basic_pass)
}

#[derive(Clone)]
struct AdminAuthConfig {
    user: String,
    pass: String,
}

fn router_with_state(
    state: AppState,
    admin_basic_user: String,
    admin_basic_pass: String,
) -> Router {
    let admin_auth = AdminAuthConfig {
        user: admin_basic_user,
        pass: admin_basic_pass,
    };

    let events_routes = Router::new()
        .route("/events", get(events_index))
        .route("/events/*rest", get(events_index))
        .layer(middleware::from_fn_with_state(
            admin_auth,
            basic_auth_middleware,
        ));

    Router::new()
        .route("/healthz", get(healthz))
        .route("/hooks/:source", post(ingest_hook))
        .merge(events_routes)
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

async fn events_index() -> impl IntoResponse {
    StatusCode::NO_CONTENT
}

async fn basic_auth_middleware(
    State(auth): State<AdminAuthConfig>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    if is_authorized(request.headers(), &auth.user, &auth.pass) {
        return next.run(request).await;
    }

    let mut response = StatusCode::UNAUTHORIZED.into_response();
    response.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_static("Basic realm=\"events\""),
    );
    response
}

fn is_authorized(headers: &HeaderMap, expected_user: &str, expected_pass: &str) -> bool {
    let Some(auth_header) = headers.get(header::AUTHORIZATION) else {
        return false;
    };

    let Ok(auth_header) = auth_header.to_str() else {
        return false;
    };

    let Some(encoded) = auth_header.strip_prefix("Basic ") else {
        return false;
    };

    let Ok(decoded) = decode_base64(encoded) else {
        return false;
    };

    let Ok(credentials) = String::from_utf8(decoded) else {
        return false;
    };

    let mut parts = credentials.splitn(2, ':');
    let Some(user) = parts.next() else {
        return false;
    };
    let Some(pass) = parts.next() else {
        return false;
    };

    user == expected_user && pass == expected_pass
}

fn decode_base64(input: &str) -> Result<Vec<u8>, ()> {
    fn val(b: u8) -> Option<u8> {
        match b {
            b'A'..=b'Z' => Some(b - b'A'),
            b'a'..=b'z' => Some(b - b'a' + 26),
            b'0'..=b'9' => Some(b - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }

    let bytes = input.as_bytes();
    if bytes.is_empty() || bytes.len() % 4 != 0 {
        return Err(());
    }

    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    let mut i = 0;
    while i < bytes.len() {
        let c0 = bytes[i];
        let c1 = bytes[i + 1];
        let c2 = bytes[i + 2];
        let c3 = bytes[i + 3];

        let v0 = val(c0).ok_or(())?;
        let v1 = val(c1).ok_or(())?;

        if c2 == b'=' {
            if c3 != b'=' || i + 4 != bytes.len() {
                return Err(());
            }
            out.push((v0 << 2) | (v1 >> 4));
            break;
        }

        let v2 = val(c2).ok_or(())?;
        out.push((v0 << 2) | (v1 >> 4));
        out.push((v1 << 4) | (v2 >> 2));

        if c3 == b'=' {
            if i + 4 != bytes.len() {
                return Err(());
            }
            break;
        }

        let v3 = val(c3).ok_or(())?;
        out.push((v2 << 6) | v3);

        i += 4;
    }

    Ok(out)
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
        let app = router_with_state(
            AppState {
                store: Arc::new(store.clone()),
            },
            "admin".to_owned(),
            "secret".to_owned(),
        );

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
        let app = router_with_state(
            AppState {
                store: Arc::new(store.clone()),
            },
            "admin".to_owned(),
            "secret".to_owned(),
        );

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

    #[tokio::test]
    async fn events_require_basic_auth() {
        let store = MemoryStore::default();
        let app = router_with_state(
            AppState {
                store: Arc::new(store),
            },
            "admin".to_owned(),
            "secret".to_owned(),
        );

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/events")
                    .body(Body::empty())
                    .expect("valid request"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            response
                .headers()
                .get(header::WWW_AUTHENTICATE)
                .and_then(|v| v.to_str().ok()),
            Some("Basic realm=\"events\"")
        );
    }

    #[tokio::test]
    async fn hooks_are_not_protected_by_basic_auth() {
        let store = MemoryStore::default();
        let app = router_with_state(
            AppState {
                store: Arc::new(store.clone()),
            },
            "admin".to_owned(),
            "secret".to_owned(),
        );

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/hooks/source")
                    .body(Body::from("hello"))
                    .expect("valid request"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn events_accept_valid_basic_auth() {
        let store = MemoryStore::default();
        let app = router_with_state(
            AppState {
                store: Arc::new(store),
            },
            "admin".to_owned(),
            "secret".to_owned(),
        );

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/events/anything")
                    .header(header::AUTHORIZATION, "Basic YWRtaW46c2VjcmV0")
                    .body(Body::empty())
                    .expect("valid request"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }
}
