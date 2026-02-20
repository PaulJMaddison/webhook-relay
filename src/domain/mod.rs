use std::collections::HashMap;

use serde::{Deserialize, Serialize};

pub type SourceDestinations = HashMap<String, String>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "event_status", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum EventStatus {
    Received,
    Delivered,
    Failed,
    Dead,
}

impl EventStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Received => "received",
            Self::Delivered => "delivered",
            Self::Failed => "failed",
            Self::Dead => "dead",
        }
    }
}
