use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};
use tempfile::TempDir;
struct Fixture {
    _temp: TempDir,
    main: PathBuf,
    base: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let main = temp.path().join("repo");
        fs::create_dir(&main).unwrap();
        git(&main, &["init", "-b", "main"]);
        git(&main, &["config", "user.name", "Test"]);
        git(&main, &["config", "user.email", "test@example.invalid"]);
        git(&main, &["config", "commit.gpgsign", "false"]);
        git(&main, &["config", "core.hooksPath", "/dev/null"]);
        fs::write(main.join("tracked"), "initial").unwrap();
        git(&main, &["add", "."]);
        git(&main, &["commit", "-m", "initial"]);
        let base = temp.path().join("targets");
        Self {
            _temp: temp,
            main,
            base,
        }
    }
    fn wt(&self, name: &str) -> PathBuf {
        let path = self.main.join(".worktrees").join(name);
        git(
            &self.main,
            &["worktree", "add", "-b", name, path.to_str().unwrap()],
        );
        path
    }
    fn target(&self, name: &str) -> PathBuf {
        let path = self.base.join("repo").join(name).join("target");
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("cache"), "cache").unwrap();
        path
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_worktree-prune"))
            .current_dir(&self.main)
            .env("CARGO_TARGET_BASE_DIR", &self.base)
            .args(args)
            .output()
            .unwrap()
    }
}
fn git(path: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(path)
        .arg(args[0])
        .args(&args[1..])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
fn success(out: Output) {
    assert!(
        out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
#[test]
fn dry_run_then_remove_target_and_branch() {
    let f = Fixture::new();
    let wt = f.wt("feature");
    let td = f.target("feature");
    success(f.run(&["feature"]));
    assert!(wt.exists() && td.exists());
    success(f.run(&["feature", "--yes", "--delete-branch"]));
    assert!(!wt.exists() && !td.exists());
    assert!(
        !Command::new("git")
            .arg("-C")
            .arg(&f.main)
            .args(["show-ref", "--verify", "refs/heads/feature"])
            .output()
            .unwrap()
            .status
            .success()
    );
}
#[test]
fn one_dirty_selection_blocks_entire_batch() {
    let f = Fixture::new();
    let a = f.wt("clean");
    let b = f.wt("dirty");
    fs::write(b.join("untracked"), "precious").unwrap();
    assert!(!f.run(&["clean", "dirty", "--yes"]).status.success());
    assert!(a.exists() && b.exists());
    success(f.run(&["dirty", "--yes", "--force"]));
    assert!(!b.exists());
}
#[test]
fn local_only_commits_are_protected() {
    let f = Fixture::new();
    let wt = f.wt("local");
    fs::write(wt.join("tracked"), "local").unwrap();
    git(&wt, &["commit", "-am", "local"]);
    assert!(!f.run(&["local", "--yes"]).status.success());
    assert!(wt.exists());
}
#[test]
fn main_and_locked_are_protected_even_with_force() {
    let f = Fixture::new();
    let wt = f.wt("locked");
    git(&f.main, &["worktree", "lock", wt.to_str().unwrap()]);
    assert!(!f.run(&["locked", "--yes", "--force"]).status.success());
    assert!(
        !f.run(&[f.main.to_str().unwrap(), "--yes", "--force"])
            .status
            .success()
    );
    assert!(wt.exists() && f.main.exists());
}
#[test]
fn shared_and_external_targets_are_kept() {
    let f = Fixture::new();
    let a = f.wt("a");
    let b = f.wt("b");
    let td = f.target("shared");
    for wt in [&a, &b] {
        fs::create_dir_all(wt.join(".cargo")).unwrap();
        fs::write(
            wt.join(".cargo/config.toml"),
            format!("[build]\ntarget-dir = '{}'\n", td.display()),
        )
        .unwrap();
    }
    success(f.run(&["a", "--yes", "--force"]));
    assert!(td.exists());
    let outside = f._temp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(
        b.join(".cargo/config.toml"),
        format!("[build]\ntarget-dir = '{}'", outside.display()),
    )
    .unwrap();
    success(f.run(&["b", "--yes", "--force"]));
    assert!(outside.exists());
}
#[test]
fn symlink_escape_target_is_kept() {
    let f = Fixture::new();
    let wt = f.wt("feature");
    let outside = f._temp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::create_dir_all(f.base.join("repo/feature")).unwrap();
    std::os::unix::fs::symlink(&outside, f.base.join("repo/feature/target")).unwrap();
    success(f.run(&["feature", "--yes"]));
    assert!(!wt.exists() && outside.exists());
}
#[test]
fn orphans_preserve_live_stale_and_main_targets() {
    let f = Fixture::new();
    f.wt("live");
    let live = f.target("live");
    let main = f.target("main");
    let orphan = f.target("orphan");
    let stale = f.main.join(".worktrees/stale");
    fs::create_dir(&stale).unwrap();
    let st = f.target("stale");
    success(f.run(&["--orphans", "--yes"]));
    assert!(!orphan.exists());
    assert!(live.exists() && main.exists() && st.exists() && stale.exists());
}
#[test]
fn stale_requires_force_and_unrelated_clone_survives() {
    let f = Fixture::new();
    f.wt("live");
    let stale = f.main.join(".worktrees/stale");
    fs::create_dir(&stale).unwrap();
    fs::write(stale.join("precious"), "data").unwrap();
    let target = f.target("stale");
    let unrelated = f.main.join(".worktrees/unrelated");
    fs::create_dir(&unrelated).unwrap();
    git(&unrelated, &["init"]);
    assert!(!f.run(&["--stale", "--yes"]).status.success());
    assert!(stale.exists());
    success(f.run(&["--stale", "--yes", "--force"]));
    assert!(!stale.exists() && !target.exists() && unrelated.exists());
}
#[test]
fn paths_with_spaces_and_newlines() {
    let f = Fixture::new();
    let wt = f.main.join(".worktrees/with space\nand newline");
    git(
        &f.main,
        &["worktree", "add", "-b", "feature", wt.to_str().unwrap()],
    );
    success(f.run(&[wt.to_str().unwrap(), "--yes"]));
    assert!(!wt.exists());
}
#[test]
fn keep_target_and_quiet_hook_behavior() {
    let f = Fixture::new();
    f.wt("a");
    let td = f.target("a");
    success(f.run(&["a", "--yes", "--keep-target"]));
    assert!(td.exists());
    let out = f.run(&["unknown", "--quiet", "--yes"]);
    assert!(out.status.success() && out.stderr.is_empty());
}
#[test]
fn tui_requires_terminal_and_help_needs_no_repo() {
    let f = Fixture::new();
    let plain = f.run(&[]);
    assert!(!plain.status.success());
    assert!(String::from_utf8_lossy(&plain.stderr).contains("use --list"));
    for args in [
        &["--list"][..],
        &["--dry-run"][..],
        &["--quiet"][..],
        &["-C", "."][..],
    ] {
        success(f.run(args));
    }
    success(
        Command::new(env!("CARGO_BIN_EXE_worktree-prune"))
            .current_dir(f._temp.path())
            .arg("--help")
            .output()
            .unwrap(),
    );
}
#[test]
fn ambiguous_basename_is_rejected() {
    let f = Fixture::new();
    for (parent, branch) in [("one", "one"), ("two", "two")] {
        let wt = f._temp.path().join(parent).join("same");
        git(
            &f.main,
            &["worktree", "add", "-b", branch, wt.to_str().unwrap()],
        );
    }
    assert!(!f.run(&["same", "--yes"]).status.success());
}
#[test]
fn sudo_removes_unwritable_worktree_and_target() {
    let f = Fixture::new();
    let wt = f.wt("locked_perm");
    let td = f.target("locked_perm");
    let sub = wt.join("sub");
    fs::create_dir(&sub).unwrap();
    fs::write(sub.join("item"), "data").unwrap();
    fs::set_permissions(&sub, fs::Permissions::from_mode(0o500)).unwrap();
    assert!(!f.run(&["locked_perm", "--yes", "--force"]).status.success());
    let bin = f._temp.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let sudo = bin.join("sudo");
    fs::write(
        &sudo,
        "#!/bin/sh\nchmod -R u+rwx \"$5\" 2>/dev/null || true\nexec \"$@\"\n",
    )
    .unwrap();
    fs::set_permissions(&sudo, fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new(env!("CARGO_BIN_EXE_worktree-prune"))
        .current_dir(&f.main)
        .env("CARGO_TARGET_BASE_DIR", &f.base)
        .env("PATH", path)
        .args(["locked_perm", "--yes", "--force", "--sudo"])
        .output()
        .unwrap();
    if sub.exists() {
        let _ = fs::set_permissions(&sub, fs::Permissions::from_mode(0o700));
    }
    success(out);
    assert!(!wt.exists() && !td.exists());
}

fn run_from(f: &Fixture, root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_worktree-prune"))
        .current_dir(root)
        .env("CARGO_TARGET_BASE_DIR", &f.base)
        .args(args)
        .output()
        .unwrap()
}

fn nested_repo(f: &Fixture) -> PathBuf {
    let other = f.main.join("nested/deep/other");
    fs::create_dir_all(other.parent().unwrap()).unwrap();
    git(
        &f.main,
        &["clone", f.main.to_str().unwrap(), other.to_str().unwrap()],
    );
    other
}

#[test]
fn recursive_discovery_deduplicates_and_lists_external_worktrees() {
    let f = Fixture::new();
    f.wt("first");
    let other = nested_repo(&f);
    let external = tempfile::tempdir().unwrap();
    let wt = external.path().join("external-feature");
    git(
        &other,
        &["worktree", "add", "-b", "second", wt.to_str().unwrap()],
    );
    std::os::unix::fs::symlink(f._temp.path(), f.main.join("loop")).unwrap();
    for out in [
        run_from(&f, f._temp.path(), &["--list"]),
        f.run(&["-C", f._temp.path().to_str().unwrap(), "--list"]),
    ] {
        let text = String::from_utf8_lossy(&out.stdout);
        assert_eq!(text.matches("Repository:").count(), 2, "{text}");
        assert_eq!(text.matches("external-feature").count(), 2, "{text}");
        assert!(text.contains("first") && text.contains("second"), "{text}");
        success(out);
    }
}

#[test]
fn recursive_batch_rejects_ambiguity_and_blocks_all_repos_before_removal() {
    let f = Fixture::new();
    let first = f.wt("same");
    let other = nested_repo(&f);
    let second = other.join(".worktrees/same");
    git(
        &other,
        &["worktree", "add", "-b", "second", second.to_str().unwrap()],
    );
    let out = f.run(&["same", "--yes"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("ambiguous"));
    fs::write(second.join("precious"), "keep").unwrap();
    let args = [first.to_str().unwrap(), second.to_str().unwrap(), "--yes"];
    assert!(!f.run(&args).status.success());
    assert!(first.exists() && second.exists());
    fs::remove_file(second.join("precious")).unwrap();
    success(f.run(&args));
    assert!(!first.exists() && !second.exists());
}

#[test]
fn recursive_batch_preserves_targets_shared_across_repositories() {
    let f = Fixture::new();
    let first = f.wt("first");
    let other = nested_repo(&f);
    let second = other.join(".worktrees/second");
    git(
        &other,
        &["worktree", "add", "-b", "second", second.to_str().unwrap()],
    );
    let target = f.target("shared");
    for path in [&first, &second] {
        fs::create_dir(path.join(".cargo")).unwrap();
        fs::write(
            path.join(".cargo/config.toml"),
            format!("[build]\ntarget-dir = '{}'", target.display()),
        )
        .unwrap();
    }
    success(f.run(&["first", "second", "--yes", "--force"]));
    assert!(!first.exists() && !second.exists() && target.exists());
}

#[test]
fn discovery_handles_empty_directories_and_bare_repositories() {
    let f = Fixture::new();
    let empty = tempfile::tempdir().unwrap();
    let out = run_from(&f, empty.path(), &["--list"]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("No Git repositories"));
    success(out);
    let bare = empty.path().join("bare.git");
    git(
        &f.main,
        &[
            "clone",
            "--bare",
            f.main.to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );
    let wt = empty.path().join("linked");
    git(
        &bare,
        &["worktree", "add", "-b", "linked", wt.to_str().unwrap()],
    );
    let out = run_from(&f, empty.path(), &["--list"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(text.matches("Repository:").count(), 1, "{text}");
    assert!(text.contains("linked"));
    success(out);
}

#[test]
fn parallel_checks_preserve_order_and_block_the_entire_batch() {
    let f = Fixture::new();
    let clean = f.wt("clean");
    let dirty = f.wt("dirty");
    let locked = f.wt("locked");
    fs::write(dirty.join("precious"), "keep").unwrap();
    git(&f.main, &["worktree", "lock", locked.to_str().unwrap()]);
    let other = nested_repo(&f);
    let second = other.join(".worktrees/second");
    git(
        &other,
        &["worktree", "add", "-b", "second", second.to_str().unwrap()],
    );
    for args in [
        vec!["--list"],
        vec!["clean", "second"],
        vec!["clean", "dirty", "locked", "second", "--yes"],
    ] {
        let run = |threads: &str| {
            Command::new(env!("CARGO_BIN_EXE_worktree-prune"))
                .current_dir(&f.main)
                .env("CARGO_TARGET_BASE_DIR", &f.base)
                .env("WTP_THREADS", threads)
                .args(&args)
                .output()
                .unwrap()
        };
        let serial = run("1");
        let parallel = run("4");
        assert_eq!(serial.status.code(), parallel.status.code());
        assert_eq!(serial.stdout, parallel.stdout);
        assert_eq!(serial.stderr, parallel.stderr);
        if args.contains(&"--yes") {
            assert!(!parallel.status.success());
            let stderr = String::from_utf8_lossy(&parallel.stderr);
            assert!(stderr.contains("uncommitted") && stderr.contains("locked worktree"));
        } else {
            success(parallel);
        }
        assert!(clean.exists() && dirty.exists() && locked.exists() && second.exists());
    }
}

#[test]
fn startup_loads_xdg_config_and_rejects_invalid_config_before_removal() {
    let f = Fixture::new();
    let wt = f.wt("feature");
    let config_root = f._temp.path().join("config");
    fs::create_dir_all(config_root.join("worktree-prune")).unwrap();
    let config = config_root.join("worktree-prune/config.toml");
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_worktree-prune"))
            .current_dir(&f.main)
            .env("CARGO_TARGET_BASE_DIR", &f.base)
            .env("XDG_CONFIG_HOME", &config_root)
            .env_remove("WTP_THREADS")
            .env("RAYON_NUM_THREADS", "invalid-ignored")
            .args(args)
            .output()
            .unwrap()
    };
    fs::write(&config, "threads = 2").unwrap();
    success(run(&["--list"]));
    fs::write(&config, "threads = 0").unwrap();
    let out = run(&["feature", "--yes"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("config.toml"));
    assert!(wt.exists());
    success(run(&["--help"]));
    success(run(&["--version"]));

    let home = f._temp.path().join("home");
    fs::create_dir_all(home.join(".config/worktree-prune")).unwrap();
    fs::write(
        home.join(".config/worktree-prune/config.toml"),
        "threads = 0",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_worktree-prune"))
        .current_dir(&f.main)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", "relative-is-ignored")
        .env_remove("WTP_THREADS")
        .arg("--list")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains(".config/worktree-prune/config.toml"));
}
