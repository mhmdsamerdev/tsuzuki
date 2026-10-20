//! Real-binary tests for the mpv target: which mpv is used, the script block on
//! `resume`/`start`, doctor's target-only checks, and log merging. `PATH` holds
//! only a fake `mpv`, so nothing depends on the host's mpv installs.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

const BUNDLED_SCRIPT: &str = include_str!("../contrib/mpv/tsuzuki.lua");

struct Env {
    _dir: tempfile::TempDir,
    home: PathBuf,
    bin: PathBuf,
    /// Created by the fake mpv after it writes its arguments to `launched.args`;
    /// only shell builtins are used, since `PATH` holds nothing else.
    launched: PathBuf,
}

impl Env {
    /// A temp `HOME` with `~/Anime/Monster/ep01.mkv` tracked, a fake `mpv` in
    /// `bin`, and `[mpv]` set to `mpv_table`.
    fn new(mpv_table: &str) -> Env {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let bin = dir.path().join("bin");
        let launched = dir.path().join("launched");
        fs::create_dir_all(home.join("Anime/Monster")).unwrap();
        fs::File::create(home.join("Anime/Monster/ep01.mkv")).unwrap();
        fs::create_dir_all(&bin).unwrap();
        write_executable(
            &bin.join("mpv"),
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > {0}.args\n: > {0}\n",
                launched.display()
            ),
        );
        let env = Env {
            _dir: dir,
            home,
            bin,
            launched,
        };
        env.write_config(mpv_table);
        env
    }

    fn write_config(&self, mpv_table: &str) {
        let path = self.home.join(".config/tsuzuki/config.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            path,
            format!(
                "[roots]\npaths = [\"{}\"]\n\n[mpv]\n{mpv_table}",
                self.home.join("Anime").display()
            ),
        )
        .unwrap();
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_with_env(args, &[])
    }

    fn run_with_env(&self, args: &[&str], vars: &[(&str, &Path)]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_tsuzuki"))
            .args(args)
            .env("HOME", &self.home)
            .env("PATH", &self.bin)
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("XDG_DATA_HOME")
            .env_remove("MPV_HOME")
            .envs(vars.iter().copied())
            .output()
            .unwrap()
    }

    fn native_script(&self) -> PathBuf {
        self.home.join(".config/mpv/scripts/tsuzuki.lua")
    }

    fn flatpak_script(&self) -> PathBuf {
        self.home
            .join(".var/app/io.mpv.Mpv/config/mpv/scripts/tsuzuki.lua")
    }

    /// Makes Flatpak mpv count as installed: the per-user install dir plus a
    /// `flatpak` on `PATH`.
    fn install_flatpak(&self) {
        self.install_flatpak_dir_only();
        write_executable(&self.bin.join("flatpak"), "#!/bin/sh\n");
    }

    /// The per-user install dir alone, with no `flatpak` CLI on `PATH`.
    fn install_flatpak_dir_only(&self) {
        fs::create_dir_all(self.home.join(".local/share/flatpak/app/io.mpv.Mpv")).unwrap();
    }

    /// Replaces the fake `mpv` with `body`, run after it records its argv.
    fn set_mpv_body(&self, body: &str) {
        write_executable(
            &self.bin.join("mpv"),
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > {0}.args\n{body}\n: > {0}\n",
                self.launched.display()
            ),
        );
    }

    fn launch_log(&self) -> PathBuf {
        self.home.join(".local/share/tsuzuki/mpv-launch.log")
    }

    /// Polls with a deadline for the fake mpv child, then returns its argv.
    fn wait_for_launch(&self) -> Vec<String> {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.launched.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(self.launched.exists(), "the fake mpv was never launched");
        fs::read_to_string(self.launched.with_extension("args"))
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn episode(&self) -> PathBuf {
        self.home.join("Anime/Monster/ep01.mkv")
    }
}

fn write_executable(path: &Path, contents: &str) {
    fs::write(path, contents).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn write_file(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

// --- resume/start enforcement ---

#[test]
fn start_and_resume_with_the_target_script_missing_exit_nonzero_and_never_launch() {
    let env = Env::new("install = \"native\"\n");
    let expected = format!(
        "error: mpv script not installed for native mpv at {} — run: tsuzuki doctor --fix\n",
        env.native_script().display()
    );

    for args in [&["start", "Monster"][..], &["resume"][..], &[][..]] {
        let out = env.run(args);
        assert!(!out.status.success(), "{args:?}");
        assert_eq!(stdout(&out), "", "{args:?}");
        assert_eq!(stderr(&out), expected, "{args:?}");
    }
    assert!(!env.launched.exists(), "mpv must never be spawned");
}

#[test]
fn start_with_the_bundled_script_installed_launches_the_target_mpv() {
    let env = Env::new("install = \"native\"\n");
    write_file(&env.native_script(), BUNDLED_SCRIPT);

    let out = env.run(&["start", "Monster"]);

    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stderr(&out), "");
    assert_eq!(
        env.wait_for_launch(),
        vec![
            "--{".to_string(),
            "--start=0".to_string(),
            env.episode().display().to_string(),
            "--}".to_string(),
        ]
    );
}

#[test]
fn start_with_a_script_that_differs_from_the_bundled_one_launches_silently() {
    let env = Env::new("install = \"native\"\n");
    write_file(&env.native_script(), "-- an older tsuzuki.lua\n");

    let out = env.run(&["start", "Monster"]);

    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stderr(&out), "");
    assert_eq!(env.wait_for_launch().len(), 4);
}

#[test]
fn other_commands_with_the_script_missing_print_nothing_about_it() {
    let env = Env::new("install = \"native\"\n");
    let log = env.home.join(".local/share/tsuzuki/mpv-log/e1");
    write_file(
        &log,
        &format!(
            "# {}\nduration=1420\nposition=300\nreason=quit\n",
            env.episode().display()
        ),
    );

    for args in [&["status"][..], &["list"][..], &["pause", "Monster"][..]] {
        let out = env.run(args);
        assert!(out.status.success(), "{args:?}: {}", stderr(&out));
        assert_eq!(stderr(&out), "", "{args:?}");
    }
}

#[test]
fn custom_without_script_path_launches_without_any_check() {
    let env = Env::new(&format!(
        "install = \"custom\"\ncommand = \"{}\"\n",
        "mpv --fs"
    ));

    let out = env.run(&["start", "Monster"]);

    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stderr(&out), "");
    assert_eq!(env.wait_for_launch()[0], "--fs");
}

#[test]
fn custom_with_a_missing_script_path_is_blocked_naming_that_path() {
    let env = Env::new(
        "install = \"custom\"\ncommand = \"mpv\"\nscript_path = \"~/my-mpv/tsuzuki.lua\"\n",
    );

    let out = env.run(&["start", "Monster"]);

    assert!(!out.status.success());
    assert_eq!(
        stderr(&out),
        format!(
            "error: mpv script not installed for custom mpv at {} — run: tsuzuki doctor --fix\n",
            env.home.join("my-mpv/tsuzuki.lua").display()
        )
    );
    assert!(!env.launched.exists());
}

#[test]
fn auto_picks_native_over_flatpak_and_checks_only_the_native_script() {
    let env = Env::new("install = \"auto\"\n");
    env.install_flatpak();
    write_file(&env.flatpak_script(), BUNDLED_SCRIPT);

    let out = env.run(&["start", "Monster"]);

    assert!(!out.status.success());
    assert_eq!(
        stderr(&out),
        format!(
            "error: mpv script not installed for native mpv at {} — run: tsuzuki doctor --fix\n",
            env.native_script().display()
        )
    );
}

#[test]
fn pinned_native_with_only_a_snap_mpv_on_path_is_not_installed() {
    let env = Env::new("install = \"native\"\n");
    let launcher = env.bin.parent().unwrap().join("snapd/snap");
    fs::create_dir_all(launcher.parent().unwrap()).unwrap();
    write_executable(&launcher, "#!/bin/sh\n");
    fs::remove_file(env.bin.join("mpv")).unwrap();
    std::os::unix::fs::symlink(&launcher, env.bin.join("mpv")).unwrap();

    let out = env.run(&["start", "Monster"]);

    assert!(!out.status.success());
    assert_eq!(
        stderr(&out),
        "error: [mpv] install is \"native\" but native mpv is not installed — install it, or change [mpv] install in config.toml\n"
    );
}

#[test]
fn an_unsupported_install_value_is_a_config_error() {
    let env = Env::new("install = \"snap\"\n");

    let out = env.run(&["status"]);

    assert!(!out.status.success());
    assert!(
        stderr(&out).starts_with("error: could not read config at "),
        "{}",
        stderr(&out)
    );
}

// --- doctor ---

#[test]
fn doctor_checks_and_fixes_only_the_target_and_names_other_installs() {
    let env = Env::new("install = \"native\"\n");
    env.install_flatpak();

    let before = stdout(&env.run(&["doctor"]));
    assert!(
        before.contains(
            "✓ mpv — native (pinned)\n  → also found: Flatpak mpv; to use it, set [mpv] install = \"flatpak\"\n"
        ),
        "{before}"
    );
    assert!(
        before.contains("✗ mpv script — not installed (native)\n  → tsuzuki doctor --fix\n"),
        "{before}"
    );
    assert!(!before.contains("mpv permissions"), "{before}");

    let fix = env.run(&["doctor", "--fix"]);
    assert!(
        stdout(&fix).contains("✓ fixed: mpv script installed for native mpv\n"),
        "{}",
        stdout(&fix)
    );
    assert_eq!(
        fs::read_to_string(env.native_script()).unwrap(),
        BUNDLED_SCRIPT
    );
    assert!(!env.flatpak_script().exists(), "Flatpak isn't the target");

    let after = stdout(&env.run(&["doctor"]));
    assert!(
        after.contains("✓ mpv script — up to date (native)\n"),
        "{after}"
    );
}

#[test]
fn doctor_auto_reports_why_the_target_was_chosen() {
    let env = Env::new("install = \"auto\"\n");

    let out = stdout(&env.run(&["doctor"]));

    assert!(out.contains("✓ mpv — native (auto)\n"), "{out}");
}

#[test]
fn doctor_custom_with_script_path_checks_and_fixes_that_path() {
    let env = Env::new(
        "install = \"custom\"\ncommand = \"mpv\"\nscript_path = \"~/my-mpv/tsuzuki.lua\"\n",
    );
    let script = env.home.join("my-mpv/tsuzuki.lua");

    let before = stdout(&env.run(&["doctor"]));
    assert!(before.contains("✓ mpv — custom (pinned)\n"), "{before}");
    assert!(
        before.contains("✗ mpv script — not installed (custom)\n"),
        "{before}"
    );

    env.run(&["doctor", "--fix"]);
    assert_eq!(fs::read_to_string(&script).unwrap(), BUNDLED_SCRIPT);
    assert!(!env.native_script().exists());
}

#[test]
fn doctor_custom_without_script_path_warns_not_verified_and_does_not_fail_for_it() {
    let env = Env::new("install = \"custom\"\ncommand = \"mpv\"\n");

    let out = env.run(&["doctor", "--fix"]);
    let text = stdout(&out);

    assert!(
        text.contains(
            "⚠ mpv script — not verified (custom)\n  → set [mpv] script_path in config.toml\n"
        ),
        "{text}"
    );
    assert!(!env.native_script().exists());
}

#[test]
fn doctor_custom_command_not_found_fails() {
    let env = Env::new("install = \"custom\"\ncommand = \"/nowhere/mpv\"\n");

    let out = env.run(&["doctor"]);

    assert!(!out.status.success());
    assert!(
        stdout(&out)
            .contains("✗ mpv — custom command not found\n  → check [mpv] command in config.toml\n"),
        "{}",
        stdout(&out)
    );
}

// --- log reading is independent of the target ---

#[test]
fn entries_from_native_and_flatpak_log_dirs_both_reconcile_whatever_the_target() {
    for mpv_table in [
        "install = \"native\"\n",
        "install = \"custom\"\ncommand = \"mpv\"\n",
    ] {
        let env = Env::new(mpv_table);
        fs::create_dir_all(env.home.join("Anime/Naruto")).unwrap();
        let naruto = env.home.join("Anime/Naruto/ep01.mkv");
        fs::File::create(&naruto).unwrap();
        write_file(
            &env.home.join(".local/share/tsuzuki/mpv-log/e1"),
            &format!(
                "# {}\nduration=1420\nposition=300\nreason=quit\n",
                env.episode().display()
            ),
        );
        write_file(
            &env.home.join(".var/app/io.mpv.Mpv/data/tsuzuki/mpv-log/e2"),
            &format!(
                "# {}\nduration=1420\nposition=400\nreason=quit\n",
                naruto.display()
            ),
        );

        let out = env.run(&["status"]);

        assert_eq!(
            stdout(&out),
            "Monster — ep. 1\nNaruto — ep. 1\n\nRunning `tsuzuki` bare will prompt among these.\n",
            "{mpv_table}"
        );
    }
}

// --- launch output and neutral launch line ---

#[test]
fn launch_prints_a_neutral_line_because_tsuzuki_cannot_know_mpv_succeeded() {
    let env = Env::new("install = \"native\"\n");
    write_file(&env.native_script(), BUNDLED_SCRIPT);

    let out = env.run(&["start", "Monster"]);

    assert_eq!(
        stdout(&out),
        format!(
            "→ Starting \"Monster\" — episode 1 (00:00)\n  → {}\n",
            env.episode().display()
        )
    );
    assert_eq!(stderr(&out), "");
}

#[test]
fn launch_log_holds_the_command_line_then_what_mpv_wrote_to_stdout_and_stderr() {
    let env = Env::new("install = \"native\"\n");
    write_file(&env.native_script(), BUNDLED_SCRIPT);
    env.set_mpv_body("echo out-line\necho err-line >&2");

    let out = env.run(&["start", "Monster"]);
    env.wait_for_launch();

    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        fs::read_to_string(env.launch_log()).unwrap(),
        format!(
            "# mpv --{{ --start=0 {} --}}\nout-line\nerr-line\n",
            env.episode().display()
        )
    );
}

#[test]
fn launch_log_is_overwritten_by_the_next_launch() {
    let env = Env::new("install = \"native\"\n");
    write_file(&env.native_script(), BUNDLED_SCRIPT);
    env.set_mpv_body("echo first-launch");
    env.run(&["start", "Monster"]);
    env.wait_for_launch();
    fs::remove_file(&env.launched).unwrap();

    env.set_mpv_body("echo second-launch");
    env.run(&["start", "Monster"]);
    env.wait_for_launch();

    assert_eq!(
        fs::read_to_string(env.launch_log()).unwrap(),
        format!(
            "# mpv --{{ --start=0 {} --}}\nsecond-launch\n",
            env.episode().display()
        )
    );
}

#[test]
fn launch_with_an_unwritable_launch_log_still_spawns_mpv_without_any_message() {
    let env = Env::new("install = \"native\"\n");
    write_file(&env.native_script(), BUNDLED_SCRIPT);
    fs::create_dir_all(env.launch_log()).unwrap();

    let out = env.run(&["start", "Monster"]);

    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stderr(&out), "");
    assert_eq!(env.wait_for_launch().len(), 4);
}

// --- a blank script is not installed ---

#[test]
fn start_with_an_empty_or_whitespace_only_script_is_blocked_and_never_launches() {
    for contents in ["", "  \n\t\n"] {
        let env = Env::new("install = \"native\"\n");
        write_file(&env.native_script(), contents);

        let out = env.run(&["start", "Monster"]);

        assert!(!out.status.success(), "{contents:?}");
        assert_eq!(stdout(&out), "", "{contents:?}");
        assert_eq!(
            stderr(&out),
            format!(
                "error: mpv script not installed for native mpv at {} — run: tsuzuki doctor --fix\n",
                env.native_script().display()
            ),
            "{contents:?}"
        );
        assert!(!env.launched.exists(), "{contents:?}");
    }
}

#[test]
fn doctor_reports_a_blank_script_as_not_installed_and_fix_replaces_it() {
    let env = Env::new("install = \"native\"\n");
    write_file(&env.native_script(), "\n  \n");

    let before = stdout(&env.run(&["doctor"]));
    env.run(&["doctor", "--fix"]);

    assert!(
        before.contains("✗ mpv script — not installed (native)\n  → tsuzuki doctor --fix\n"),
        "{before}"
    );
    assert_eq!(
        fs::read_to_string(env.native_script()).unwrap(),
        BUNDLED_SCRIPT
    );
}

// --- MPV_HOME ---

#[test]
fn native_script_lives_under_mpv_home_when_it_is_set() {
    let env = Env::new("install = \"native\"\n");
    let mpv_home = env.home.join("portable-mpv");
    let expected = mpv_home.join("scripts/tsuzuki.lua");

    let blocked = env.run_with_env(&["start", "Monster"], &[("MPV_HOME", &mpv_home)]);
    let fix = env.run_with_env(&["doctor", "--fix"], &[("MPV_HOME", &mpv_home)]);
    let launched = env.run_with_env(&["start", "Monster"], &[("MPV_HOME", &mpv_home)]);

    assert_eq!(
        stderr(&blocked),
        format!(
            "error: mpv script not installed for native mpv at {} — run: tsuzuki doctor --fix\n",
            expected.display()
        )
    );
    assert!(stdout(&fix).contains("✓ fixed: mpv script installed for native mpv\n"));
    assert_eq!(fs::read_to_string(&expected).unwrap(), BUNDLED_SCRIPT);
    assert!(
        !env.native_script().exists(),
        "XDG path must stay untouched"
    );
    assert!(launched.status.success(), "{}", stderr(&launched));
}

#[test]
fn an_empty_mpv_home_is_ignored() {
    let env = Env::new("install = \"native\"\n");

    let out = env.run_with_env(&["start", "Monster"], &[("MPV_HOME", Path::new(""))]);

    assert_eq!(
        stderr(&out),
        format!(
            "error: mpv script not installed for native mpv at {} — run: tsuzuki doctor --fix\n",
            env.native_script().display()
        )
    );
}

// --- Flatpak needs its CLI ---

#[test]
fn flatpak_install_dir_without_the_flatpak_cli_is_not_installed() {
    let env = Env::new("install = \"flatpak\"\n");
    env.install_flatpak_dir_only();

    let out = env.run(&["start", "Monster"]);

    assert!(!out.status.success());
    assert_eq!(
        stderr(&out),
        "error: [mpv] install is \"flatpak\" but Flatpak mpv is not installed — install it, or change [mpv] install in config.toml\n"
    );
}

#[test]
fn auto_with_only_a_flatpak_install_dir_and_no_cli_finds_no_mpv() {
    let env = Env::new("install = \"auto\"\n");
    fs::remove_file(env.bin.join("mpv")).unwrap();
    env.install_flatpak_dir_only();

    let out = env.run(&["start", "Monster"]);

    assert_eq!(
        stderr(&out),
        "error: could not find mpv — install it, or set [mpv] install = \"custom\" and command in config.toml\n"
    );
}

// --- doctor --fix keeps a symlinked script ---

#[test]
fn doctor_fix_writes_through_a_symlinked_script_and_keeps_the_link() {
    let env = Env::new("install = \"native\"\n");
    let real = env.home.join("dotfiles/tsuzuki.lua");
    write_file(&real, "-- an older tsuzuki.lua\n");
    fs::create_dir_all(env.native_script().parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&real, env.native_script()).unwrap();

    let out = env.run(&["doctor", "--fix"]);

    assert!(out.status.success(), "{}", stdout(&out));
    assert_eq!(fs::read_link(env.native_script()).unwrap(), real);
    assert_eq!(fs::read_to_string(&real).unwrap(), BUNDLED_SCRIPT);
}

#[test]
fn doctor_fix_with_a_dangling_script_symlink_fails_and_leaves_the_link() {
    let env = Env::new("install = \"native\"\n");
    let missing = env.home.join("dotfiles/gone.lua");
    fs::create_dir_all(env.native_script().parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&missing, env.native_script()).unwrap();

    let out = env.run(&["doctor", "--fix"]);

    assert!(!out.status.success());
    assert_eq!(fs::read_link(env.native_script()).unwrap(), missing);
    assert!(!missing.exists());
}

// --- doctor: Flatpak access per tracked root ---

fn permissions_lines(out: &Output) -> Vec<String> {
    let text = stdout(out);
    let mut lines = text.lines().peekable();
    let mut found = Vec::new();
    while let Some(line) = lines.next() {
        if line.contains("mpv permissions") {
            found.push(line.to_string());
            while let Some(next) = lines.peek().filter(|l| l.starts_with("  →")) {
                found.push(next.to_string());
                lines.next();
            }
        }
    }
    found
}

#[test]
fn doctor_flatpak_names_a_symlinked_root_whose_target_is_hidden() {
    let env = Env::new("install = \"flatpak\"\n");
    env.install_flatpak();
    let hidden = env.home.join("Hidden/Anime");
    fs::create_dir_all(&hidden).unwrap();
    let link = env.home.join("Linked");
    std::os::unix::fs::symlink(&hidden, &link).unwrap();
    let hidden = hidden.canonicalize().unwrap();
    env.write_config("install = \"flatpak\"\n");
    let config = env.home.join(".config/tsuzuki/config.toml");
    fs::write(
        &config,
        format!(
            "[roots]\npaths = [\"{}\", \"{}\"]\n\n[mpv]\ninstall = \"flatpak\"\n",
            env.home.join("Anime").display(),
            link.display()
        ),
    )
    .unwrap();
    // Answers per probed path, so only the symlink's real target is hidden.
    write_executable(
        &env.bin.join("flatpak"),
        &format!(
            "#!/bin/sh\ncase \"$2\" in\n  --file-access={0}) echo hidden;;\n  *) echo read-only;;\nesac\n",
            hidden.display()
        ),
    );

    let out = env.run(&["doctor"]);

    assert_eq!(
        permissions_lines(&out),
        vec![
            format!("⚠ mpv permissions — mpv can't see: {}", hidden.display()),
            format!(
                "  → flatpak override --user --filesystem={}:ro io.mpv.Mpv",
                hidden.display()
            ),
        ]
    );
}

#[test]
fn doctor_flatpak_with_host_read_only_access_reports_accessible() {
    let env = Env::new("install = \"flatpak\"\n");
    env.install_flatpak();
    write_executable(&env.bin.join("flatpak"), "#!/bin/sh\necho read-only\n");

    let out = env.run(&["doctor"]);

    assert_eq!(
        permissions_lines(&out),
        vec!["✓ mpv permissions — tracked directories accessible"]
    );
}

#[test]
fn doctor_flatpak_with_an_unusable_flatpak_cli_could_not_verify() {
    let env = Env::new("install = \"flatpak\"\n");
    env.install_flatpak();
    write_executable(&env.bin.join("flatpak"), "#!/bin/sh\nexit 1\n");

    let out = env.run(&["doctor"]);

    assert_eq!(
        permissions_lines(&out),
        vec![
            "⚠ mpv permissions — could not verify",
            "  → flatpak info --file-access=<root> io.mpv.Mpv"
        ]
    );
}
