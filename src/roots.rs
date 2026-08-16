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
    /// The root (after normalization) was already tracked. Not an error —
    /// idempotent, exit 0.
    AlreadyTracked(PathBuf),
    /// Newly appended to `config.roots.paths`. Caller must persist `Config`.
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
/// Idempotency is decided by normalized-path comparison (see `normalize`),
/// not by raw string equality — `~/Anime`, `/home/user/Anime`, and
/// `/home/user/Anime/` must all recognize the same tracked root.
pub fn add_root(config: &mut Config, path: &Path) -> Result<AddOutcome, AddError> {
    if !path.exists() {
        return Err(AddError::NotFound(path.to_path_buf()));
    }
    if !path.is_dir() {
        return Err(AddError::NotADirectory(path.to_path_buf()));
    }

    let canonical = fs::canonicalize(path).map_err(AddError::Io)?;

    for existing in &config.roots.paths {
        if normalize(Path::new(existing)) == canonical {
            return Ok(AddOutcome::AlreadyTracked(canonical));
        }
    }

    config
        .roots
        .paths
        .push(canonical.to_string_lossy().into_owned());
    Ok(AddOutcome::Added(canonical))
}

/// Normalizes a stored config path for comparison: expands a leading `~`,
/// then canonicalizes if the path still exists on disk. A stored root that
/// no longer exists (moved/deleted since it was added) falls back to the
/// tilde-expanded form rather than erroring — idempotency checks must not
/// fail just because an unrelated stored entry is currently unreachable.
fn normalize(path: &Path) -> PathBuf {
    let expanded = expand_tilde(path);
    fs::canonicalize(&expanded).unwrap_or(expanded)
}

fn expand_tilde(path: &Path) -> PathBuf {
    if let Ok(stripped) = path.strip_prefix("~")
        && let Some(home) = dirs::home_dir()
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
    fn adds_new_root_and_persists_to_config() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        fs::create_dir_all(&root).unwrap();

        let mut config = Config::default();
        let outcome = add_root(&mut config, &root).unwrap();

        let canonical = fs::canonicalize(&root).unwrap();
        assert_eq!(outcome, AddOutcome::Added(canonical.clone()));
        assert_eq!(config.roots.paths, vec![canonical.to_string_lossy()]);
    }

    #[test]
    fn re_adding_the_same_root_is_idempotent() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        fs::create_dir_all(&root).unwrap();

        let mut config = Config::default();
        add_root(&mut config, &root).unwrap();

        let outcome = add_root(&mut config, &root).unwrap();
        let canonical = fs::canonicalize(&root).unwrap();
        assert_eq!(outcome, AddOutcome::AlreadyTracked(canonical));
        // No duplicate entry.
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
