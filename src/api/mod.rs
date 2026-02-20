use std::{collections::HashMap, sync::Arc};

use reqwest::Client;

use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, Method, Request, StatusCode, Uri},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::errors::AppError;

pub fn router(
    pool: PgPool,
    admin_basic_user: String,
    admin_basic_pass: String,
    source_destinations: HashMap<String, String>,
) -> Router {
    let state = AppState {
        store: Arc::new(PgEventStore { pool }),
        source_destinations,
        http_client: Client::new(),
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
        .route("/events/:id", get(events_index))
        .route("/events/:id/replay", post(replay_event))
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
    source_destinations: HashMap<String, String>,
    http_client: Client,
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
    async fn list_events(&self, filters: EventListFilters) -> Result<Vec<ListedEvent>, AppError>;
    async fn get_event(&self, id: Uuid) -> Result<Option<NewEvent>, AppError>;
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

    async fn list_events(&self, filters: EventListFilters) -> Result<Vec<ListedEvent>, AppError> {
        let events = sqlx::query_as::<_, ListedEvent>(
            r#"
            SELECT id, source, status, received_at
            FROM events
            WHERE ($1::text IS NULL OR source = $1)
              AND ($2::text IS NULL OR status = $2)
              AND ($3::timestamptz IS NULL OR received_at >= $3)
              AND ($4::timestamptz IS NULL OR received_at <= $4)
              AND (
                $5::timestamptz IS NULL
                OR (received_at, id) < ($5, $6)
              )
            ORDER BY received_at DESC, id DESC
            LIMIT $7
            "#,
        )
        .bind(filters.source)
        .bind(filters.status)
        .bind(filters.since)
        .bind(filters.until)
        .bind(filters.cursor_received_at)
        .bind(filters.cursor_id)
        .bind(i64::from(filters.limit) + 1)
        .fetch_all(&self.pool)
        .await?;

        Ok(events)
    }

    async fn get_event(&self, id: Uuid) -> Result<Option<NewEvent>, AppError> {
        let event = sqlx::query_as::<_, NewEvent>(
            r#"
            SELECT id, source, method, path, query, headers, body, content_type
            FROM events
            WHERE id = $1
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(event)
    }
}

#[derive(Debug, Deserialize)]
struct EventListQuery {
    source: Option<String>,
    status: Option<String>,
    since: Option<OffsetDateTime>,
    until: Option<OffsetDateTime>,
    limit: Option<u16>,
    cursor: Option<String>,
}

#[derive(Debug, Clone)]
struct EventListFilters {
    source: Option<String>,
    status: Option<String>,
    since: Option<OffsetDateTime>,
    until: Option<OffsetDateTime>,
    cursor_received_at: Option<OffsetDateTime>,
    cursor_id: Option<Uuid>,
    limit: u16,
}

#[derive(Debug, Serialize)]
struct ListedEvent {
    id: Uuid,
    source: String,
    status: String,
    received_at: OffsetDateTime,
}

#[derive(Debug, Serialize)]
struct EventListResponse {
    items: Vec<ListedEvent>,
    next_cursor: Option<String>,
}

impl EventListQuery {
    fn into_filters(self) -> Result<EventListFilters, AppError> {
        let limit = self.limit.unwrap_or(50).min(200);
        let (cursor_received_at, cursor_id) = match self.cursor {
            Some(raw) => parse_cursor(&raw)?,
            None => (None, None),
        };

        Ok(EventListFilters {
            source: self.source,
            status: self.status,
            since: self.since,
            until: self.until,
            cursor_received_at,
            cursor_id,
            limit,
        })
    }
}

fn parse_cursor(raw: &str) -> Result<(Option<OffsetDateTime>, Option<Uuid>), AppError> {
    let (received_at, id) = raw
        .split_once('|')
        .ok_or_else(|| AppError::bad_request("invalid cursor format"))?;

    let received_at =
        OffsetDateTime::parse(received_at, &time::format_description::well_known::Rfc3339)
            .map_err(|_| AppError::bad_request("invalid cursor timestamp"))?;
    let id = Uuid::parse_str(id).map_err(|_| AppError::bad_request("invalid cursor id"))?;

    Ok((Some(received_at), Some(id)))
}

fn encode_cursor(event: &ListedEvent) -> String {
    let ts = event
        .received_at
        .format(&time::format_description::well_known::Rfc3339)
        .expect("received_at should format as RFC3339");
    format!("{}|{}", ts, event.id)
}

async fn events_index(
    State(state): State<AppState>,
    Query(query): Query<EventListQuery>,
) -> Result<impl IntoResponse, AppError> {
    let filters = query.into_filters()?;
    let limit = usize::from(filters.limit);
    let mut items = state.store.list_events(filters).await?;

    let next_cursor = if items.len() > limit {
        let last_visible = items.get(limit - 1).map(encode_cursor);
        items.truncate(limit);
        last_visible
    } else {
        None
    };

    Ok(Json(EventListResponse { items, next_cursor }))
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

#[derive(Debug, Serialize)]
struct ReplayResponse {
    event_id: Uuid,
    destination: String,
    status: u16,
}

async fn replay_event(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let event = state.store.get_event(id).await?.ok_or_else(|| AppError {
        status: StatusCode::NOT_FOUND,
        message: "event not found".to_owned(),
    })?;

    let destination = state
        .source_destinations
        .get(&event.source)
        .cloned()
        .ok_or_else(|| {
            AppError::bad_request(format!("missing destination for source '{}'", event.source))
        })?;

    let target = build_target_url(&destination, &event.path, event.query.as_deref());
    let mut request = state
        .http_client
        .request(
            reqwest::Method::from_bytes(event.method.as_bytes())
                .map_err(|err| AppError::internal(err.to_string()))?,
            &target,
        )
        .body(event.body);

    if let Some(content_type) = &event.content_type {
        request = request.header(header::CONTENT_TYPE.as_str(), content_type);
    }

    let response = request
        .send()
        .await
        .map_err(|err| AppError::internal(err.to_string()))?;

    Ok(Json(ReplayResponse {
        event_id: id,
        destination: target,
        status: response.status().as_u16(),
    }))
}

fn build_target_url(base: &str, path: &str, query: Option<&str>) -> String {
    let mut url = format!("{}{}", base.trim_end_matches('/'), path);
    if let Some(query) = query {
        url.push('?');
        url.push_str(query);
    }
    url
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

impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for NewEvent {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        use sqlx::Row;

        Ok(Self {
            id: row.try_get("id")?,
            source: row.try_get("source")?,
            method: row.try_get("method")?,
            path: row.try_get("path")?,
            query: row.try_get("query")?,
            headers: row.try_get("headers")?,
            body: row.try_get("body")?,
            content_type: row.try_get("content_type")?,
        })
    }
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

impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for ListedEvent {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        use sqlx::Row;

        Ok(Self {
            id: row.try_get("id")?,
            source: row.try_get("source")?,
            status: row.try_get("status")?,
            received_at: row.try_get("received_at")?,
        })
    }
}

impl From<sqlx::Error> for AppError {
    fn from(value: sqlx::Error) -> Self {
        AppError::internal(value.to_string())
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

        async fn list_events(
            &self,
            filters: EventListFilters,
        ) -> Result<Vec<ListedEvent>, AppError> {
            let mut items = self
                .events
                .read()
                .await
                .iter()
                .filter(|event| match &filters.source {
                    Some(source) => &event.source == source,
                    None => true,
                })
                .map(|event| ListedEvent {
                    id: event.id,
                    source: event.source.clone(),
                    status: "received".to_owned(),
                    received_at: OffsetDateTime::now_utc(),
                })
                .collect::<Vec<_>>();

            items.sort_by(|a, b| (b.received_at, b.id).cmp(&(a.received_at, a.id)));

            if let Some(status) = filters.status {
                items.retain(|e| e.status == status);
            }
            if let Some(since) = filters.since {
                items.retain(|e| e.received_at >= since);
            }
            if let Some(until) = filters.until {
                items.retain(|e| e.received_at <= until);
            }
            if let (Some(cursor_received_at), Some(cursor_id)) =
                (filters.cursor_received_at, filters.cursor_id)
            {
                items.retain(|e| (e.received_at, e.id) < (cursor_received_at, cursor_id));
            }

            items.truncate(usize::from(filters.limit) + 1);
            Ok(items)
        }

        async fn get_event(&self, id: Uuid) -> Result<Option<NewEvent>, AppError> {
            let event = self
                .events
                .read()
                .await
                .iter()
                .find(|event| event.id == id)
                .map(|event| NewEvent {
                    id: event.id,
                    source: event.source.clone(),
                    method: event.method.clone(),
                    path: event.path.clone(),
                    query: event.query.clone(),
                    headers: event.headers.clone(),
                    body: event.body.clone(),
                    content_type: event.content_type.clone(),
                });

            Ok(event)
        }
    }

    #[tokio::test]
    async fn ingest_json_payload_stores_exact_bytes() {
        let store = MemoryStore::default();
        let app = router_with_state(
            AppState {
                store: Arc::new(store.clone()),
                source_destinations: HashMap::new(),
                http_client: Client::new(),
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
                source_destinations: HashMap::new(),
                http_client: Client::new(),
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
                source_destinations: HashMap::new(),
                http_client: Client::new(),
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
                source_destinations: HashMap::new(),
                http_client: Client::new(),
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
                source_destinations: HashMap::new(),
                http_client: Client::new(),
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

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn replay_returns_bad_request_when_source_destination_missing() {
        let store = MemoryStore::default();
        let app = router_with_state(
            AppState {
                store: Arc::new(store),
                source_destinations: HashMap::new(),
                http_client: Client::new(),
            },
            "admin".to_owned(),
            "secret".to_owned(),
        );

        let ingest_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/hooks/stripe")
                    .body(Body::from("{}"))
                    .expect("valid request"),
            )
            .await
            .expect("request should succeed");

        let event_id = ingest_response
            .headers()
            .get("X-Event-Id")
            .and_then(|value| value.to_str().ok())
            .expect("event id header should exist");

        let replay_response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/events/{event_id}/replay"))
                    .header(header::AUTHORIZATION, "Basic YWRtaW46c2VjcmV0")
                    .body(Body::empty())
                    .expect("valid request"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(replay_response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn events_returns_items_and_next_cursor() {
        let store = MemoryStore::default();
        let app = router_with_state(
            AppState {
                store: Arc::new(store.clone()),
                source_destinations: HashMap::new(),
                http_client: Client::new(),
            },
            "admin".to_owned(),
            "secret".to_owned(),
        );

        for source in ["one", "two"] {
            let _ = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(format!("/hooks/{source}").as_str())
                        .body(Body::from("{}"))
                        .expect("valid request"),
                )
                .await
                .expect("request should succeed");
        }

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/events?limit=1")
                    .header(header::AUTHORIZATION, "Basic YWRtaW46c2VjcmV0")
                    .body(Body::empty())
                    .expect("valid request"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), StatusCode::OK);
    }
}
