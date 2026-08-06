//! Path helpers shared across the CLI and check crates.

use std::path::{Component, Path, PathBuf};

/// Join a base directory with a relative path, dropping leading `.` components
/// so the result renders as `base/gen/api.ts` instead of `base/./gen/api.ts`.
pub fn resolve_path(base: &Path, rel: &Path) -> PathBuf {
    if rel.is_absolute() {
        return rel.to_path_buf();
    }
    let mut out = base.to_path_buf();
    for component in rel.components() {
        match component {
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_leading_curdir() {
        let base = Path::new("/project");
        assert_eq!(
            resolve_path(base, Path::new("./gen/api.ts")),
            Path::new("/project/gen/api.ts")
        );
    }

    #[test]
    fn preserves_absolute() {
        let base = Path::new("/project");
        assert_eq!(
            resolve_path(base, Path::new("/abs/api.ts")),
            Path::new("/abs/api.ts")
        );
    }

    #[test]
    fn plain_join() {
        let base = Path::new("/project");
        assert_eq!(resolve_path(base, Path::new("gen/api.ts")), Path::new("/project/gen/api.ts"));
    }
}
