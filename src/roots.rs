//! `tsuzuki add`: validates a root and appends it to the in-memory `Config`.
//! The caller persists the config.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::config::Config;

#[derive(Debug, PartialEq, Eq)]
pub enum AddOutcome {
    /// The root was already tracked. Holds the path as passed in.
    AlreadyTracked(PathBuf),
    /// Appended to `config.roots.paths` as typed.
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

/// Appends `path` to `config.roots.paths` unless it resolves to an already
/// tracked root. Expansion and canonicalization are used only to validate and
/// compare; the stored and returned path is always as typed.
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

/// Expands a leading `~`, then canonicalizes if the path exists. A missing
/// path falls back to the expanded form. For comparison only, never stored.
pub(crate) fn normalize(path: &Path, home: Option<&Path>) -> PathBuf {
    let expanded = expand_tilde(path, home);
    fs::canonicalize(&expanded).unwrap_or(expanded)
}

/// Canonical paths of every tracked root, for telling roots from series.
pub(crate) fn tracked_real_paths(config: &Config, home: Option<&Path>) -> Vec<PathBuf> {
    config
        .roots
        .paths
        .iter()
        .map(|root| normalize(Path::new(root), home))
        .collect()
}

/// A stored root's folder name: its last path component, so a trailing
/// slash doesn't matter. `None` for roots with no final component (`/`).
pub(crate) fn root_folder_name(root: &str) -> Option<String> {
    Path::new(root)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
}

/// Replaces a leading `~` with `home`; unchanged if there is no home.
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

        // Stored as passed, not canonicalized.
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

        // The absolute form of `~/Anime` is the same root.
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

        // Holds the trailing-slash path as passed.
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

        // `Rewatches/..` canonicalizes to the tracked root.
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

        // A symlink to a tracked root is not a second root.
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
        // `~` resolves through a symlinked home.
        add_root_with_home(&mut config, Path::new("~/Anime"), Some(&home_link)).unwrap();
        assert_eq!(config.roots.paths, vec!["~/Anime".to_string()]);

        // The real path bypasses the symlink but is the same directory.
        let real_anime = real_home.path().join("Anime");
        let outcome = add_root_with_home(&mut config, &real_anime, Some(&home_link)).unwrap();

        assert_eq!(outcome, AddOutcome::AlreadyTracked(real_anime));
        assert_eq!(config.roots.paths.len(), 1);
    }

    #[test]
    fn distinctly_cased_paths_are_treated_as_different_roots() {
        // Paths are case-sensitive; no case-folding.
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

    #[test]
    fn root_folder_name_is_the_last_component_ignoring_a_trailing_slash() {
        assert_eq!(
            [
                root_folder_name("~/Anime"),
                root_folder_name("/mnt/nas/Shows/"),
                root_folder_name("re"),
                root_folder_name("/"),
            ],
            [
                Some("Anime".to_string()),
                Some("Shows".to_string()),
                Some("re".to_string()),
                None,
            ]
        );
    }
}
