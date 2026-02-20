use std::{collections::HashMap, error::Error as _, sync::Arc};

use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Extension, Path, Query, State},
    http::{header, HeaderMap, HeaderValue, Method, Request, StatusCode, Uri},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{config::SourceConfig, domain::EventStatus, errors::AppError};

pub fn router(
    pool: PgPool,
    admin_basic_user: String,
    admin_basic_pass: String,
    source_configs: HashMap<String, SourceConfig>,
    source_secrets: HashMap<String, String>,
    max_webhook_size_bytes: usize,
    replay_forward_headers: Vec<String>,
) -> Router {
    let http_client = build_http_client();

    let state = AppState {
        store: Arc::new(PgEventStore { pool }),
        source_configs,
        source_secrets,
        max_webhook_size_bytes,
        replay_forward_headers,
        http_client,
    };

    router_with_state(
        state,
        admin_basic_user,
        admin_basic_pass,
        max_webhook_size_bytes,
    )
}

#[derive(Clone)]
struct AdminAuthConfig {
    user: String,
    pass: String,
}

fn build_http_client() -> Client {
    Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .no_proxy()
        .build()
        .expect("http client should build")
}
fn router_with_state(
    state: AppState,
    admin_basic_user: String,
    admin_basic_pass: String,
    max_webhook_size_bytes: usize,
) -> Router {
    let admin_auth = AdminAuthConfig {
        user: admin_basic_user,
        pass: admin_basic_pass,
    };

    let events_routes = Router::new()
        .route("/events", get(events_index))
        .route("/events/:id", get(event_show))
        .route("/events/:id/replay", post(replay_event))
        .layer(middleware::from_fn_with_state(
            admin_auth,
            basic_auth_middleware,
        ));

    Router::new()
        .route("/healthz", get(healthz))
        .route(
            "/hooks/:source",
            post(ingest_hook).layer(DefaultBodyLimit::max(max_webhook_size_bytes)),
        )
        .merge(events_routes)
        .layer(middleware::from_fn(request_id_middleware))
        .with_state(state)
}

#[derive(Clone, Debug)]
struct RequestId(String);

async fn request_id_middleware(mut request: Request<axum::body::Body>, next: Next) -> Response {
    let request_id = request
        .headers()
        .get("X-Request-Id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    request
        .extensions_mut()
        .insert(RequestId(request_id.clone()));

    let mut response = next.run(request).await;
    if let Ok(header_value) = HeaderValue::from_str(&request_id) {
        response.headers_mut().insert("X-Request-Id", header_value);
    }

    response
}

#[derive(Clone)]
struct AppState {
    store: Arc<dyn EventStore>,
    source_configs: HashMap<String, SourceConfig>,
    source_secrets: HashMap<String, String>,
    max_webhook_size_bytes: usize,
    replay_forward_headers: Vec<String>,
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

#[derive(Debug, Clone)]
struct NewEvent {
    id: Uuid,
    source: String,
    method: String,
    path: String,
    query: Option<String>,
    headers: serde_json::Value,
    body: Vec<u8>,
    body_size_bytes: i32,
    content_type: Option<String>,
}

#[derive(Debug)]
struct StoredEvent {
    id: Uuid,
    source: String,
    received_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
struct ReplayEvent {
    id: Uuid,
    source: String,
    method: String,
    headers: serde_json::Value,
    body: Vec<u8>,
    content_type: Option<String>,
}

#[derive(Debug)]
struct NewDeliveryAttempt {
    event_id: Uuid,
    destination: String,
    response_status: Option<i32>,
    response_headers: Option<serde_json::Value>,
    response_body: Option<Vec<u8>>,
    error: Option<String>,
    duration_ms: i32,
}

#[derive(Clone)]
struct PgEventStore {
    pool: PgPool,
}

#[axum::async_trait]
trait EventStore: Send + Sync {
    async fn create_event(&self, event: NewEvent) -> Result<StoredEvent, AppError>;
    async fn list_events(&self, filters: EventListFilters) -> Result<Vec<ListedEvent>, AppError>;
    async fn get_event(&self, id: Uuid) -> Result<Option<ListedEvent>, AppError>;
    async fn load_event_for_replay(&self, id: Uuid) -> Result<Option<ReplayEvent>, AppError>;
    async fn create_delivery_attempt(&self, attempt: NewDeliveryAttempt) -> Result<(), AppError>;
    async fn update_event_status(
        &self,
        event_id: Uuid,
        status: EventStatus,
    ) -> Result<(), AppError>;
}

#[axum::async_trait]
impl EventStore for PgEventStore {
    async fn create_event(&self, event: NewEvent) -> Result<StoredEvent, AppError> {
        let stored = sqlx::query_as::<_, StoredEvent>(
            r#"
            INSERT INTO events (id, source, method, path, query, headers, body, body_size_bytes, content_type)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
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
        .bind(event.body_size_bytes)
        .bind(event.content_type)
        .fetch_one(&self.pool)
        .await?;

        Ok(stored)
    }

    async fn list_events(&self, filters: EventListFilters) -> Result<Vec<ListedEvent>, AppError> {
        let events = sqlx::query_as::<_, ListedEvent>(
            r#"
            SELECT id, source, status, received_at, body_size_bytes
            FROM events
            WHERE ($1::text IS NULL OR source = $1)
              AND ($2::event_status IS NULL OR status = $2)
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

    async fn get_event(&self, id: Uuid) -> Result<Option<ListedEvent>, AppError> {
        let event = sqlx::query_as::<_, ListedEvent>(
            r#"
            SELECT id, source, status, received_at, body_size_bytes
            FROM events
            WHERE id = $1
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(event)
    }

    async fn load_event_for_replay(&self, id: Uuid) -> Result<Option<ReplayEvent>, AppError> {
        let event = sqlx::query_as::<_, ReplayEvent>(
            r#"
            SELECT id, source, method, headers, body, content_type
            FROM events
            WHERE id = $1
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(event)
    }

    async fn create_delivery_attempt(&self, attempt: NewDeliveryAttempt) -> Result<(), AppError> {
        sqlx::query(
            r#"
            INSERT INTO delivery_attempts (
                event_id,
                destination,
                response_status,
                response_headers,
                response_body,
                error,
                duration_ms
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            "#,
        )
        .bind(attempt.event_id)
        .bind(attempt.destination)
        .bind(attempt.response_status)
        .bind(attempt.response_headers)
        .bind(attempt.response_body)
        .bind(attempt.error)
        .bind(attempt.duration_ms)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn update_event_status(
        &self,
        event_id: Uuid,
        status: EventStatus,
    ) -> Result<(), AppError> {
        sqlx::query(
            r#"
            UPDATE events
            SET status = $1
            WHERE id = $2
            "#,
        )
        .bind(status)
        .bind(event_id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }
}

#[derive(Debug, Serialize)]
struct ReplayResponse {
    event_id: Uuid,
    status: EventStatus,
}

async fn replay_event(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let event = state
        .store
        .load_event_for_replay(id)
        .await?
        .ok_or_else(|| AppError::not_found(format!("event not found: {id}")))?;

    tracing::info!(request_id = %request_id.0, event_id = %event.id, "replaying event");

    let source_config = state.source_configs.get(&event.source).ok_or_else(|| {
        AppError::validation(format!(
            "no destination configured for source: {}",
            event.source
        ))
    })?;

    let destination = source_config.url.clone();

    let started_at = std::time::Instant::now();
    let method = event
        .method
        .parse::<reqwest::Method>()
        .map_err(|err| AppError::internal(err.to_string()))?;

    let mut request = state
        .http_client
        .request(method, destination.clone())
        .header("X-Webhook-Replay", "true")
        .header("X-Original-Event-Id", event.id.to_string())
        .timeout(std::time::Duration::from_millis(source_config.timeout_ms))
        .body(event.body.clone());

    if let Some(content_type) = &event.content_type {
        request = request.header(reqwest::header::CONTENT_TYPE, content_type);
    }

    let original_headers = json_to_headermap(&event.headers);
    request = apply_forwarded_headers(request, &original_headers, &state.replay_forward_headers);

    let (status, attempt) = match request.send().await {
        Ok(response) => {
            let response_status = i32::from(response.status().as_u16());
            let headers = headers_to_json_value(response.headers());
            let body = response
                .bytes()
                .await
                .map(|bytes| bytes.to_vec())
                .map_err(|err| AppError::upstream(err.to_string()))?;
            let status = if response_status >= 200 && response_status < 300 {
                EventStatus::Delivered
            } else {
                EventStatus::Failed
            };

            (
                status,
                NewDeliveryAttempt {
                    event_id: event.id,
                    destination,
                    response_status: Some(response_status),
                    response_headers: Some(headers),
                    response_body: Some(body),
                    error: None,
                    duration_ms: started_at.elapsed().as_millis() as i32,
                },
            )
        }
        Err(err) => (
            EventStatus::Failed,
            NewDeliveryAttempt {
                event_id: event.id,
                destination,
                response_status: None,
                response_headers: None,
                response_body: None,
                error: Some(err.to_string()),
                duration_ms: started_at.elapsed().as_millis() as i32,
            },
        ),
    };

    state.store.create_delivery_attempt(attempt).await?;
    state.store.update_event_status(event.id, status).await?;

    tracing::info!(request_id = %request_id.0, event_id = %event.id, status = ?status, "replay finished");

    Ok((
        StatusCode::OK,
        Json(ReplayResponse {
            event_id: event.id,
            status,
        }),
    ))
}

#[derive(Debug, Deserialize)]
struct EventListQuery {
    source: Option<String>,
    status: Option<EventStatus>,
    since: Option<OffsetDateTime>,
    until: Option<OffsetDateTime>,
    limit: Option<u16>,
    cursor: Option<String>,
}

#[derive(Debug, Clone)]
struct EventListFilters {
    source: Option<String>,
    status: Option<EventStatus>,
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
    status: EventStatus,
    received_at: OffsetDateTime,
    body_size_bytes: i32,
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
        .ok_or_else(|| AppError::validation("invalid cursor format"))?;

    let received_at =
        OffsetDateTime::parse(received_at, &time::format_description::well_known::Rfc3339)
            .map_err(|_| AppError::validation("invalid cursor timestamp"))?;
    let id = Uuid::parse_str(id).map_err(|_| AppError::validation("invalid cursor id"))?;

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
    Extension(request_id): Extension<RequestId>,
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

    tracing::info!(request_id = %request_id.0, event_count = items.len(), "events listed");

    Ok(Json(EventListResponse { items, next_cursor }))
}

async fn event_show(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let item = state
        .store
        .get_event(id)
        .await?
        .ok_or_else(|| AppError::not_found(format!("event not found: {id}")))?;

    tracing::info!(request_id = %request_id.0, event_id = %item.id, "event fetched");

    Ok(Json(item))
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

fn parse_signature_header(value: &str) -> Option<Vec<u8>> {
    let signature = value.strip_prefix("sha256=")?;
    hex::decode(signature).ok()
}

fn verify_signature(secret: &str, body: &[u8], signature: &[u8]) -> bool {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .expect("HMAC accepts arbitrary key lengths");
    mac.update(body);
    let expected = mac.finalize().into_bytes();

    if expected.len() != signature.len() {
        return false;
    }

    expected.ct_eq(signature).into()
}

fn validate_source_signature(
    source: &str,
    source_secrets: &HashMap<String, String>,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<(), AppError> {
    let Some(secret) = source_secrets.get(source) else {
        return Ok(());
    };

    let provided = headers
        .get("X-Signature")
        .and_then(|value| value.to_str().ok())
        .and_then(parse_signature_header)
        .ok_or_else(|| AppError::auth("invalid signature"))?;

    if verify_signature(secret, body, &provided) {
        Ok(())
    } else {
        Err(AppError::auth("invalid signature"))
    }
}

#[allow(clippy::too_many_arguments)]
async fn ingest_hook(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(source): Path<String>,
    method: Method,
    headers: HeaderMap,
    uri: Uri,
    body: Result<Bytes, axum::extract::rejection::BytesRejection>,
) -> Result<impl IntoResponse, AppError> {
    let body = body.map_err(|rejection| {
        if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE
            || rejection
                .source()
                .and_then(|source| source.downcast_ref::<http_body_util::LengthLimitError>())
                .is_some()
        {
            AppError::payload_too_large("request body too large").with_details(
                serde_json::json!({"max_webhook_size_bytes": state.max_webhook_size_bytes}),
            )
        } else {
            AppError::validation(rejection.to_string())
        }
    })?;

    validate_source_signature(&source, &state.source_secrets, &headers, &body)?;

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
        body_size_bytes: i32::try_from(body.len())
            .map_err(|_| AppError::validation("request body too large"))?,
        body: body.to_vec(),
        content_type,
    };

    let stored = state.store.create_event(event).await?;

    tracing::info!(request_id = %request_id.0, event_id = %stored.id, source = %stored.source, "event ingested");

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

fn headers_to_json_value(headers: &reqwest::header::HeaderMap) -> serde_json::Value {
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

#[cfg(test)]
fn default_replay_forward_headers() -> Vec<String> {
    [
        "content-type",
        "user-agent",
        "x-github-event",
        "x-github-delivery",
        "stripe-signature",
    ]
    .iter()
    .map(|value| (*value).to_owned())
    .collect()
}

fn is_hop_by_hop_header(name: &str) -> bool {
    matches!(
        name,
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

fn json_to_headermap(headers: &serde_json::Value) -> HeaderMap {
    let mut header_map = HeaderMap::new();

    if let serde_json::Value::Object(map) = headers {
        for (name, raw_values) in map {
            let Ok(header_name) = reqwest::header::HeaderName::from_bytes(name.as_bytes()) else {
                continue;
            };

            let values = match raw_values {
                serde_json::Value::Array(items) => items,
                _ => continue,
            };

            for value in values {
                let Some(value) = value.as_str() else {
                    continue;
                };

                let Ok(header_value) = reqwest::header::HeaderValue::from_str(value) else {
                    continue;
                };

                header_map.append(header_name.clone(), header_value);
            }
        }
    }

    header_map
}

fn apply_forwarded_headers(
    mut request: reqwest::RequestBuilder,
    original_headers: &HeaderMap,
    allowlist: &[String],
) -> reqwest::RequestBuilder {
    for header_name in allowlist {
        let normalized = header_name.trim().to_ascii_lowercase();
        if normalized.is_empty() || is_hop_by_hop_header(&normalized) {
            continue;
        }

        let Ok(source_name) = header::HeaderName::from_bytes(normalized.as_bytes()) else {
            continue;
        };

        let values = original_headers.get_all(&source_name);
        if values.iter().next().is_none() {
            continue;
        }

        let Ok(target_name) = reqwest::header::HeaderName::from_bytes(normalized.as_bytes()) else {
            continue;
        };

        for value in values.iter() {
            let Ok(value_str) = value.to_str() else {
                continue;
            };
            request = request.header(target_name.clone(), value_str);
        }
    }

    request
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
            body_size_bytes: row.try_get("body_size_bytes")?,
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
            body_size_bytes: row.try_get("body_size_bytes")?,
        })
    }
}

impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for ReplayEvent {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        use sqlx::Row;

        Ok(Self {
            id: row.try_get("id")?,
            source: row.try_get("source")?,
            method: row.try_get("method")?,
            headers: row.try_get("headers")?,
            body: row.try_get("body")?,
            content_type: row.try_get("content_type")?,
        })
    }
}

impl From<sqlx::Error> for AppError {
    fn from(value: sqlx::Error) -> Self {
        AppError::db(value.to_string())
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
        events: Arc<RwLock<Vec<StoredMemoryEvent>>>,
        attempts: Arc<RwLock<Vec<NewDeliveryAttempt>>>,
        statuses: Arc<RwLock<HashMap<Uuid, EventStatus>>>,
    }

    #[derive(Clone)]
    struct StoredMemoryEvent {
        event: NewEvent,
        received_at: OffsetDateTime,
    }

    #[axum::async_trait]
    impl EventStore for MemoryStore {
        async fn create_event(&self, event: NewEvent) -> Result<StoredEvent, AppError> {
            let received_at = OffsetDateTime::now_utc();
            self.events.write().await.push(StoredMemoryEvent {
                received_at,
                event: event.clone(),
            });

            Ok(StoredEvent {
                id: event.id,
                source: event.source,
                received_at,
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
                    Some(source) => &event.event.source == source,
                    None => true,
                })
                .map(|event| ListedEvent {
                    id: event.event.id,
                    source: event.event.source.clone(),
                    status: EventStatus::Received,
                    received_at: event.received_at,
                    body_size_bytes: event.event.body_size_bytes,
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

        async fn get_event(&self, id: Uuid) -> Result<Option<ListedEvent>, AppError> {
            let event = self
                .events
                .read()
                .await
                .iter()
                .find(|event| event.event.id == id)
                .map(|event| ListedEvent {
                    id: event.event.id,
                    source: event.event.source.clone(),
                    status: EventStatus::Received,
                    received_at: event.received_at,
                    body_size_bytes: event.event.body_size_bytes,
                });

            Ok(event)
        }

        async fn load_event_for_replay(&self, id: Uuid) -> Result<Option<ReplayEvent>, AppError> {
            let event = self
                .events
                .read()
                .await
                .iter()
                .find(|event| event.event.id == id)
                .map(|event| ReplayEvent {
                    id: event.event.id,
                    source: event.event.source.clone(),
                    method: event.event.method.clone(),
                    headers: event.event.headers.clone(),
                    body: event.event.body.clone(),
                    content_type: event.event.content_type.clone(),
                });

            Ok(event)
        }

        async fn create_delivery_attempt(
            &self,
            attempt: NewDeliveryAttempt,
        ) -> Result<(), AppError> {
            self.attempts.write().await.push(attempt);
            Ok(())
        }

        async fn update_event_status(
            &self,
            event_id: Uuid,
            status: EventStatus,
        ) -> Result<(), AppError> {
            self.statuses.write().await.insert(event_id, status);
            Ok(())
        }
    }

    async fn spawn_test_receiver() -> (String, tokio::sync::oneshot::Receiver<(HeaderMap, Vec<u8>)>)
    {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener should bind");
        let addr = listener
            .local_addr()
            .expect("listener should expose local addr");

        let (tx, rx) = tokio::sync::oneshot::channel();
        let tx = Arc::new(tokio::sync::Mutex::new(Some(tx)));

        let app = Router::new().route(
            "/",
            post({
                let tx = tx.clone();
                move |headers: HeaderMap, body: Bytes| {
                    let tx = tx.clone();
                    async move {
                        if let Some(tx) = tx.lock().await.take() {
                            let _ = tx.send((headers, body.to_vec()));
                        }
                        StatusCode::OK
                    }
                }
            }),
        );

        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("test receiver should serve");
        });

        (format!("http://{addr}/"), rx)
    }

    #[tokio::test]
    async fn response_includes_generated_request_id_when_missing() {
        let store = MemoryStore::default();
        let app = router_with_state(
            AppState {
                store: Arc::new(store),
                source_configs: HashMap::new(),
                source_secrets: HashMap::new(),
                max_webhook_size_bytes: 5_242_880,
                replay_forward_headers: default_replay_forward_headers(),
                http_client: build_http_client(),
            },
            "admin".to_owned(),
            "secret".to_owned(),
            5_242_880,
        );

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/hooks/stripe")
                    .body(Body::from("{}"))
                    .expect("valid request"),
            )
            .await
            .expect("request should succeed");

        assert!(response.headers().contains_key("X-Request-Id"));
    }

    #[tokio::test]
    async fn response_echoes_request_id_header() {
        let store = MemoryStore::default();
        let app = router_with_state(
            AppState {
                store: Arc::new(store),
                source_configs: HashMap::new(),
                source_secrets: HashMap::new(),
                max_webhook_size_bytes: 5_242_880,
                replay_forward_headers: default_replay_forward_headers(),
                http_client: build_http_client(),
            },
            "admin".to_owned(),
            "secret".to_owned(),
            5_242_880,
        );

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/hooks/stripe")
                    .header("X-Request-Id", "req-123")
                    .body(Body::from("{}"))
                    .expect("valid request"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(
            response
                .headers()
                .get("X-Request-Id")
                .and_then(|value| value.to_str().ok()),
            Some("req-123")
        );
    }

    #[tokio::test]
    async fn ingest_json_payload_stores_exact_bytes() {
        let store = MemoryStore::default();
        let app = router_with_state(
            AppState {
                store: Arc::new(store.clone()),
                source_configs: HashMap::new(),
                source_secrets: HashMap::new(),
                max_webhook_size_bytes: 5_242_880,
                replay_forward_headers: default_replay_forward_headers(),
                http_client: build_http_client(),
            },
            "admin".to_owned(),
            "secret".to_owned(),
            5_242_880,
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
        assert_eq!(stored[0].event.source, "stripe");
        assert_eq!(stored[0].event.method, "POST");
        assert_eq!(stored[0].event.path, "/hooks/stripe");
        assert_eq!(stored[0].event.query.as_deref(), Some("attempt=1"));
        assert_eq!(stored[0].event.body, payload);
        assert_eq!(stored[0].event.source, "stripe");
        assert_eq!(stored[0].event.method, "POST");
        assert_eq!(stored[0].event.path, "/hooks/stripe");
        assert_eq!(stored[0].event.query.as_deref(), Some("attempt=1"));
        assert_eq!(stored[0].event.body, payload);
        assert_eq!(
            stored[0].event.body_size_bytes as usize,
            stored[0].event.body.len()
        );

        let sig_headers = stored[0]
            .event
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
                source_configs: HashMap::new(),
                source_secrets: HashMap::new(),
                max_webhook_size_bytes: 5_242_880,
                replay_forward_headers: default_replay_forward_headers(),
                http_client: build_http_client(),
            },
            "admin".to_owned(),
            "secret".to_owned(),
            5_242_880,
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
        assert_eq!(stored[0].event.source, "github");
        assert_eq!(stored[0].event.body, payload);
        assert_eq!(stored[0].event.source, "github");
        assert_eq!(stored[0].event.body, payload);
        assert_eq!(
            stored[0].event.body_size_bytes as usize,
            stored[0].event.body.len()
        );
    }

    #[tokio::test]
    async fn ingest_with_body_under_limit_succeeds() {
        let store = MemoryStore::default();
        let app = router_with_state(
            AppState {
                store: Arc::new(store.clone()),
                source_configs: HashMap::new(),
                source_secrets: HashMap::new(),
                max_webhook_size_bytes: 8,
                replay_forward_headers: default_replay_forward_headers(),
                http_client: build_http_client(),
            },
            "admin".to_owned(),
            "secret".to_owned(),
            8,
        );

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/hooks/source")
                    .body(Body::from("12345678"))
                    .expect("valid request"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), StatusCode::CREATED);
        assert_eq!(store.events.read().await.len(), 1);
    }

    #[tokio::test]
    async fn ingest_with_body_over_limit_returns_413_error() {
        let store = MemoryStore::default();
        let app = router_with_state(
            AppState {
                store: Arc::new(store.clone()),
                source_configs: HashMap::new(),
                source_secrets: HashMap::new(),
                max_webhook_size_bytes: 8,
                replay_forward_headers: default_replay_forward_headers(),
                http_client: build_http_client(),
            },
            "admin".to_owned(),
            "secret".to_owned(),
            8,
        );

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/hooks/source")
                    .body(Body::from("123456789"))
                    .expect("valid request"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(store.events.read().await.len(), 0);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body should be readable");
        let payload: Value = serde_json::from_slice(&body).expect("valid json");

        assert_eq!(
            payload.get("code"),
            Some(&Value::String("payload_too_large".to_owned()))
        );
        assert_eq!(
            payload.get("message"),
            Some(&Value::String("request body too large".to_owned()))
        );
        assert_eq!(
            payload
                .get("details")
                .and_then(Value::as_object)
                .and_then(|d| d.get("max_webhook_size_bytes")),
            Some(&Value::Number(8_u64.into()))
        );
    }

    #[tokio::test]
    async fn events_require_basic_auth() {
        let store = MemoryStore::default();
        let app = router_with_state(
            AppState {
                store: Arc::new(store),
                source_configs: HashMap::new(),
                source_secrets: HashMap::new(),
                max_webhook_size_bytes: 5_242_880,
                replay_forward_headers: default_replay_forward_headers(),
                http_client: build_http_client(),
            },
            "admin".to_owned(),
            "secret".to_owned(),
            5_242_880,
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
                source_configs: HashMap::new(),
                source_secrets: HashMap::new(),
                max_webhook_size_bytes: 5_242_880,
                replay_forward_headers: default_replay_forward_headers(),
                http_client: build_http_client(),
            },
            "admin".to_owned(),
            "secret".to_owned(),
            5_242_880,
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
    async fn ingest_requires_signature_when_source_secret_present() {
        let store = MemoryStore::default();
        let mut source_secrets = HashMap::new();
        source_secrets.insert("stripe".to_owned(), "topsecret".to_owned());

        let app = router_with_state(
            AppState {
                store: Arc::new(store),
                source_configs: HashMap::new(),
                source_secrets,
                max_webhook_size_bytes: 5_242_880,
                replay_forward_headers: default_replay_forward_headers(),
                http_client: build_http_client(),
            },
            "admin".to_owned(),
            "secret".to_owned(),
            5_242_880,
        );

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/hooks/stripe")
                    .body(Body::from("{}"))
                    .expect("valid request"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn ingest_accepts_valid_signature_when_source_secret_present() {
        let store = MemoryStore::default();
        let mut source_secrets = HashMap::new();
        source_secrets.insert("stripe".to_owned(), "topsecret".to_owned());

        let app = router_with_state(
            AppState {
                store: Arc::new(store),
                source_configs: HashMap::new(),
                source_secrets,
                max_webhook_size_bytes: 5_242_880,
                replay_forward_headers: default_replay_forward_headers(),
                http_client: build_http_client(),
            },
            "admin".to_owned(),
            "secret".to_owned(),
            5_242_880,
        );

        let payload = b"{}";
        let mut mac = Hmac::<Sha256>::new_from_slice(b"topsecret").expect("valid key");
        mac.update(payload);
        let signature = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/hooks/stripe")
                    .header("X-Signature", signature)
                    .body(Body::from(payload.to_vec()))
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
                source_configs: HashMap::new(),
                source_secrets: HashMap::new(),
                max_webhook_size_bytes: 5_242_880,
                replay_forward_headers: default_replay_forward_headers(),
                http_client: build_http_client(),
            },
            "admin".to_owned(),
            "secret".to_owned(),
            5_242_880,
        );

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/events")
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
                source_configs: HashMap::new(),
                source_secrets: HashMap::new(),
                max_webhook_size_bytes: 5_242_880,
                replay_forward_headers: default_replay_forward_headers(),
                http_client: build_http_client(),
            },
            "admin".to_owned(),
            "secret".to_owned(),
            5_242_880,
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
    async fn invalid_cursor_returns_unified_error_payload() {
        let store = MemoryStore::default();
        let app = router_with_state(
            AppState {
                store: Arc::new(store),
                source_configs: HashMap::new(),
                source_secrets: HashMap::new(),
                max_webhook_size_bytes: 5_242_880,
                replay_forward_headers: default_replay_forward_headers(),
                http_client: build_http_client(),
            },
            "admin".to_owned(),
            "secret".to_owned(),
            5_242_880,
        );

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/events?cursor=invalid")
                    .header(header::AUTHORIZATION, "Basic YWRtaW46c2VjcmV0")
                    .body(Body::empty())
                    .expect("valid request"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body should be readable");
        let payload: Value = serde_json::from_slice(&body).expect("valid json");

        assert_eq!(
            payload.get("code"),
            Some(&Value::String("validation".to_owned()))
        );
        assert_eq!(
            payload.get("message"),
            Some(&Value::String("invalid cursor format".to_owned()))
        );
        assert_eq!(payload.get("details"), Some(&Value::Null));
    }

    #[tokio::test]
    async fn events_returns_items_and_next_cursor() {
        let store = MemoryStore::default();
        let app = router_with_state(
            AppState {
                store: Arc::new(store.clone()),
                source_configs: HashMap::new(),
                source_secrets: HashMap::new(),
                max_webhook_size_bytes: 5_242_880,
                replay_forward_headers: default_replay_forward_headers(),
                http_client: build_http_client(),
            },
            "admin".to_owned(),
            "secret".to_owned(),
            5_242_880,
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

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body should be readable");
        let payload: Value = serde_json::from_slice(&body).expect("valid json");

        let items = payload
            .get("items")
            .and_then(Value::as_array)
            .expect("items should be an array");
        assert_eq!(items.len(), 1);
        assert!(items[0]
            .get("body_size_bytes")
            .and_then(Value::as_i64)
            .is_some());
    }

    #[tokio::test]
    async fn event_show_returns_body_size_bytes() {
        let store = MemoryStore::default();
        let app = router_with_state(
            AppState {
                store: Arc::new(store.clone()),
                source_configs: HashMap::new(),
                source_secrets: HashMap::new(),
                max_webhook_size_bytes: 5_242_880,
                replay_forward_headers: default_replay_forward_headers(),
                http_client: build_http_client(),
            },
            "admin".to_owned(),
            "secret".to_owned(),
            5_242_880,
        );

        let ingest_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/hooks/stripe")
                    .body(Body::from("{\"ok\":true}"))
                    .expect("valid request"),
            )
            .await
            .expect("request should succeed");

        let event_id = ingest_response
            .headers()
            .get("X-Event-Id")
            .and_then(|value| value.to_str().ok())
            .expect("event id header should exist");

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/events/{event_id}"))
                    .header(header::AUTHORIZATION, "Basic YWRtaW46c2VjcmV0")
                    .body(Body::empty())
                    .expect("valid request"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body should be readable");
        let payload: Value = serde_json::from_slice(&body).expect("valid json");
        assert_eq!(payload.get("id").and_then(Value::as_str), Some(event_id));
        assert_eq!(
            payload.get("body_size_bytes").and_then(Value::as_i64),
            Some(11)
        );
    }

    #[tokio::test]
    async fn events_pagination_is_stable_across_cursor_pages() {
        let store = MemoryStore::default();
        let app = router_with_state(
            AppState {
                store: Arc::new(store.clone()),
                source_configs: HashMap::new(),
                source_secrets: HashMap::new(),
                max_webhook_size_bytes: 5_242_880,
                replay_forward_headers: default_replay_forward_headers(),
                http_client: build_http_client(),
            },
            "admin".to_owned(),
            "secret".to_owned(),
            5_242_880,
        );

        for source in ["one", "two", "three"] {
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

        let page_1 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/events?limit=2")
                    .header(header::AUTHORIZATION, "Basic YWRtaW46c2VjcmV0")
                    .body(Body::empty())
                    .expect("valid request"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(page_1.status(), StatusCode::OK);
        let body = axum::body::to_bytes(page_1.into_body(), usize::MAX)
            .await
            .expect("response body should be readable");
        let payload: Value = serde_json::from_slice(&body).expect("valid json");
        let first_page_items = payload
            .get("items")
            .and_then(Value::as_array)
            .expect("items should be an array");
        assert_eq!(first_page_items.len(), 2);

        let cursor = payload
            .get("next_cursor")
            .and_then(Value::as_str)
            .expect("next cursor should exist")
            .to_owned();

        let page_2 = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/events?limit=2&cursor={cursor}").as_str())
                    .header(header::AUTHORIZATION, "Basic YWRtaW46c2VjcmV0")
                    .body(Body::empty())
                    .expect("valid request"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(page_2.status(), StatusCode::OK);
        let body = axum::body::to_bytes(page_2.into_body(), usize::MAX)
            .await
            .expect("response body should be readable");
        let payload: Value = serde_json::from_slice(&body).expect("valid json");
        let second_page_items = payload
            .get("items")
            .and_then(Value::as_array)
            .expect("items should be an array");
        assert_eq!(second_page_items.len(), 1);

        let first_page_ids = first_page_items
            .iter()
            .map(|item| item.get("id").and_then(Value::as_str).unwrap_or_default())
            .collect::<Vec<_>>();

        let second_page_ids = second_page_items
            .iter()
            .map(|item| item.get("id").and_then(Value::as_str).unwrap_or_default())
            .collect::<Vec<_>>();

        assert!(
            second_page_ids
                .iter()
                .all(|id| !first_page_ids.iter().any(|first| first == id)),
            "cursor pagination should not repeat ids across pages"
        );
    }

    #[tokio::test]
    async fn replay_forwards_exact_body_and_content_type() {
        let (destination, received) = spawn_test_receiver().await;
        let mut destinations = HashMap::new();
        destinations.insert(
            "stripe".to_owned(),
            SourceConfig {
                url: destination,
                timeout_ms: 10_000,
            },
        );

        let store = MemoryStore::default();
        let app = router_with_state(
            AppState {
                store: Arc::new(store.clone()),
                source_configs: destinations,
                source_secrets: HashMap::new(),
                max_webhook_size_bytes: 5_242_880,
                replay_forward_headers: default_replay_forward_headers(),
                http_client: build_http_client(),
            },
            "admin".to_owned(),
            "secret".to_owned(),
            5_242_880,
        );

        let payload = br#"{"type":"invoice.paid","ok":true}"#.to_vec();
        let ingest_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/hooks/stripe")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(payload.clone()))
                    .expect("valid request"),
            )
            .await
            .expect("ingest request should succeed");

        assert_eq!(ingest_response.status(), StatusCode::CREATED);

        let event_id = store.events.read().await[0].event.id;
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
            .expect("replay request should succeed");

        assert_eq!(replay_response.status(), StatusCode::OK);

        let (headers, body) = tokio::time::timeout(std::time::Duration::from_secs(2), received)
            .await
            .expect("receiver should be called")
            .expect("receiver should capture replay");
        assert_eq!(body, payload);
        assert_eq!(
            headers
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/json")
        );
        assert_eq!(
            headers
                .get("x-webhook-replay")
                .and_then(|v| v.to_str().ok()),
            Some("true")
        );
        assert_eq!(
            headers
                .get("x-original-event-id")
                .and_then(|v| v.to_str().ok()),
            Some(event_id.to_string().as_str())
        );

        let attempts = store.attempts.read().await;
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].response_status, Some(200));

        let statuses = store.statuses.read().await;
        assert_eq!(
            statuses.get(&event_id).copied(),
            Some(EventStatus::Delivered)
        );
    }

    #[tokio::test]
    async fn replay_forwards_allowlisted_headers_only() {
        let (destination, received) = spawn_test_receiver().await;
        let mut destinations = HashMap::new();
        destinations.insert(
            "github".to_owned(),
            SourceConfig {
                url: destination,
                timeout_ms: 10_000,
            },
        );

        let store = MemoryStore::default();
        let app = router_with_state(
            AppState {
                store: Arc::new(store.clone()),
                source_configs: destinations,
                source_secrets: HashMap::new(),
                max_webhook_size_bytes: 5_242_880,
                replay_forward_headers: default_replay_forward_headers(),
                http_client: build_http_client(),
            },
            "admin".to_owned(),
            "secret".to_owned(),
            5_242_880,
        );

        let ingest_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/hooks/github")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("x-github-event", "push")
                    .header("x-github-delivery", "delivery-1")
                    .header("user-agent", "GitHub-Hookshot/abc")
                    .header("x-signature", "do-not-forward")
                    .body(Body::from("{}"))
                    .expect("valid request"),
            )
            .await
            .expect("ingest request should succeed");

        assert_eq!(ingest_response.status(), StatusCode::CREATED);

        let event_id = store.events.read().await[0].event.id;
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
            .expect("replay request should succeed");

        assert_eq!(replay_response.status(), StatusCode::OK);

        let (headers, _) = tokio::time::timeout(std::time::Duration::from_secs(2), received)
            .await
            .expect("receiver should be called")
            .expect("receiver should capture replay");

        assert_eq!(
            headers.get("x-github-event").and_then(|v| v.to_str().ok()),
            Some("push")
        );
        assert_eq!(
            headers
                .get("x-github-delivery")
                .and_then(|v| v.to_str().ok()),
            Some("delivery-1")
        );
        assert_eq!(
            headers.get("user-agent").and_then(|v| v.to_str().ok()),
            Some("GitHub-Hookshot/abc")
        );
        assert!(headers.get("x-signature").is_none());
    }

    #[tokio::test]
    async fn replay_never_forwards_hop_by_hop_headers_even_when_allowlisted() {
        let (destination, received) = spawn_test_receiver().await;
        let mut destinations = HashMap::new();
        destinations.insert(
            "github".to_owned(),
            SourceConfig {
                url: destination,
                timeout_ms: 10_000,
            },
        );

        let store = MemoryStore::default();
        let app = router_with_state(
            AppState {
                store: Arc::new(store.clone()),
                source_configs: destinations,
                source_secrets: HashMap::new(),
                max_webhook_size_bytes: 5_242_880,
                replay_forward_headers: vec![
                    "connection".to_owned(),
                    "x-github-delivery".to_owned(),
                ],
                http_client: build_http_client(),
            },
            "admin".to_owned(),
            "secret".to_owned(),
            5_242_880,
        );

        let ingest_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/hooks/github")
                    .header("connection", "keep-alive")
                    .header("x-github-delivery", "delivery-2")
                    .body(Body::from("{}"))
                    .expect("valid request"),
            )
            .await
            .expect("ingest request should succeed");

        assert_eq!(ingest_response.status(), StatusCode::CREATED);

        let event_id = store.events.read().await[0].event.id;
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
            .expect("replay request should succeed");

        assert_eq!(replay_response.status(), StatusCode::OK);

        let (headers, _) = tokio::time::timeout(std::time::Duration::from_secs(2), received)
            .await
            .expect("receiver should be called")
            .expect("receiver should capture replay");

        assert!(headers.get("connection").is_none());
        assert_eq!(
            headers
                .get("x-github-delivery")
                .and_then(|v| v.to_str().ok()),
            Some("delivery-2")
        );
    }

    #[tokio::test]
    async fn ingest_persists_uri_query_string() {
        let store = MemoryStore::default();
        let app = router_with_state(
            AppState {
                store: Arc::new(store.clone()),
                source_configs: HashMap::new(),
                source_secrets: HashMap::new(),
                max_webhook_size_bytes: 1024,
                replay_forward_headers: default_replay_forward_headers(),
                http_client: build_http_client(),
            },
            "admin".to_owned(),
            "secret".to_owned(),
            1024,
        );

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/hooks/stripe?attempt=2&foo=bar")
                    .body(Body::from("{}"))
                    .expect("valid request"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), StatusCode::CREATED);

        let events = store.events.read().await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event.query.as_deref(), Some("attempt=2&foo=bar"));
    }
}
