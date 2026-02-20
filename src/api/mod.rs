use axum::{routing::get, Json, Router};
use serde::Serialize;

pub fn router() -> Router {
    Router::new().route("/healthz", get(healthz))
}

#[derive(Debug, Serialize)]
struct HealthzResponse {
    status: &'static str,
}

async fn healthz() -> Json<HealthzResponse> {
    Json(HealthzResponse { status: "ok" })
}
