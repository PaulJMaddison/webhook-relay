use async_trait::async_trait;

use crate::{
    domain::{NewEvent, StoredEvent},
    errors::AppError,
};

pub mod memory;
pub mod pg;

#[async_trait]
pub trait EventStore: Send + Sync {
    async fn create_event(&self, event: NewEvent) -> Result<StoredEvent, AppError>;
    async fn list_events(&self) -> Result<Vec<StoredEvent>, AppError>;
    async fn get_event(&self, id: uuid::Uuid) -> Result<StoredEvent, AppError>;
}
