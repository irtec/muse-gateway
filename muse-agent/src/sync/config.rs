use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum SyncRole {
    #[default]
    Disabled,
    Master,
    Replica,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct SyncConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub role: SyncRole,
    #[serde(default = "default_sync_dir")]
    pub sync_dir: String,
    #[serde(default = "default_master_url")]
    pub master_url: String,
    #[serde(default = "default_interval_secs")]
    pub interval_secs: u64,
    #[serde(default)]
    pub auth_token: Option<String>,
}

fn default_sync_dir() -> String {
    "/home/hatch/workspace/public".to_string()
}

fn default_master_url() -> String {
    "http://127.0.0.1:18081".to_string()
}

fn default_interval_secs() -> u64 {
    30
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            role: SyncRole::Disabled,
            sync_dir: default_sync_dir(),
            master_url: default_master_url(),
            interval_secs: default_interval_secs(),
            auth_token: None,
        }
    }
}
