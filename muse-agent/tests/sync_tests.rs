use muse_agent::sync::{compute_sha256, generate_manifest, safe_resolve_sync};
use std::fs::{create_dir_all, File};
use std::io::Write;
use tempfile::tempdir;

#[test]
fn test_sync_manifest_generation() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    let sub = root.join("files");
    create_dir_all(&sub).unwrap();

    let mut f1 = File::create(root.join("hello.txt")).unwrap();
    f1.write_all(b"Hello World").unwrap();

    let mut f2 = File::create(sub.join("nested.bin")).unwrap();
    f2.write_all(b"Binary Data 12345").unwrap();

    // Temporary files (.tmp) should be ignored automatically
    let mut f_tmp = File::create(root.join("test.tmp")).unwrap();
    f_tmp.write_all(b"Should be ignored").unwrap();

    let manifest = generate_manifest(root).unwrap();
    assert_eq!(manifest.files.len(), 2);

    let paths: Vec<String> = manifest.files.iter().map(|f| f.rel_path.clone()).collect();
    assert!(paths.contains(&"hello.txt".to_string()));
    assert!(paths.contains(&"files/nested.bin".to_string()));
    assert!(!paths.contains(&"test.tmp".to_string()));

    assert!(!manifest.version_hash.is_empty());
}

#[test]
fn test_sync_sha256_integrity() {
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("data.txt");

    let mut file = File::create(&file_path).unwrap();
    file.write_all(b"Test SHA-256 Consistency").unwrap();

    let hash1 = compute_sha256(&file_path).unwrap();
    let hash2 = compute_sha256(&file_path).unwrap();
    assert_eq!(hash1, hash2);

    // Modify file content, hash must change
    let mut file2 = File::create(&file_path).unwrap();
    file2.write_all(b"Different Content").unwrap();
    let hash3 = compute_sha256(&file_path).unwrap();
    assert_ne!(hash1, hash3);
}

#[test]
fn test_safe_resolve_sync() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    assert!(safe_resolve_sync(root, "../secret.txt").is_none());
    assert!(safe_resolve_sync(root, "/etc/passwd").is_none());
    assert!(safe_resolve_sync(root, "folder/../../etc").is_none());

    let resolved = safe_resolve_sync(root, "docs/readme.md").unwrap();
    assert_eq!(resolved, root.join("docs").join("readme.md"));
}
