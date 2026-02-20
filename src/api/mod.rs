use std::sync::Arc;

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, Method, StatusCode, Uri},
    routing::{get, post},
    Json, Router,
};
use url::Url;
use uuid::Uuid;

use crate::{
    db::EventStore,
    domain::{IngestResponse, NewEvent, ReplayRequest, ReplayResponse, StoredEvent},
    errors::AppError,
};

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<dyn EventStore>,
    pub client: reqwest::Client,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/hooks/:source", post(ingest))
        .route("/events", get(list_events))
        .route("/events/:id", get(get_event))
        .route("/events/:id/replay", post(replay_event))
        .with_state(state)
}

async fn ingest(
    State(state): State<AppState>,
    Path(source): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<IngestResponse>), AppError> {
    let header_json = headers_to_json(&headers)?;

    let event = state
        .store
        .create_event(NewEvent {
            source,
            method: method.to_string(),
            path: uri.path().to_string(),
            query: uri.query().map(ToOwned::to_owned),
            headers: header_json,
            body: body.to_vec(),
        })
        .await?;

    Ok((StatusCode::ACCEPTED, Json(IngestResponse { id: event.id })))
}

async fn list_events(State(state): State<AppState>) -> Result<Json<Vec<StoredEvent>>, AppError> {
    Ok(Json(state.store.list_events().await?))
}

async fn get_event(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<StoredEvent>, AppError> {
    Ok(Json(state.store.get_event(id).await?))
}

async fn replay_event(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(payload): Json<ReplayRequest>,
) -> Result<Json<ReplayResponse>, AppError> {
    let event = state.store.get_event(id).await?;
    let replay_url = build_replay_url(&payload.target_url, &event.path, event.query.as_deref())?;

    let method: reqwest::Method = event
        .method
        .parse()
        .map_err(|_| AppError::Internal("stored event contains invalid HTTP method".to_string()))?;

    let mut request = state.client.request(method, replay_url).body(event.body);

    if let serde_json::Value::Object(map) = event.headers {
        for (name, value) in map {
            if name.eq_ignore_ascii_case("host") || name.eq_ignore_ascii_case("content-length") {
                continue;
            }
            if let Some(v) = value.as_str() {
                request = request.header(&name, v);
            }
        }
    }

    let response = request.send().await?;

    Ok(Json(ReplayResponse {
        status_code: response.status().as_u16(),
    }))
}

fn headers_to_json(headers: &HeaderMap) -> Result<serde_json::Value, AppError> {
    let mut map = serde_json::Map::new();
    for (name, value) in headers {
        let value_str = value
            .to_str()
            .map_err(|_| AppError::BadRequest("header value is not valid UTF-8".to_string()))?;
        map.insert(
            name.to_string(),
            serde_json::Value::String(value_str.to_string()),
        );
    }
    Ok(serde_json::Value::Object(map))
}

fn build_replay_url(target_url: &str, path: &str, query: Option<&str>) -> Result<Url, AppError> {
    let mut url = Url::parse(target_url)
        .map_err(|err| AppError::BadRequest(format!("invalid target_url: {err}")))?;
    url.set_path(path);
    url.set_query(query);
    Ok(url)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
        routing::any,
        Router,
    };
    use serde_json::json;
    use tower::ServiceExt;

    use crate::db::{memory::MemoryEventStore, EventStore};

    use super::{router, AppState};

    fn test_app(store: MemoryEventStore) -> Router {
        router(AppState {
            store: Arc::new(store),
            client: reqwest::Client::builder().no_proxy().build().unwrap(),
        })
    }

    #[tokio::test]
    async fn ingest_and_get_event_round_trip() {
        let store = MemoryEventStore::default();
        let app = test_app(store.clone());

        let ingest = app
            .clone()
            .oneshot(
                Request::post("/hooks/stripe?attempt=1")
                    .header("content-type", "application/json")
                    .body(Body::from("{\"ok\":true}"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(ingest.status(), StatusCode::ACCEPTED);
        let body = to_bytes(ingest.into_body(), usize::MAX).await.unwrap();
        let ingest_value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = ingest_value["id"].as_str().unwrap();

        let get_resp = app
            .oneshot(
                Request::get(format!("/events/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(get_resp.status(), StatusCode::OK);
        let payload = to_bytes(get_resp.into_body(), usize::MAX).await.unwrap();
        let event: serde_json::Value = serde_json::from_slice(&payload).unwrap();
        assert_eq!(event["source"], "stripe");
        assert_eq!(event["method"], "POST");
        assert_eq!(event["query"], "attempt=1");
        assert_eq!(
            event["body"],
            json!([123, 34, 111, 107, 34, 58, 116, 114, 117, 101, 125])
        );
    }

    #[tokio::test]
    async fn replay_event_sends_same_payload() {
        let captured_body = Arc::new(Mutex::new(Vec::<u8>::new()));
        let captured_body_clone = Arc::clone(&captured_body);

        let downstream = Router::new().route(
            "/*path",
            any(move |body: axum::body::Bytes| {
                let captured = Arc::clone(&captured_body_clone);
                async move {
                    *captured.lock().unwrap() = body.to_vec();
                    StatusCode::NO_CONTENT
                }
            }),
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, downstream).await.unwrap();
        });

        let store = MemoryEventStore::default();
        let app = test_app(store.clone());

        let ingest = app
            .clone()
            .oneshot(
                Request::post("/hooks/github")
                    .header("x-test", "1")
                    .body(Body::from("payload"))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(ingest.into_body(), usize::MAX).await.unwrap();
        let ingest_value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = ingest_value["id"].as_str().unwrap();

        let stored = store.get_event(id.parse().unwrap()).await.unwrap();
        assert_eq!(stored.body, b"payload");

        let replay = app
            .oneshot(
                Request::post(format!("/events/{id}/replay"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"target_url": format!("http://{addr}")}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(replay.status(), StatusCode::OK);
        let replay_body = to_bytes(replay.into_body(), usize::MAX).await.unwrap();
        let replay_value: serde_json::Value = serde_json::from_slice(&replay_body).unwrap();
        assert_eq!(replay_value["status_code"], 204);
        assert_eq!(&*captured_body.lock().unwrap(), b"payload");
    }
}
