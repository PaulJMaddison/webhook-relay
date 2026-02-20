use std::sync::Arc;

use webhook_relay::{api, config::AppConfig, db::pg::PgEventStore};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "webhook_relay=info,tower_http=info".into()),
        )
        .init();

    let config = AppConfig::from_env()?;
    let store = PgEventStore::connect(&config.database_url).await?;
    sqlx::migrate!("./migrations").run(store.pool()).await?;

    let app = api::router(api::AppState {
        store: Arc::new(store),
        client: reqwest::Client::new(),
    });

    let listener = tokio::net::TcpListener::bind(&config.bind_addr).await?;
    tracing::info!(addr = %config.bind_addr, "server listening");
    axum::serve(listener, app).await?;

    Ok(())
}
