use webhook_relay::{db, domain::EventStatus};

#[derive(sqlx::FromRow)]
struct EventStatusRow {
    status: EventStatus,
}

#[tokio::test]
async fn postgres_event_status_round_trip_and_default() -> Result<(), Box<dyn std::error::Error>> {
    let Some(database_url) = std::env::var("DATABASE_URL").ok() else {
        eprintln!("skipping integration test: DATABASE_URL is not set");
        return Ok(());
    };

    let pool = db::connect(&database_url).await?;
    db::run_migrations(&pool).await?;

    let source_prefix = format!("status-it-{}", uuid::Uuid::new_v4());

    sqlx::query(
        r#"
        INSERT INTO events (source, method, path, headers, body, status)
        VALUES ($1, 'POST', '/hooks/test', '{}'::jsonb, '\\x00'::bytea, $2)
        "#,
    )
    .bind(format!("{source_prefix}-explicit"))
    .bind(EventStatus::Failed)
    .execute(&pool)
    .await?;

    sqlx::query(
        r#"
        INSERT INTO events (source, method, path, headers, body, status)
        VALUES ($1, 'POST', '/hooks/test', '{}'::jsonb, '\\x02'::bytea, $2)
        "#,
    )
    .bind(format!("{source_prefix}-dead"))
    .bind(EventStatus::Dead)
    .execute(&pool)
    .await?;

    sqlx::query(
        r#"
        INSERT INTO events (source, method, path, headers, body)
        VALUES ($1, 'POST', '/hooks/test', '{}'::jsonb, '\\x01'::bytea)
        "#,
    )
    .bind(format!("{source_prefix}-default"))
    .execute(&pool)
    .await?;

    let rows = sqlx::query_as::<_, EventStatusRow>(
        r#"
        SELECT status
        FROM events
        WHERE source LIKE $1
        ORDER BY source ASC
        "#,
    )
    .bind(format!("{source_prefix}%"))
    .fetch_all(&pool)
    .await?;

    let statuses = rows.into_iter().map(|row| row.status).collect::<Vec<_>>();
    assert_eq!(
        statuses,
        vec![
            EventStatus::Dead,
            EventStatus::Received,
            EventStatus::Failed
        ]
    );

    sqlx::query("DELETE FROM events WHERE source LIKE $1")
        .bind(format!("{source_prefix}%"))
        .execute(&pool)
        .await?;

    Ok(())
}
