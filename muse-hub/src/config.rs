use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize, Clone)]
pub struct HubConfig {
    pub server: ServerConfig,
    pub pool: PoolConfig,
    pub telegram: Option<TelegramConfig>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ServerConfig {
    pub host: String,   // "0.0.0.0"
    pub http_port: u16, // 18080 (public API endpoint for OpenAI / Anthropic)
    #[serde(default = "default_file_server_listen_port")]
    pub file_server_port: u16, // 18090 (port listener khusus active-standby file-server)
    pub api_key: Option<String>, // Optional global API key for clients
}

fn default_file_server_listen_port() -> u16 {
    18090
}

#[derive(Debug, Deserialize, Clone)]
pub struct WorkerDefinition {
    #[serde(default)]
    pub name: String,
    pub url: String,
    pub ssh_port: Option<u16>,
    /// Internal proxy port for worker File Manager if configured (e.g. 18091 for VM1, 18092 for VM2, etc.)
    pub file_server_port: Option<u16>,
    /// Command hook executed once when this worker drops offline
    pub on_offline_hook: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct PoolConfig {
    #[serde(default)]
    pub targets: Vec<String>, // e.g. ["http://127.0.0.1:18081", "http://127.0.0.1:18082"]
    #[serde(default)]
    pub workers: Vec<WorkerDefinition>, // Explicit structured workers
    #[serde(default = "default_health_check_interval_secs")]
    pub health_check_interval_secs: u64,
    #[serde(default = "default_request_timeout_secs")]
    pub request_timeout_secs: u64,
}

impl PoolConfig {
    /// Returns structured workers either from explicit `workers` list or auto-derived from `targets`
    pub fn resolved_workers(&self) -> Vec<WorkerDefinition> {
        if !self.workers.is_empty() {
            return self.workers.clone();
        }

        self.targets
            .iter()
            .enumerate()
            .map(|(idx, target)| {
                let name = format!("VM-{:02}", idx + 1);
                let ssh_port = target
                    .rsplit(':')
                    .next()
                    .and_then(|p| p.trim_matches('/').parse::<u16>().ok())
                    .map(|p| if p >= 18080 { p - 18080 + 2220 } else { 2221 });

                let file_server_port = target
                    .rsplit(':')
                    .next()
                    .and_then(|p| p.trim_matches('/').parse::<u16>().ok())
                    .map(|p| if p >= 18080 { p - 18080 + 18090 } else { 18091 });

                WorkerDefinition {
                    name,
                    url: target.clone(),
                    ssh_port,
                    file_server_port,
                    on_offline_hook: None,
                }
            })
            .collect()
    }
}

fn default_health_check_interval_secs() -> u64 {
    5
}

fn default_request_timeout_secs() -> u64 {
    120
}

#[derive(Debug, Deserialize, Clone)]
pub struct TelegramConfig {
    pub enabled: bool,
    pub bot_token: String,
    pub chat_id: String,
}

impl HubConfig {
    pub fn load_from_file<P: AsRef<Path>>(
        path: P,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let content = std::fs::read_to_string(path)?;
        let config: HubConfig = toml::from_str(&content)?;
        Ok(config)
    }

    pub fn default_example() -> Self {
        Self {
            server: ServerConfig {
                host: "0.0.0.0".to_string(),
                http_port: 18080,
                file_server_port: 18090,
                api_key: None,
            },
            pool: PoolConfig {
                targets: vec![
                    "http://127.0.0.1:18081".to_string(),
                    "http://127.0.0.1:18082".to_string(),
                ],
                workers: vec![],
                health_check_interval_secs: 5,
                request_timeout_secs: 120,
            },
            telegram: Some(TelegramConfig {
                enabled: false,
                bot_token: "".to_string(),
                chat_id: "".to_string(),
            }),
        }
    }
}
