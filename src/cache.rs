//! BLAKE3-based build caching.
//!
//! Axiom writes a small JSON manifest (`.axiom.cache`) describing the config
//! file and every resolved input file by their BLAKE3 hashes. On the next run
//! those hashes are recomputed and compared; when nothing changed, code
//! generation is skipped entirely.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Persisted cache manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheManifest {
    /// BLAKE3 hash of the `axiom.json` configuration file.
    pub config_hash: String,
    /// BLAKE3 hashes of every resolved input file, keyed by path.
    pub file_hashes: BTreeMap<String, String>,
}

/// Compute the BLAKE3 hex hash of a file's contents.
pub fn compute_file_hash(path: &Path) -> Result<String, io::Error> {
    let mut hasher = blake3::Hasher::new();
    let mut file = std::fs::File::open(path)?;
    std::io::copy(&mut file, &mut hasher)?;
    Ok(hasher.finalize().to_hex().to_string())
}

/// Return `true` when the cached manifest matches the given config hash and
/// current input file hashes.
///
/// A missing, corrupt, or outdated manifest always yields `false`.
pub fn is_cache_valid(
    cache_path: &Path,
    config_hash: &str,
    current_hashes: &BTreeMap<String, String>,
) -> bool {
    let Ok(contents) = std::fs::read(cache_path) else {
        return false;
    };
    let Ok(manifest) = serde_json::from_slice::<CacheManifest>(&contents) else {
        return false;
    };
    manifest.config_hash == config_hash && manifest.file_hashes == *current_hashes
}

/// Persist the cache manifest, writing to a `.tmp` sibling first and then
/// atomically renaming it into place so a crash never leaves a partial file.
pub fn write_cache_atomically(
    cache_path: &Path,
    config_hash: String,
    file_hashes: BTreeMap<String, String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let manifest = CacheManifest {
        config_hash,
        file_hashes,
    };
    let serialized = serde_json::to_string_pretty(&manifest)?;

    if let Some(parent) = cache_path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }

    let tmp_name = format!("{}.tmp", cache_path.display());
    let tmp_path = Path::new(&tmp_name);
    std::fs::write(tmp_path, serialized)?;
    std::fs::rename(tmp_path, cache_path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn hash(data: &[u8]) -> String {
        blake3::hash(data).to_hex().to_string()
    }

    #[test]
    fn file_hash_matches_blake3_of_contents() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("axiom-cache-test-{}", std::process::id()));
        std::fs::write(&path, b"hello world").unwrap();
        assert_eq!(compute_file_hash(&path).unwrap(), hash(b"hello world"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn missing_cache_is_invalid() {
        let missing = PathBuf::from("/nonexistent/.axiom.cache");
        let hashes = BTreeMap::from([("schema.sql".to_string(), hash(b"x"))]);
        assert!(!is_cache_valid(&missing, "cfg", &hashes));
    }

    #[test]
    fn stale_config_or_files_invalidate() {
        let dir = std::env::temp_dir();
        let cache = dir.join(format!("axiom-cache-valid-{}", std::process::id()));
        let hashes = BTreeMap::from([("schema.sql".to_string(), hash(b"abc"))]);
        let config_hash = hash(b"cfg-v1");

        write_cache_atomically(&cache, config_hash.clone(), hashes.clone()).unwrap();
        assert!(is_cache_valid(&cache, &config_hash, &hashes));

        assert!(!is_cache_valid(&cache, &hash(b"cfg-v2"), &hashes));
        let changed = BTreeMap::from([("schema.sql".to_string(), hash(b"def"))]);
        assert!(!is_cache_valid(&cache, &config_hash, &changed));
        let _ = std::fs::remove_file(&cache);
    }

    #[test]
    fn atomic_write_leaves_no_tmp_behind() {
        let dir = std::env::temp_dir();
        let cache = dir.join(format!("axiom-cache-atomic-{}", std::process::id()));
        let tmp = PathBuf::from(format!("{}.tmp", cache.display()));
        write_cache_atomically(
            &cache,
            hash(b"cfg"),
            BTreeMap::from([("a.sql".to_string(), hash(b"a"))]),
        )
        .unwrap();
        assert!(cache.exists());
        assert!(!tmp.exists());
        let _ = std::fs::remove_file(&cache);
    }
}
