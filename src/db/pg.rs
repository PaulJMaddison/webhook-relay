use async_trait::async_trait;
use sqlx::{postgres::PgPoolOptions, PgPool};

use crate::{
    db::EventStore,
    domain::{NewEvent, StoredEvent},
    errors::AppError,
};

#[derive(Clone)]
pub struct PgEventStore {
    pool: PgPool,
}

impl PgEventStore {
    pub async fn connect(database_url: &str) -> Result<Self, AppError> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await?;
        Ok(Self { pool })
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

#[async_trait]
impl EventStore for PgEventStore {
    async fn create_event(&self, event: NewEvent) -> Result<StoredEvent, AppError> {
        let created = sqlx::query_as::<_, StoredEvent>(
            r#"
            INSERT INTO events (source, method, path, query, headers, body)
            VALUES ($1, $2, $3, $4, $5, $6)
            RETURNING id, source, method, path, query, headers, body, created_at
            "#,
        )
        .bind(event.source)
        .bind(event.method)
        .bind(event.path)
        .bind(event.query)
        .bind(event.headers)
        .bind(event.body)
        .fetch_one(&self.pool)
        .await?;

        Ok(created)
    }

    async fn list_events(&self) -> Result<Vec<StoredEvent>, AppError> {
        let events = sqlx::query_as::<_, StoredEvent>(
            r#"
            SELECT id, source, method, path, query, headers, body, created_at
            FROM events
            ORDER BY created_at DESC
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(events)
    }

    async fn get_event(&self, id: uuid::Uuid) -> Result<StoredEvent, AppError> {
        let event = sqlx::query_as::<_, StoredEvent>(
            r#"
            SELECT id, source, method, path, query, headers, body, created_at
            FROM events
            WHERE id = $1
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        event.ok_or(AppError::NotFound)
    }
}
