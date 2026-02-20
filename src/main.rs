use webhook_relay::{api, config::AppConfig, db};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "webhook_relay=info".into()),
        )
        .init();

    let config = AppConfig::from_env()?;

    let pool = db::connect(&config.database_url).await?;
    db::run_migrations(&pool).await?;

    tracing::info!(
        bind_addr = %config.bind_addr,
        source_destination_count = config.source_destinations.len(),
        "starting webhook-relay"
    );

    let app = api::router(pool, config.admin_basic_user, config.admin_basic_pass);
    let listener = tokio::net::TcpListener::bind(&config.bind_addr).await?;
    tracing::info!(address = %config.bind_addr, "server listening");

    axum::serve(listener, app).await?;

    Ok(())
}
