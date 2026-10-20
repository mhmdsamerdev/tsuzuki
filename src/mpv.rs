//! Which mpv tsuzuki uses: the only place that detects mpv installs.
//!
//! [`resolve`] turns `[mpv]` into one target: the launch command and the
//! `tsuzuki.lua` that mpv loads. Launch, the script check, `doctor` and
//! `doctor --fix` all use it. Reading the script log deliberately doesn't:
//! `script_log::mpv_log_dirs` merges every known log dir regardless of target.
//!
//! Snap isn't supported: its config and data paths depend on snapd and the
//! snap's own wrapper, so they can't be known without guessing. Snap users
//! use `install = "custom"`.

use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::config::{Install, Mpv};
use crate::roots::expand_tilde;

const FLATPAK_APP_ID: &str = "io.mpv.Mpv";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Native,
    Flatpak,
    Custom,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Native => "native",
            Kind::Flatpak => "Flatpak",
            Kind::Custom => "custom",
        }
    }
}

/// How the target was picked: by `install = "auto"` or a pinned value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChosenBy {
    Auto,
    Pinned,
}

/// Which installs exist, from [`detect`]. Injected so [`resolve`] is testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Installed {
    pub native: bool,
    pub flatpak: bool,
}

/// The one mpv tsuzuki launches, checks and installs the script for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub kind: Kind,
    pub chosen_by: ChosenBy,
    pub program: String,
    /// Arguments before the file arguments (e.g. `["run", "io.mpv.Mpv"]`).
    pub base_args: Vec<String>,
    /// The `tsuzuki.lua` this mpv loads. `None` only when it can't be known:
    /// `custom` without `script_path`, or no home/config directory.
    pub script_path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// `auto` found neither native nor Flatpak mpv.
    NotFound,
    /// A pinned `native` or `flatpak` isn't installed.
    NotInstalled(Kind),
    /// `custom` without a `command`.
    NoCustomCommand,
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResolveError::NotFound => write!(
                f,
                "could not find mpv — install it, or set [mpv] install = \"custom\" and command in config.toml"
            ),
            ResolveError::NotInstalled(kind) => write!(
                f,
                "[mpv] install is \"{}\" but {} mpv is not installed — install it, or change [mpv] install in config.toml",
                kind.label().to_lowercase(),
                kind.label()
            ),
            ResolveError::NoCustomCommand => write!(
                f,
                "[mpv] install is \"custom\" but command is empty — set [mpv] command in config.toml"
            ),
        }
    }
}

/// Resolves `[mpv]` to the target. `auto` tries native, then Flatpak; a pinned
/// kind that isn't installed is an error, never a fallback.
pub fn resolve(
    mpv: &Mpv,
    installed: Installed,
    config_dir: Option<&Path>,
    home: Option<&Path>,
) -> Result<Target, ResolveError> {
    match mpv.install {
        Install::Auto if installed.native => Ok(native(ChosenBy::Auto, config_dir)),
        Install::Auto if installed.flatpak => Ok(flatpak(ChosenBy::Auto, home)),
        Install::Auto => Err(ResolveError::NotFound),
        Install::Native if installed.native => Ok(native(ChosenBy::Pinned, config_dir)),
        Install::Native => Err(ResolveError::NotInstalled(Kind::Native)),
        Install::Flatpak if installed.flatpak => Ok(flatpak(ChosenBy::Pinned, home)),
        Install::Flatpak => Err(ResolveError::NotInstalled(Kind::Flatpak)),
        Install::Custom => custom(mpv, home),
    }
}

/// [`resolve`] against the real `PATH`, Flatpak dirs and XDG config dir.
pub fn resolve_here(mpv: &Mpv, home: Option<&Path>) -> Result<Target, ResolveError> {
    resolve(mpv, detect(home), dirs::config_dir().as_deref(), home)
}

fn native(chosen_by: ChosenBy, config_dir: Option<&Path>) -> Target {
    Target {
        kind: Kind::Native,
        chosen_by,
        program: "mpv".to_string(),
        base_args: Vec::new(),
        script_path: config_dir.map(|d| d.join("mpv/scripts/tsuzuki.lua")),
    }
}

fn flatpak(chosen_by: ChosenBy, home: Option<&Path>) -> Target {
    Target {
        kind: Kind::Flatpak,
        chosen_by,
        program: "flatpak".to_string(),
        base_args: vec!["run".to_string(), FLATPAK_APP_ID.to_string()],
        script_path: home.map(|h| h.join(".var/app/io.mpv.Mpv/config/mpv/scripts/tsuzuki.lua")),
    }
}

fn custom(mpv: &Mpv, home: Option<&Path>) -> Result<Target, ResolveError> {
    let mut parts = mpv.command.split_whitespace();
    let program = parts.next().ok_or(ResolveError::NoCustomCommand)?;
    let script_path =
        (!mpv.script_path.is_empty()).then(|| expand_tilde(Path::new(&mpv.script_path), home));
    Ok(Target {
        kind: Kind::Custom,
        chosen_by: ChosenBy::Pinned,
        program: program.to_string(),
        base_args: parts.map(str::to_string).collect(),
        script_path,
    })
}

/// Which installs exist on this machine. Cheap: a `PATH` scan and two
/// directory checks, no spawn.
pub fn detect(home: Option<&Path>) -> Installed {
    Installed {
        native: std::env::var_os("PATH").is_some_and(|p| native_mpv_on(&p)),
        flatpak: flatpak_mpv_installed(home),
    }
}

/// True if an executable `mpv` on `path_var` isn't Snap's. `/snap/bin/mpv` is a
/// symlink to the `snap` launcher, and Snap mpv's paths aren't known.
fn native_mpv_on(path_var: &OsStr) -> bool {
    std::env::split_paths(path_var)
        .filter(|dir| !is_snap(dir))
        .map(|dir| dir.join("mpv"))
        .any(|candidate| is_executable_file(&candidate) && !resolves_to_snap(&candidate))
}

fn is_snap(path: &Path) -> bool {
    path.starts_with("/snap")
}

/// True when `candidate` canonicalizes under `/snap` or to the `snap` launcher.
fn resolves_to_snap(candidate: &Path) -> bool {
    std::fs::canonicalize(candidate)
        .is_ok_and(|real| is_snap(&real) || real.file_name() == Some(OsStr::new("snap")))
}

/// True if an executable named `name` exists on `$PATH`.
pub fn program_on_path(name: &str) -> bool {
    let Some(path_var) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path_var).any(|dir| is_executable_file(&dir.join(name)))
}

/// True for a regular file with an execute bit set, so a non-executable file
/// that shares the name is rejected. One `stat`, no spawn.
#[cfg(unix)]
pub fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
pub fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

/// True if `io.mpv.Mpv` is installed system-wide or under `home`, checked by
/// directory existence.
fn flatpak_mpv_installed(home: Option<&Path>) -> bool {
    flatpak_mpv_installed_in(Path::new("/var/lib/flatpak"), home)
}

/// [`flatpak_mpv_installed`] with the system install root injected for tests.
fn flatpak_mpv_installed_in(system_root: &Path, home: Option<&Path>) -> bool {
    system_root.join("app/io.mpv.Mpv").is_dir()
        || home.is_some_and(|h| h.join(".local/share/flatpak/app/io.mpv.Mpv").is_dir())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const NONE: Installed = Installed {
        native: false,
        flatpak: false,
    };
    const NATIVE: Installed = Installed {
        native: true,
        flatpak: false,
    };
    const FLATPAK: Installed = Installed {
        native: false,
        flatpak: true,
    };
    const BOTH: Installed = Installed {
        native: true,
        flatpak: true,
    };

    fn mpv(install: Install) -> Mpv {
        Mpv {
            install,
            ..Mpv::default()
        }
    }

    fn native_target(chosen_by: ChosenBy) -> Target {
        Target {
            kind: Kind::Native,
            chosen_by,
            program: "mpv".to_string(),
            base_args: Vec::new(),
            script_path: Some(PathBuf::from("/cfg/mpv/scripts/tsuzuki.lua")),
        }
    }

    fn flatpak_target(chosen_by: ChosenBy) -> Target {
        Target {
            kind: Kind::Flatpak,
            chosen_by,
            program: "flatpak".to_string(),
            base_args: vec!["run".to_string(), "io.mpv.Mpv".to_string()],
            script_path: Some(PathBuf::from(
                "/home/u/.var/app/io.mpv.Mpv/config/mpv/scripts/tsuzuki.lua",
            )),
        }
    }

    fn resolve_with(mpv: &Mpv, installed: Installed) -> Result<Target, ResolveError> {
        resolve(
            mpv,
            installed,
            Some(Path::new("/cfg")),
            Some(Path::new("/home/u")),
        )
    }

    #[test]
    fn resolve_auto_with_nothing_installed_is_not_found() {
        assert_eq!(
            resolve_with(&mpv(Install::Auto), NONE),
            Err(ResolveError::NotFound)
        );
    }

    #[test]
    fn resolve_auto_with_only_native_picks_native() {
        assert_eq!(
            resolve_with(&mpv(Install::Auto), NATIVE),
            Ok(native_target(ChosenBy::Auto))
        );
    }

    #[test]
    fn resolve_auto_with_only_flatpak_picks_flatpak() {
        assert_eq!(
            resolve_with(&mpv(Install::Auto), FLATPAK),
            Ok(flatpak_target(ChosenBy::Auto))
        );
    }

    #[test]
    fn resolve_auto_with_both_installed_picks_native() {
        assert_eq!(
            resolve_with(&mpv(Install::Auto), BOTH),
            Ok(native_target(ChosenBy::Auto))
        );
    }

    #[test]
    fn resolve_pinned_native_picks_native_whatever_else_is_installed() {
        for installed in [NATIVE, BOTH] {
            assert_eq!(
                resolve_with(&mpv(Install::Native), installed),
                Ok(native_target(ChosenBy::Pinned))
            );
        }
    }

    #[test]
    fn resolve_pinned_native_not_installed_errors_and_never_falls_back_to_flatpak() {
        for installed in [NONE, FLATPAK] {
            assert_eq!(
                resolve_with(&mpv(Install::Native), installed),
                Err(ResolveError::NotInstalled(Kind::Native))
            );
        }
    }

    #[test]
    fn resolve_pinned_flatpak_picks_flatpak_even_when_native_is_installed() {
        for installed in [FLATPAK, BOTH] {
            assert_eq!(
                resolve_with(&mpv(Install::Flatpak), installed),
                Ok(flatpak_target(ChosenBy::Pinned))
            );
        }
    }

    #[test]
    fn resolve_pinned_flatpak_not_installed_errors_and_never_falls_back_to_native() {
        for installed in [NONE, NATIVE] {
            assert_eq!(
                resolve_with(&mpv(Install::Flatpak), installed),
                Err(ResolveError::NotInstalled(Kind::Flatpak))
            );
        }
    }

    #[test]
    fn resolve_custom_splits_the_command_and_expands_the_script_path() {
        let config = Mpv {
            install: Install::Custom,
            command: "/opt/mpv/mpv  --profile=anime".to_string(),
            script_path: "~/mpv/scripts/tsuzuki.lua".to_string(),
            command_override: String::new(),
        };
        for installed in [NONE, NATIVE, FLATPAK, BOTH] {
            assert_eq!(
                resolve_with(&config, installed),
                Ok(Target {
                    kind: Kind::Custom,
                    chosen_by: ChosenBy::Pinned,
                    program: "/opt/mpv/mpv".to_string(),
                    base_args: vec!["--profile=anime".to_string()],
                    script_path: Some(PathBuf::from("/home/u/mpv/scripts/tsuzuki.lua")),
                })
            );
        }
    }

    #[test]
    fn resolve_custom_without_script_path_has_no_script_to_check() {
        let config = Mpv {
            install: Install::Custom,
            command: "mpv".to_string(),
            ..Mpv::default()
        };
        assert_eq!(
            resolve_with(&config, BOTH),
            Ok(Target {
                kind: Kind::Custom,
                chosen_by: ChosenBy::Pinned,
                program: "mpv".to_string(),
                base_args: Vec::new(),
                script_path: None,
            })
        );
    }

    #[test]
    fn resolve_custom_with_a_blank_command_errors() {
        for command in ["", "   "] {
            let config = Mpv {
                install: Install::Custom,
                command: command.to_string(),
                ..Mpv::default()
            };
            assert_eq!(
                resolve_with(&config, BOTH),
                Err(ResolveError::NoCustomCommand)
            );
        }
    }

    #[test]
    fn resolve_without_home_or_config_dir_leaves_the_script_path_unknown() {
        let native = resolve(&mpv(Install::Native), NATIVE, None, None).unwrap();
        assert_eq!(native.script_path, None);
        let flatpak = resolve(&mpv(Install::Flatpak), FLATPAK, None, None).unwrap();
        assert_eq!(flatpak.script_path, None);
    }

    #[test]
    fn resolve_error_messages_name_the_next_step() {
        assert_eq!(
            ResolveError::NotInstalled(Kind::Flatpak).to_string(),
            "[mpv] install is \"flatpak\" but Flatpak mpv is not installed — install it, or change [mpv] install in config.toml"
        );
        assert_eq!(
            ResolveError::NoCustomCommand.to_string(),
            "[mpv] install is \"custom\" but command is empty — set [mpv] command in config.toml"
        );
    }

    #[cfg(unix)]
    fn executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        fs::write(path, b"#!/bin/sh\n").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn native_detection_finds_an_executable_mpv_on_path() {
        let dir = tempfile::tempdir().unwrap();
        executable(&dir.path().join("mpv"));
        assert!(native_mpv_on(dir.path().as_os_str()));
    }

    #[cfg(unix)]
    #[test]
    fn native_detection_ignores_a_non_executable_mpv() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let mpv = dir.path().join("mpv");
        fs::write(&mpv, b"not a binary").unwrap();
        fs::set_permissions(&mpv, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!native_mpv_on(dir.path().as_os_str()));
    }

    #[cfg(unix)]
    #[test]
    fn native_detection_ignores_an_mpv_that_links_to_the_snap_launcher() {
        let dir = tempfile::tempdir().unwrap();
        let launcher_dir = dir.path().join("usr-bin");
        fs::create_dir(&launcher_dir).unwrap();
        executable(&launcher_dir.join("snap"));
        let bin = dir.path().join("bin");
        fs::create_dir(&bin).unwrap();
        std::os::unix::fs::symlink(launcher_dir.join("snap"), bin.join("mpv")).unwrap();

        assert!(!native_mpv_on(bin.as_os_str()));
    }

    #[cfg(unix)]
    #[test]
    fn native_detection_skips_snap_bin_but_still_finds_a_later_native_mpv() {
        let dir = tempfile::tempdir().unwrap();
        executable(&dir.path().join("mpv"));
        let path_var = std::env::join_paths([Path::new("/snap/bin"), dir.path()]).unwrap();
        assert!(native_mpv_on(&path_var));
        assert!(!native_mpv_on(OsStr::new("/snap/bin")));
    }

    #[test]
    fn flatpak_detection_needs_a_system_or_user_install_dir() {
        let dir = tempfile::tempdir().unwrap();
        let system = dir.path().join("system");
        let home = dir.path().join("home");

        assert!(!flatpak_mpv_installed_in(&system, Some(&home)));
        assert!(!flatpak_mpv_installed_in(&system, None));

        fs::create_dir_all(home.join(".local/share/flatpak/app/io.mpv.Mpv")).unwrap();
        assert!(flatpak_mpv_installed_in(&system, Some(&home)));
        assert!(!flatpak_mpv_installed_in(&system, None));

        fs::create_dir_all(system.join("app/io.mpv.Mpv")).unwrap();
        assert!(flatpak_mpv_installed_in(&system, None));
    }

    #[test]
    fn snap_paths_are_recognized_by_prefix_component() {
        assert!(is_snap(Path::new("/snap/bin")));
        assert!(is_snap(Path::new("/snap/mpv/123/usr/bin/mpv")));
        assert!(!is_snap(Path::new("/snapshots/bin")));
        assert!(!is_snap(Path::new("/usr/bin")));
    }
}
