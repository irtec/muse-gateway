use hmac::Hmac;
use pbkdf2::pbkdf2;
use sha2::Sha256;
use std::path::{Path, PathBuf};
use subtle::ConstantTimeEq;

use crate::file_manager::config::FileManagerConfig;

pub fn verify_basic_auth(auth_header: Option<&str>, config: &FileManagerConfig) -> bool {
    let auth = match auth_header {
        Some(a) if a.starts_with("Basic ") => &a[6..],
        _ => return false,
    };

    let decoded =
        match base64::Engine::decode(&base64::engine::general_purpose::STANDARD, auth.trim()) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(s) => s,
                Err(_) => return false,
            },
            Err(_) => return false,
        };

    let mut parts = decoded.splitn(2, ':');
    let user = parts.next().unwrap_or("");
    let pass = parts.next().unwrap_or("");

    if user != config.auth_user {
        return false;
    }

    let salt = match hex::decode(&config.auth_salt) {
        Ok(s) => s,
        Err(_) => return false,
    };

    let mut derived_key = [0u8; 32];
    let _ = pbkdf2::<Hmac<Sha256>>(pass.as_bytes(), &salt, 200_000, &mut derived_key);

    let expected_hash = match hex::decode(&config.auth_hash) {
        Ok(h) => h,
        Err(_) => return false,
    };

    if expected_hash.len() != 32 {
        return false;
    }

    derived_key.ct_eq(&expected_hash).into()
}

pub fn safe_resolve(root: &Path, rel_path: &str) -> Option<PathBuf> {
    let clean_rel = rel_path.trim_start_matches('/');
    let target = root.join(clean_rel);

    // Normalize target
    let canonical_root = match root.canonicalize() {
        Ok(r) => r,
        Err(_) => return None,
    };

    // If target exists, canonicalize directly
    if target.exists() {
        if let Ok(canon) = target.canonicalize() {
            if canon.starts_with(&canonical_root) {
                return Some(canon);
            }
        }
        return None;
    }

    // If target doesn't exist yet (for write/create), check parent
    if let Some(parent) = target.parent() {
        if let Ok(canon_parent) = parent.canonicalize() {
            if canon_parent.starts_with(&canonical_root) {
                if let Some(filename) = target.file_name() {
                    return Some(canon_parent.join(filename));
                }
            }
        }
    }

    None
}

pub fn human_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}
