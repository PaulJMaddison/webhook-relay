use std::{collections::HashMap, env, error::Error};

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub database_url: String,
    pub bind_addr: String,
    pub admin_basic_user: String,
    pub admin_basic_pass: String,
    pub source_destinations: HashMap<String, String>,
}

impl AppConfig {
    pub fn from_env() -> Result<Self, Box<dyn Error>> {
        let database_url = env::var("DATABASE_URL")?;
        let bind_addr = env::var("BIND_ADDR")?;
        let admin_basic_user = env::var("ADMIN_BASIC_USER")?;
        let admin_basic_pass = env::var("ADMIN_BASIC_PASS")?;
        let source_destinations = match env::var("SOURCE_DESTINATIONS") {
            Ok(raw) => serde_json::from_str(&raw)?,
            Err(env::VarError::NotPresent) => HashMap::new(),
            Err(err) => return Err(Box::new(err)),
        };

        Ok(Self {
            database_url,
            bind_addr,
            admin_basic_user,
            admin_basic_pass,
            source_destinations,
        })
    }
}
