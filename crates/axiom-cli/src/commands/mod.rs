//! Subcommand implementations. Each keeps the CLI surface thin: business logic
//! lives in the `axiom-check`, `axiom-format`, and `axiom-lint` crates.

pub mod check;
pub mod format;
pub mod lint;
pub mod tty;

use std::path::{Path, PathBuf};

/// The directory containing the config file, used to resolve relative paths.
/// Canonicalized so joined output paths render cleanly (no `././`).
pub fn base_dir(config_path: &Path) -> PathBuf {
    let parent = config_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf())
}
