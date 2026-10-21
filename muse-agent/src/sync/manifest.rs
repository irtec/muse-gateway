use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use tracing::warn;

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub rel_path: String,
    pub size: u64,
    pub mtime: u64,
    pub sha256: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub version_hash: String,
    pub files: Vec<FileEntry>,
}

pub fn compute_sha256(path: &Path) -> std::io::Result<String> {
    let file = File::open(path)?;
    let mut reader = BufReader::with_capacity(64 * 1024, file);
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];

    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }

    Ok(hex::encode(hasher.finalize()))
}

pub fn generate_manifest(root_dir: &Path) -> std::io::Result<Manifest> {
    let mut entries = Vec::new();
    if root_dir.exists() && root_dir.is_dir() {
        scan_dir_recursive(root_dir, root_dir, &mut entries)?;
    }

    // Sort by rel_path for deterministic consistency
    entries.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));

    // Compute global version_hash from all sha256 + rel_path entries
    let mut global_hasher = Sha256::new();
    for f in &entries {
        global_hasher.update(f.rel_path.as_bytes());
        global_hasher.update(f.sha256.as_bytes());
        global_hasher.update(f.size.to_be_bytes());
    }
    let version_hash = hex::encode(global_hasher.finalize());

    Ok(Manifest {
        version_hash,
        files: entries,
    })
}

fn scan_dir_recursive(
    current: &Path,
    root: &Path,
    entries: &mut Vec<FileEntry>,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;

        if file_type.is_dir() {
            scan_dir_recursive(&path, root, entries)?;
        } else if file_type.is_file() {
            let rel_path = match path.strip_prefix(root) {
                Ok(p) => p.to_string_lossy().to_string(),
                Err(_) => continue,
            };

            // Ignore temporary files such as .tmp
            if rel_path.ends_with(".tmp") || rel_path.starts_with('.') {
                continue;
            }

            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(e) => {
                    warn!("Failed to read metadata for {:?}: {}", path, e);
                    continue;
                }
            };

            let size = meta.len();
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);

            let sha256 = match compute_sha256(&path) {
                Ok(h) => h,
                Err(e) => {
                    warn!("Failed to compute sha256 for {:?}: {}", path, e);
                    continue;
                }
            };

            entries.push(FileEntry {
                rel_path,
                size,
                mtime,
                sha256,
            });
        }
    }

    Ok(())
}

pub fn safe_resolve_sync(root_dir: &Path, rel_path: &str) -> Option<PathBuf> {
    if rel_path.contains("..") || rel_path.starts_with('/') || rel_path.starts_with('\\') {
        return None;
    }

    let mut target = root_dir.to_path_buf();
    for part in rel_path.split(['/', '\\']) {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            return None;
        }
        target.push(part);
    }

    Some(target)
}
