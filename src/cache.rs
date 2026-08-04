//! BLAKE3-based build caching with zero-copy binary reads.
//!
//! Axiom persists a small manifest (`.axiom.cache`) describing the config file
//! and every resolved input file by their raw 32-byte BLAKE3 digests. The
//! manifest is archived with `rkyv` and memory-mapped back with `memmap2`, so
//! cache-hit validation compares digests straight out of the page cache with no
//! JSON parsing or text decoding.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use memmap2::{Mmap, MmapOptions};
use rkyv::{Archive, Deserialize, Serialize};

/// Persisted cache manifest, archived in zero-copy form.
#[derive(Archive, Deserialize, Serialize, Debug, PartialEq)]
pub struct CacheManifest {
    /// BLAKE3 digest of the `axiom.json` configuration file.
    pub config_hash: [u8; 32],
    /// BLAKE3 digests of every resolved input file, keyed by path.
    pub file_hashes: BTreeMap<String, [u8; 32]>,
}

impl CacheManifest {
    /// Memory-map the cache file and deserialize the archived manifest.
    ///
    /// Returns `None` when the file is missing, unreadable, or does not
    /// validate as a well-formed rkyv archive.
    pub fn load(cache_path: &Path) -> Option<CacheManifest> {
        let mmap = mmap_cache_file(cache_path)?;
        let archived =
            rkyv::access::<ArchivedCacheManifest, rkyv::rancor::Error>(&mmap).ok()?;
        rkyv::deserialize::<CacheManifest, rkyv::rancor::Error>(archived).ok()
    }

    /// Serialize to raw rkyv bytes and write atomically via a `.tmp` sibling,
    /// renaming into place so a crash never leaves a partial cache file.
    pub fn save(&self, cache_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let serialized = rkyv::to_bytes::<rkyv::rancor::Error>(self)?;

        if let Some(parent) = cache_path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }

        let tmp_name = format!("{}.tmp", cache_path.display());
        let tmp_path = Path::new(&tmp_name);
        std::fs::write(tmp_path, serialized.as_ref())?;
        std::fs::rename(tmp_path, cache_path)?;
        Ok(())
    }

    /// True when this manifest matches the given config digest and the current
    /// input file digests.
    pub fn matches_current(
        &self,
        config_hash: &[u8; 32],
        current_hashes: &BTreeMap<String, [u8; 32]>,
    ) -> bool {
        self.config_hash == *config_hash && self.file_hashes == *current_hashes
    }
}

/// Compute the raw BLAKE3 digest of a file's contents.
pub fn compute_file_hash(path: &Path) -> Result<[u8; 32], io::Error> {
    let mut hasher = blake3::Hasher::new();
    let mut file = std::fs::File::open(path)?;
    std::io::copy(&mut file, &mut hasher)?;
    Ok(hasher.finalize().into())
}

/// Memory-map a cache file, returning `None` if it cannot be opened.
fn mmap_cache_file(cache_path: &Path) -> Option<Mmap> {
    let file = std::fs::File::open(cache_path).ok()?;
    // SAFETY: the mapping is read-only; the file is not concurrently mutated
    // by this process while the mapping is used, and the mapped range is
    // bounded by the file size captured at map time.
    unsafe { MmapOptions::new().map(&file) }.ok()
}

/// Return `true` when the cached manifest matches the given config digest and
/// current input file digests.
///
/// The archived manifest is compared in place, directly from the memory map,
/// without allocating or decoding any strings. A missing, corrupt, or outdated
/// manifest always yields `false`.
pub fn is_cache_valid(
    cache_path: &Path,
    config_hash: &[u8; 32],
    current_hashes: &BTreeMap<String, [u8; 32]>,
) -> bool {
    let Some(mmap) = mmap_cache_file(cache_path) else {
        return false;
    };
    let Ok(archived) =
        rkyv::access::<ArchivedCacheManifest, rkyv::rancor::Error>(&mmap)
    else {
        return false;
    };

    if archived.config_hash != *config_hash {
        return false;
    }
    if archived.file_hashes.len() != current_hashes.len() {
        return false;
    }
    for (key, value) in archived.file_hashes.iter() {
        let Some(expected) = current_hashes.get(key.as_str()) else {
            return false;
        };
        if expected != value {
            return false;
        }
    }
    true
}

/// Persist the cache manifest atomically.
pub fn write_cache_atomically(
    cache_path: &Path,
    config_hash: [u8; 32],
    file_hashes: BTreeMap<String, [u8; 32]>,
) -> Result<(), Box<dyn std::error::Error>> {
    let manifest = CacheManifest {
        config_hash,
        file_hashes,
    };
    manifest.save(cache_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn hash(data: &[u8]) -> [u8; 32] {
        blake3::hash(data).into()
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
        assert!(!is_cache_valid(&missing, &hash(b"cfg"), &hashes));
    }

    #[test]
    fn stale_config_or_files_invalidate() {
        let dir = std::env::temp_dir();
        let cache = dir.join(format!("axiom-cache-valid-{}", std::process::id()));
        let hashes = BTreeMap::from([("schema.sql".to_string(), hash(b"abc"))]);
        let config_hash = hash(b"cfg-v1");

        write_cache_atomically(&cache, config_hash, hashes.clone()).unwrap();
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

    #[test]
    fn load_round_trips_binary_manifest() {
        let dir = std::env::temp_dir();
        let cache = dir.join(format!("axiom-cache-load-{}", std::process::id()));
        let hashes = BTreeMap::from([("schema.sql".to_string(), hash(b"abc"))]);
        write_cache_atomically(&cache, hash(b"cfg"), hashes.clone()).unwrap();

        let loaded = CacheManifest::load(&cache).expect("load manifest");
        assert_eq!(loaded.config_hash, hash(b"cfg"));
        assert_eq!(loaded.file_hashes, hashes);

        let bytes = std::fs::read(&cache).unwrap();
        assert!(!bytes.starts_with(b"{"), "manifest must be binary, not JSON");
        assert!(
            String::from_utf8(bytes).is_err(),
            "manifest should not decode as UTF-8 text"
        );

        let _ = std::fs::remove_file(&cache);
    }

    #[test]
    fn corrupted_cache_is_rejected() {
        let dir = std::env::temp_dir();
        let cache = dir.join(format!("axiom-cache-corrupt-{}", std::process::id()));
        std::fs::write(&cache, b"this is not an rkyv archive").unwrap();
        assert!(!is_cache_valid(&cache, &hash(b"cfg"), &BTreeMap::new()));
        let _ = std::fs::remove_file(&cache);
    }
}
