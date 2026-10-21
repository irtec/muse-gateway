use crate::file_manager::FileManagerConfig;
use crate::sync::SyncConfig;
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize, Clone)]
pub struct AgentConfig {
    pub vm: VmConfig,
    pub server: ServerConfig,
    pub egress: Option<EgressConfig>,
    pub inference: InferenceConfig,
    #[serde(default)]
    pub supervisor: SupervisorConfig,
    #[serde(default)]
    pub file_manager: FileManagerConfig,
    #[serde(default)]
    pub sync: SyncConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct SupervisorConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_check_interval")]
    pub check_interval_secs: u64,
    #[serde(default = "default_base_dir")]
    pub base_dir: String,
    #[serde(default = "default_frpc_bin")]
    pub frpc_bin: String,
    #[serde(default = "default_frpc_config")]
    pub frpc_config: String,
}

fn default_true() -> bool {
    true
}

fn default_check_interval() -> u64 {
    5
}

fn default_base_dir() -> String {
    "/home/hatch/workspace/agent".to_string()
}

fn default_frpc_bin() -> String {
    "/home/hatch/workspace/agent/bin/frpc".to_string()
}

fn default_frpc_config() -> String {
    "/home/hatch/workspace/agent/config/frpc.toml".to_string()
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            check_interval_secs: 5,
            base_dir: default_base_dir(),
            frpc_bin: default_frpc_bin(),
            frpc_config: default_frpc_config(),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct VmConfig {
    pub id: String,    // e.g. "vm1", "vm2", etc.
    pub token: String, // Auth token expected for incoming requests
}

#[derive(Debug, Deserialize, Clone)]
pub struct ServerConfig {
    pub host: String, // "127.0.0.1"
    pub port: u16,    // e.g. 11436
}

#[derive(Debug, Deserialize, Clone)]
pub struct EgressConfig {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub auth: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct InferenceConfig {
    pub socket_path: String, // "/run/hatch/sandbox/space-inference.sock"
    #[serde(default = "default_media_socket_path")]
    pub media_socket_path: String, // "/run/hatch/sandbox/space-media.sock"
    pub max_frame_size_mb: Option<usize>,
}

fn default_media_socket_path() -> String {
    "/run/hatch/sandbox/space-media.sock".to_string()
}

impl AgentConfig {
    pub fn load_from_file<P: AsRef<Path>>(path: P) -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path)?;
        let config: AgentConfig = toml::from_str(&content)?;
        Ok(config)
    }

    pub fn default_example() -> Self {
        Self {
            vm: VmConfig {
                id: "VM1".to_string(),
                token: "muse-cluster-secret-key".to_string(),
            },
            server: ServerConfig {
                host: "127.0.0.1".to_string(),
                port: 8080,
            },
            egress: None,
            inference: InferenceConfig {
                socket_path: "/run/hatch/sandbox/space-inference.sock".to_string(),
                media_socket_path: default_media_socket_path(),
                max_frame_size_mb: Some(32),
            },
            supervisor: SupervisorConfig::default(),
            file_manager: FileManagerConfig::default(),
            sync: SyncConfig::default(),
        }
    }
}
