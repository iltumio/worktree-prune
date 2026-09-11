use std::{
    fs,
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
        .args(args)
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
