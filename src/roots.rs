//! `tsuzuki add` — validating and persisting tracked roots (CLAUDE.md's
//! "`tsuzuki add` validation and idempotency").
//!
//! This module only mutates the in-memory `Config`; the caller is
//! responsible for writing it to disk (main.rs) so tests here can exercise
//! the logic without any config-file I/O.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::config::Config;

#[derive(Debug, PartialEq, Eq)]
pub enum AddOutcome {
    /// The root (after in-memory normalization for comparison) was already
    /// tracked. Holds the path exactly as passed to `add_root`. Not an
    /// error — idempotent, exit 0.
    AlreadyTracked(PathBuf),
    /// Newly appended to `config.roots.paths`, in the exact form passed to
    /// `add_root` — no expansion, no canonicalization (CLAUDE.md's "Roots
    /// are persisted in the form the user typed"). Caller must persist
    /// `Config`.
    Added(PathBuf),
}

#[derive(Debug)]
pub enum AddError {
    NotFound(PathBuf),
    NotADirectory(PathBuf),
    Io(io::Error),
}

impl fmt::Display for AddError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AddError::NotFound(p) => write!(f, "path does not exist: {}", p.display()),
            AddError::NotADirectory(p) => write!(f, "not a directory: {}", p.display()),
            AddError::Io(e) => write!(f, "{e}"),
        }
    }
}

/// Validates `path`, then either records it as already-tracked or appends
/// it to `config.roots.paths` — never both, and never a duplicate entry.
///
/// `~` expansion and canonicalization happen only in memory, for
/// existence/dir-ness validation and for the already-tracked comparison
/// against existing entries (which may themselves be in `~`-form or
/// absolute form — comparison works correctly regardless of which form
/// either side is in). The value written to `config.roots.paths` — and
/// returned in `AddOutcome` — is always `path` exactly as given, never the
/// expanded/canonicalized form (CLAUDE.md's "Roots are persisted in the
/// form the user typed").
pub fn add_root(config: &mut Config, path: &Path) -> Result<AddOutcome, AddError> {
    add_root_with_home(config, path, dirs::home_dir().as_deref())
}

fn add_root_with_home(
    config: &mut Config,
    path: &Path,
    home: Option<&Path>,
) -> Result<AddOutcome, AddError> {
    let resolved = expand_tilde(path, home);

    if !resolved.exists() {
        return Err(AddError::NotFound(path.to_path_buf()));
    }
    if !resolved.is_dir() {
        return Err(AddError::NotADirectory(path.to_path_buf()));
    }

    let canonical = fs::canonicalize(&resolved).map_err(AddError::Io)?;

    for existing in &config.roots.paths {
        if normalize(Path::new(existing), home) == canonical {
            return Ok(AddOutcome::AlreadyTracked(path.to_path_buf()));
        }
    }

    config.roots.paths.push(path.to_string_lossy().into_owned());
    Ok(AddOutcome::Added(path.to_path_buf()))
}

/// Normalizes a stored config path for comparison: expands a leading `~`,
/// then canonicalizes if the path still exists on disk. A stored root that
/// no longer exists (moved/deleted since it was added) falls back to the
/// tilde-expanded form rather than erroring — idempotency checks must not
/// fail just because an unrelated stored entry is currently unreachable.
/// This is a comparison-only helper — its output is never written back to
/// config.
fn normalize(path: &Path, home: Option<&Path>) -> PathBuf {
    let expanded = expand_tilde(path, home);
    fs::canonicalize(&expanded).unwrap_or(expanded)
}

fn expand_tilde(path: &Path, home: Option<&Path>) -> PathBuf {
    if let Ok(stripped) = path.strip_prefix("~")
        && let Some(home) = home
    {
        return home.join(stripped);
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn rejects_nonexistent_path() {
        let mut config = Config::default();
        let missing = PathBuf::from("/definitely/does/not/exist/tsuzuki-test");

        let err = add_root(&mut config, &missing).unwrap_err();
        assert!(matches!(err, AddError::NotFound(p) if p == missing));
        assert!(config.roots.paths.is_empty());
    }

    #[test]
    fn rejects_path_that_is_a_file_not_a_directory() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("not-a-dir.txt");
        fs::write(&file_path, b"hello").unwrap();

        let mut config = Config::default();
        let err = add_root(&mut config, &file_path).unwrap_err();
        assert!(matches!(err, AddError::NotADirectory(p) if p == file_path));
        assert!(config.roots.paths.is_empty());
    }

    #[test]
    fn adds_new_root_and_persists_to_config_exactly_as_given() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        fs::create_dir_all(&root).unwrap();

        let mut config = Config::default();
        let outcome = add_root(&mut config, &root).unwrap();

        // No canonicalization applied to the stored/returned value — it's
        // exactly the path passed in, not `fs::canonicalize(&root)`.
        assert_eq!(outcome, AddOutcome::Added(root.clone()));
        assert_eq!(config.roots.paths, vec![root.to_string_lossy()]);
    }

    #[test]
    fn re_adding_the_same_root_is_idempotent() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        fs::create_dir_all(&root).unwrap();

        let mut config = Config::default();
        add_root(&mut config, &root).unwrap();

        let outcome = add_root(&mut config, &root).unwrap();
        assert_eq!(outcome, AddOutcome::AlreadyTracked(root));
        // No duplicate entry.
        assert_eq!(config.roots.paths.len(), 1);
    }

    #[test]
    fn add_persists_a_tilde_path_literally_not_expanded() {
        let home = tempdir().unwrap();
        fs::create_dir_all(home.path().join("Anime")).unwrap();

        let mut config = Config::default();
        let outcome =
            add_root_with_home(&mut config, Path::new("~/Anime"), Some(home.path())).unwrap();

        assert_eq!(outcome, AddOutcome::Added(PathBuf::from("~/Anime")));
        assert_eq!(config.roots.paths, vec!["~/Anime".to_string()]);
    }

    #[test]
    fn already_tracked_matches_across_tilde_and_absolute_forms() {
        let home = tempdir().unwrap();
        let root = home.path().join("Anime");
        fs::create_dir_all(&root).unwrap();

        let mut config = Config::default();
        add_root_with_home(&mut config, Path::new("~/Anime"), Some(home.path())).unwrap();
        assert_eq!(config.roots.paths, vec!["~/Anime".to_string()]);

        // Second add uses the equivalent absolute path instead of `~/Anime`
        // — must still recognize it as the same tracked root.
        let outcome = add_root_with_home(&mut config, &root, Some(home.path())).unwrap();
        assert_eq!(outcome, AddOutcome::AlreadyTracked(root));
        assert_eq!(config.roots.paths.len(), 1);
    }

    #[test]
    fn re_adding_with_a_trailing_slash_is_recognized_as_already_tracked() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        fs::create_dir_all(&root).unwrap();

        let mut config = Config::default();
        add_root(&mut config, &root).unwrap();

        let mut with_slash = root.as_os_str().to_owned();
        with_slash.push("/");
        let outcome = add_root(&mut config, Path::new(&with_slash)).unwrap();

        assert!(matches!(outcome, AddOutcome::AlreadyTracked(_)));
        assert_eq!(config.roots.paths.len(), 1);
    }

    #[test]
    fn distinct_roots_both_get_added() {
        let dir = tempdir().unwrap();
        let anime = dir.path().join("Anime");
        let rewatches = anime.join("Rewatches");
        fs::create_dir_all(&rewatches).unwrap();

        let mut config = Config::default();
        add_root(&mut config, &anime).unwrap();
        let outcome = add_root(&mut config, &rewatches).unwrap();

        assert!(matches!(outcome, AddOutcome::Added(_)));
        assert_eq!(config.roots.paths.len(), 2);
    }
}
