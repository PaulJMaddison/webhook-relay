use std::{collections::HashMap, env, error::Error};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceConfig {
    pub url: String,
    pub timeout_ms: u64,
}

#[derive(Debug, serde::Deserialize)]
struct RawSourceConfig {
    url: String,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub database_url: String,
    pub bind_addr: String,
    pub admin_basic_user: String,
    pub admin_basic_pass: String,
    pub source_configs: HashMap<String, SourceConfig>,
    pub source_secrets: HashMap<String, String>,
    pub max_webhook_size_bytes: usize,
    pub replay_forward_headers: Vec<String>,
}

const DEFAULT_REPLAY_FORWARD_HEADERS: [&str; 5] = [
    "content-type",
    "user-agent",
    "x-github-event",
    "x-github-delivery",
    "stripe-signature",
];
const DEFAULT_SOURCE_TIMEOUT_MS: u64 = 10_000;

impl AppConfig {
    pub fn from_env() -> Result<Self, Box<dyn Error>> {
        let database_url = env::var("DATABASE_URL")?;
        let bind_addr = env::var("BIND_ADDR")?;
        let admin_basic_user = env::var("ADMIN_BASIC_USER")?;
        let admin_basic_pass = env::var("ADMIN_BASIC_PASS")?;
        let source_configs = match env::var("SOURCE_CONFIGS") {
            Ok(raw) => {
                let raw_configs: HashMap<String, RawSourceConfig> = serde_json::from_str(&raw)?;
                raw_configs
                    .into_iter()
                    .map(|(source, raw)| {
                        (
                            source,
                            SourceConfig {
                                url: raw.url,
                                timeout_ms: raw.timeout_ms.unwrap_or(DEFAULT_SOURCE_TIMEOUT_MS),
                            },
                        )
                    })
                    .collect()
            }
            Err(env::VarError::NotPresent) => match env::var("SOURCE_DESTINATIONS") {
                Ok(raw) => {
                    let destinations: HashMap<String, String> = serde_json::from_str(&raw)?;
                    destinations
                        .into_iter()
                        .map(|(source, url)| {
                            (
                                source,
                                SourceConfig {
                                    url,
                                    timeout_ms: DEFAULT_SOURCE_TIMEOUT_MS,
                                },
                            )
                        })
                        .collect()
                }
                Err(env::VarError::NotPresent) => HashMap::new(),
                Err(err) => return Err(Box::new(err)),
            },
            Err(err) => return Err(Box::new(err)),
        };
        let source_secrets = match env::var("SOURCE_SECRETS") {
            Ok(raw) => serde_json::from_str(&raw)?,
            Err(env::VarError::NotPresent) => HashMap::new(),
            Err(err) => return Err(Box::new(err)),
        };
        let max_webhook_size_bytes = match env::var("MAX_WEBHOOK_SIZE_BYTES") {
            Ok(raw) => raw.parse::<usize>()?,
            Err(env::VarError::NotPresent) => 5_242_880,
            Err(err) => return Err(Box::new(err)),
        };
        let replay_forward_headers = match env::var("REPLAY_FORWARD_HEADERS") {
            Ok(raw) => raw
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_ascii_lowercase)
                .collect(),
            Err(env::VarError::NotPresent) => DEFAULT_REPLAY_FORWARD_HEADERS
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
            Err(err) => return Err(Box::new(err)),
        };

        Ok(Self {
            database_url,
            bind_addr,
            admin_basic_user,
            admin_basic_pass,
            source_configs,
            source_secrets,
            max_webhook_size_bytes,
            replay_forward_headers,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn parses_source_configs_with_optional_timeout() {
        let _guard = env_lock().lock().expect("env lock should be acquired");
        env::set_var("DATABASE_URL", "postgres://localhost/db");
        env::set_var("BIND_ADDR", "127.0.0.1:3000");
        env::set_var("ADMIN_BASIC_USER", "admin");
        env::set_var("ADMIN_BASIC_PASS", "secret");
        env::set_var(
            "SOURCE_CONFIGS",
            r#"{"stripe":{"url":"https://stripe.test","timeout_ms":5000},"github":{"url":"https://github.test"}}"#,
        );
        env::remove_var("SOURCE_DESTINATIONS");

        let config = AppConfig::from_env().expect("config should parse");

        assert_eq!(config.source_configs["stripe"].url, "https://stripe.test");
        assert_eq!(config.source_configs["stripe"].timeout_ms, 5000);
        assert_eq!(config.source_configs["github"].url, "https://github.test");
        assert_eq!(
            config.source_configs["github"].timeout_ms,
            DEFAULT_SOURCE_TIMEOUT_MS
        );
    }

    #[test]
    fn falls_back_to_source_destinations_with_default_timeout() {
        let _guard = env_lock().lock().expect("env lock should be acquired");
        env::set_var("DATABASE_URL", "postgres://localhost/db");
        env::set_var("BIND_ADDR", "127.0.0.1:3000");
        env::set_var("ADMIN_BASIC_USER", "admin");
        env::set_var("ADMIN_BASIC_PASS", "secret");
        env::remove_var("SOURCE_CONFIGS");
        env::set_var(
            "SOURCE_DESTINATIONS",
            r#"{"stripe":"https://stripe.example"}"#,
        );

        let config = AppConfig::from_env().expect("config should parse");

        assert_eq!(
            config.source_configs["stripe"].url,
            "https://stripe.example"
        );
        assert_eq!(
            config.source_configs["stripe"].timeout_ms,
            DEFAULT_SOURCE_TIMEOUT_MS
        );
    }
}
