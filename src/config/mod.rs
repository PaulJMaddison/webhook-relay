#[derive(Debug, Clone)]
pub struct AppConfig {
    pub database_url: String,
    pub bind_addr: String,
}

impl AppConfig {
    pub fn from_env() -> Result<Self, std::env::VarError> {
        let database_url = std::env::var("DATABASE_URL")?;
        let bind_addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
        Ok(Self {
            database_url,
            bind_addr,
        })
    }
}
