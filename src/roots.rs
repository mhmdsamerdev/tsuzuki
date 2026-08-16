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
/// config. `pub(crate)` so `exclude.rs` can resolve a `--root` argument
/// against a tracked root's literal form the same way `add`'s idempotency
/// check does, rather than duplicating this logic.
pub(crate) fn normalize(path: &Path, home: Option<&Path>) -> PathBuf {
    let expanded = expand_tilde(path, home);
    fs::canonicalize(&expanded).unwrap_or(expanded)
}

/// `pub(crate)` so `exclude.rs` can expand a tracked root's literal form
/// (e.g. `~/Anime`) the same way `add_root` does, rather than duplicating
/// tilde-expansion logic.
pub(crate) fn expand_tilde(path: &Path, home: Option<&Path>) -> PathBuf {
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

        // Exact value, not just the variant — `AlreadyTracked` must hold
        // the trailing-slash path exactly as passed to this second call,
        // per the literal-persistence contract (it just isn't written to
        // disk in this branch).
        assert_eq!(
            outcome,
            AddOutcome::AlreadyTracked(PathBuf::from(&with_slash))
        );
        assert_eq!(config.roots.paths.len(), 1);
    }

    #[test]
    fn re_adding_a_tilde_path_with_a_trailing_slash_is_recognized_as_already_tracked() {
        let home = tempdir().unwrap();
        fs::create_dir_all(home.path().join("Anime")).unwrap();

        let mut config = Config::default();
        add_root_with_home(&mut config, Path::new("~/Anime"), Some(home.path())).unwrap();

        let outcome =
            add_root_with_home(&mut config, Path::new("~/Anime/"), Some(home.path())).unwrap();

        assert_eq!(
            outcome,
            AddOutcome::AlreadyTracked(PathBuf::from("~/Anime/"))
        );
        assert_eq!(config.roots.paths.len(), 1);
    }

    #[test]
    fn a_dotdot_path_resolving_to_an_already_tracked_root_is_recognized() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let sibling = root.join("Rewatches");
        fs::create_dir_all(&sibling).unwrap();

        let mut config = Config::default();
        add_root(&mut config, &root).unwrap();

        // `~/Anime/Rewatches/..` resolves (via canonicalize) to the same
        // directory as `~/Anime` — `fs::canonicalize` resolves `..`
        // components, so the in-memory comparison must catch this even
        // though the two strings share no literal prefix relationship.
        let dotdot_path = sibling.join("..");
        let outcome = add_root(&mut config, &dotdot_path).unwrap();

        assert_eq!(outcome, AddOutcome::AlreadyTracked(dotdot_path));
        assert_eq!(config.roots.paths.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_to_an_already_tracked_root_is_recognized() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let real_root = dir.path().join("real-anime");
        fs::create_dir_all(&real_root).unwrap();
        let link_path = dir.path().join("linked-anime");
        symlink(&real_root, &link_path).unwrap();

        let mut config = Config::default();
        add_root(&mut config, &real_root).unwrap();

        // A symlink pointing at the same directory resolves to the same
        // canonical path via `fs::canonicalize` — registering it a second
        // time under the symlinked name must be recognized as the same
        // root, not silently create a duplicate that would later double
        // up series enumeration.
        let outcome = add_root(&mut config, &link_path).unwrap();

        assert_eq!(outcome, AddOutcome::AlreadyTracked(link_path));
        assert_eq!(config.roots.paths.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn already_tracked_matches_through_a_symlinked_home_directory() {
        use std::os::unix::fs::symlink;

        let real_home = tempdir().unwrap();
        fs::create_dir_all(real_home.path().join("Anime")).unwrap();

        let link_parent = tempdir().unwrap();
        let home_link = link_parent.path().join("home-link");
        symlink(real_home.path(), &home_link).unwrap();

        let mut config = Config::default();
        // First add via `~/Anime`, where `~` resolves through a symlinked
        // home directory.
        add_root_with_home(&mut config, Path::new("~/Anime"), Some(&home_link)).unwrap();
        assert_eq!(config.roots.paths, vec!["~/Anime".to_string()]);

        // Second add uses the fully-resolved real path, bypassing the
        // symlinked home entirely — canonicalization must still land on
        // the same real directory for both.
        let real_anime = real_home.path().join("Anime");
        let outcome = add_root_with_home(&mut config, &real_anime, Some(&home_link)).unwrap();

        assert_eq!(outcome, AddOutcome::AlreadyTracked(real_anime));
        assert_eq!(config.roots.paths.len(), 1);
    }

    #[test]
    fn distinctly_cased_paths_are_treated_as_different_roots() {
        // Linux filesystems are case-sensitive by default (v1 targets
        // Linux only, design.md §10) — `Anime` and `anime` are genuinely
        // different directories, and nothing in `normalize`/`expand_tilde`
        // does any case-folding, so they must not be merged.
        let dir = tempdir().unwrap();
        let lower = dir.path().join("anime");
        let upper = dir.path().join("Anime");
        fs::create_dir_all(&lower).unwrap();
        fs::create_dir_all(&upper).unwrap();

        let mut config = Config::default();
        add_root(&mut config, &lower).unwrap();
        let outcome = add_root(&mut config, &upper).unwrap();

        assert_eq!(outcome, AddOutcome::Added(upper));
        assert_eq!(config.roots.paths.len(), 2);
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
