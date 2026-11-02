use std::path::PathBuf;

#[derive(Debug, serde::Deserialize, Clone)]
pub struct FileManagerConfig {
    #[serde(default = "default_fm_enabled")]
    pub enabled: bool,
    #[serde(default = "default_fm_host")]
    pub host: String,
    #[serde(default = "default_fm_port")]
    pub port: u16,
    #[serde(default = "default_fm_root")]
    pub root_dir: String,
    #[serde(default = "default_fm_user")]
    pub auth_user: String,
    #[serde(default = "default_fm_salt")]
    pub auth_salt: String,
    #[serde(default = "default_fm_hash")]
    pub auth_hash: String,
    #[serde(default)]
    pub auth_token: Option<String>,
}

fn default_fm_enabled() -> bool {
    true
}

fn default_fm_host() -> String {
    "127.0.0.1".to_string()
}

fn default_fm_port() -> u16 {
    8080
}

fn default_fm_root() -> String {
    "/home/hatch/workspace/public".to_string()
}

fn default_fm_user() -> String {
    "admin".to_string()
}

fn default_fm_salt() -> String {
    "00000000000000000000000000000000".to_string()
}

fn default_fm_hash() -> String {
    "0000000000000000000000000000000000000000000000000000000000000000".to_string()
}

impl Default for FileManagerConfig {
    fn default() -> Self {
        Self {
            enabled: default_fm_enabled(),
            host: default_fm_host(),
            port: default_fm_port(),
            root_dir: default_fm_root(),
            auth_user: default_fm_user(),
            auth_salt: default_fm_salt(),
            auth_hash: default_fm_hash(),
            auth_token: None,
        }
    }
}

impl FileManagerConfig {
    pub fn root_path(&self) -> PathBuf {
        PathBuf::from(&self.root_dir)
    }

    /// True when the credentials are still the shipped placeholders. Such a node
    /// cannot serve a login that works on the other workers, so the hub must not
    /// route file traffic to it.
    pub fn has_placeholder_credentials(&self) -> bool {
        let zero_salt = self.auth_salt.chars().all(|c| c == '0');
        let zero_hash = self.auth_hash.chars().all(|c| c == '0');
        self.auth_salt.is_empty() || self.auth_hash.is_empty() || zero_salt || zero_hash
    }
}
