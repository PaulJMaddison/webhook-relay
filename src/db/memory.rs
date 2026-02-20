use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::{
    db::EventStore,
    domain::{NewEvent, StoredEvent},
    errors::AppError,
};

#[derive(Clone, Default)]
pub struct MemoryEventStore {
    events: Arc<RwLock<Vec<StoredEvent>>>,
}

#[async_trait]
impl EventStore for MemoryEventStore {
    async fn create_event(&self, event: NewEvent) -> Result<StoredEvent, AppError> {
        let stored = StoredEvent {
            id: Uuid::new_v4(),
            source: event.source,
            method: event.method,
            path: event.path,
            query: event.query,
            headers: event.headers,
            body: event.body,
            created_at: chrono::Utc::now(),
        };
        self.events.write().await.push(stored.clone());
        Ok(stored)
    }

    async fn list_events(&self) -> Result<Vec<StoredEvent>, AppError> {
        let mut events = self.events.read().await.clone();
        events.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(events)
    }

    async fn get_event(&self, id: Uuid) -> Result<StoredEvent, AppError> {
        let maybe = self
            .events
            .read()
            .await
            .iter()
            .find(|event| event.id == id)
            .cloned();

        maybe.ok_or_else(|| AppError::not_found(format!("event not found: {id}")))
    }
}
