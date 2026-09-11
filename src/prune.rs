use crate::Args;
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::BTreeSet,
    ffi::OsStr,
    fs,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{Component, Path, PathBuf},
    process::{Command, Output},
};
use walkdir::WalkDir;

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Main,
    Worktree,
    Stale,
    Orphan,
}
#[derive(Clone, Debug)]
pub struct Entry {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub target: PathBuf,
    pub kind: Kind,
    pub locked: bool,
}
impl Entry {
    pub fn name(&self) -> String {
        self.path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    }
}
pub struct Repo {
    root: PathBuf,
    common: PathBuf,
    main: PathBuf,
    base: PathBuf,
    key: PathBuf,
}
pub struct Item {
    entry: Entry,
    target: bool,
}
pub struct Plan {
    pub items: Vec<Item>,
    pub errors: Vec<String>,
    notes: Vec<String>,
}
impl Plan {
    pub fn describe(&self) -> String {
        let mut lines = self.notes.clone();
        for item in &self.items {
            lines.push(format!(
                "REMOVE {:?}: {}",
                item.entry.kind,
                item.entry.path.display()
            ));
            if item.target {
                lines.push(format!("  cargo target: {}", item.entry.target.display()));
            }
        }
        lines.extend(self.errors.iter().map(|e| format!("BLOCKED: {e}")));
        if lines.is_empty() {
            lines.push("Nothing to do.".into());
        }
        lines.join("\n")
    }
}
fn output(cmd: &mut Command) -> Result<Output> {
    let out = cmd
        .output()
        .with_context(|| format!("cannot run {cmd:?}"))?;
    ensure!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(out)
}
fn text(cmd: &mut Command) -> Result<String> {
    Ok(String::from_utf8(output(cmd)?.stdout)?.trim().into())
}
// Resolve existing ancestors too, so missing paths and symlink aliases compare safely.
fn absolute(path: &Path) -> Result<PathBuf> {
    let full = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut out = PathBuf::new();
    for c in full.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => (),
            other => {
                out.push(other.as_os_str());
                if out.exists() {
                    out = fs::canonicalize(&out)?;
                } else if out.is_symlink() {
                    bail!("dangling symlink: {}", out.display());
                }
            }
        }
    }
    Ok(out)
}
fn overlaps(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}
impl Repo {
    pub fn open(path: &Path) -> Result<Self> {
        let root = absolute(path)?;
        let common = absolute(Path::new(&text(
            Command::new("git").arg("-C").arg(&root).args([
                "rev-parse",
                "--path-format=absolute",
                "--git-common-dir",
            ]),
        )?))?;
        let main = if common.file_name() == Some(OsStr::new(".git")) {
            common.parent().unwrap().to_owned()
        } else {
            common.clone()
        };
        let name = if common.file_name() == Some(OsStr::new(".git")) {
            main.file_name().unwrap()
        } else {
            common.file_stem().context("repository has no name")?
        };
        let base = absolute(
            &std::env::var_os("CARGO_TARGET_BASE_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/.cargo-targets/targets")),
        )?;
        let key = base.join(name);
        ensure!(
            base.parent().is_some(),
            "Cargo target base must not be the filesystem root"
        );
        Ok(Self {
            root,
            common,
            main,
            base,
            key,
        })
    }
    fn git(&self) -> Command {
        let mut c = Command::new("git");
        c.arg("-C").arg(&self.common);
        c
    }
    fn registered(&self) -> Result<Vec<Entry>> {
        let raw = output(self.git().args(["worktree", "list", "--porcelain", "-z"]))?.stdout;
        let mut result = Vec::new();
        let mut current: Option<Entry> = None;
        for field in raw.split(|b| *b == 0) {
            if let Some(path) = field.strip_prefix(b"worktree ") {
                if let Some(e) = current.take() {
                    result.push(e);
                }
                let path = absolute(Path::new(OsStr::from_bytes(path)))?;
                let kind = if path == self.main || path == self.common {
                    Kind::Main
                } else {
                    Kind::Worktree
                };
                current = Some(Entry {
                    target: self.target(&path)?,
                    path,
                    kind,
                    branch: None,
                    locked: false,
                });
            } else if let Some(e) = &mut current {
                if let Some(branch) = field.strip_prefix(b"branch refs/heads/") {
                    e.branch = Some(String::from_utf8(branch.to_vec())?);
                }
                if field.starts_with(b"locked") {
                    e.locked = true;
                }
            }
        }
        if let Some(e) = current {
            result.push(e);
        }
        Ok(result)
    }
    fn target(&self, wt: &Path) -> Result<PathBuf> {
        // Cargo gives the extensionless spelling precedence when both exist.
        for cfg in [wt.join(".cargo/config"), wt.join(".cargo/config.toml")] {
            if cfg.exists() {
                let value: toml::Value = toml::from_str(&fs::read_to_string(&cfg)?)
                    .with_context(|| format!("invalid {}", cfg.display()))?;
                if let Some(td) = value.get("build").and_then(|b| b.get("target-dir")) {
                    let td = td.as_str().context("build.target-dir must be a string")?;
                    return absolute(&wt.join(td));
                }
                break;
            }
        }
        absolute(
            &self
                .key
                .join(if wt == self.main {
                    OsStr::new("main")
                } else {
                    wt.file_name().context("worktree has no name")?
                })
                .join("target"),
        )
    }
    pub fn inventory(&self, orphans: bool) -> Result<Vec<Entry>> {
        let mut entries = self.registered()?;
        let mut parents: BTreeSet<PathBuf> = entries
            .iter()
            .filter(|e| e.kind == Kind::Worktree)
            .filter_map(|e| e.path.parent().map(Path::to_owned))
            .collect();
        for p in [".claude/worktrees", ".worktrees", "worktrees"] {
            parents.insert(self.main.join(p));
        }
        for parent in parents {
            if !parent.is_dir() {
                continue;
            }
            for child in fs::read_dir(parent)? {
                let child = child?;
                if !child.file_type()?.is_dir() {
                    continue;
                }
                let path = absolute(&child.path())?;
                if entries
                    .iter()
                    .any(|e| overlaps(&e.path, &path) && e.kind != Kind::Main)
                    || path == self.main
                    || path == self.common
                {
                    continue;
                }
                let git = path.join(".git");
                let target = self.target(&path)?;
                let ours = if git.is_file() {
                    fs::read_to_string(&git)?
                        .trim()
                        .strip_prefix("gitdir: ")
                        .and_then(|s| absolute(&path.join(s)).ok())
                        .is_some_and(|p| p.starts_with(self.common.join("worktrees")))
                } else if git.exists() || git.is_symlink() {
                    false
                } else {
                    target.starts_with(&self.key)
                        && (target.is_dir()
                            || path.join(".cargo/config.toml").is_file()
                            || path.join(".cargo/config").is_file())
                };
                if ours {
                    entries.push(Entry {
                        path,
                        target,
                        branch: None,
                        kind: Kind::Stale,
                        locked: false,
                    });
                }
            }
        }
        if orphans && self.key.is_dir() {
            for child in fs::read_dir(&self.key)? {
                let child = child?;
                if !child.file_type()?.is_dir() || child.file_name() == "main" {
                    continue;
                }
                let path = absolute(&child.path())?;
                if !path.starts_with(&self.key)
                    || entries
                        .iter()
                        .any(|e| overlaps(&path, &e.target) || overlaps(&path, &e.path))
                {
                    continue;
                }
                entries.push(Entry {
                    target: path.clone(),
                    path,
                    branch: None,
                    kind: Kind::Orphan,
                    locked: false,
                });
            }
        }
        Ok(entries)
    }
    pub fn state(&self, entry: &Entry) -> Result<String> {
        if entry.kind != Kind::Worktree {
            return Ok(format!("{:?}", entry.kind).to_lowercase());
        }
        if !entry.path.is_dir() {
            return Ok("missing".into());
        }
        let tip = text(
            Command::new("git")
                .arg("-C")
                .arg(&entry.path)
                .args(["rev-parse", "HEAD"]),
        )?;
        let refs = text(self.git().args([
            "for-each-ref",
            "--format=%(refname)",
            "--contains",
            &tip,
            "refs/remotes",
            "refs/heads/main",
            "refs/heads/master",
        ]))?;
        if refs.lines().any(|r| {
            r == "refs/heads/main"
                || r == "refs/heads/master"
                || r == "refs/remotes/origin/main"
                || r == "refs/remotes/origin/master"
                || r.starts_with("refs/remotes/origin/sprint-")
        }) {
            return Ok("merged".into());
        }
        if refs.lines().any(|r| r.starts_with("refs/remotes/")) {
            return Ok("pushed".into());
        }
        // An old merged PR for a reused branch does not prove its current tip is safe.
        // Only local reachability evidence is accepted; no forge/network dependency.
        Ok("local-only".into())
    }
    pub fn dirty(&self, entry: &Entry) -> Result<bool> {
        let status = output(Command::new("git").arg("-C").arg(&entry.path).args([
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
        ]))?;
        Ok(!status.stdout.is_empty())
    }
    pub fn list(&self, entries: &[Entry], quiet: bool) {
        if quiet {
            return;
        }
        println!(
            "{:<25} {:<28} {:<12} {:<7} TARGET",
            "NAME", "BRANCH", "STATE", "DIRTY"
        );
        for e in entries {
            let state = self.state(e).unwrap_or_else(|_| "unknown".into());
            let dirty = if e.kind == Kind::Worktree || e.kind == Kind::Main {
                self.dirty(e)
                    .map(|d| if d { "yes" } else { "no" })
                    .unwrap_or("?")
            } else {
                "?"
            };
            println!(
                "{:<25} {:<28} {:<12} {:<7} {}  {}",
                e.name().escape_default(),
                e.branch.as_deref().unwrap_or("(detached)"),
                state,
                dirty,
                human(bytes(&e.target)),
                e.target.display()
            );
        }
    }
    fn target_safe(&self, entry: &Entry, entries: &[Entry]) -> bool {
        let td = &entry.target;
        td != &self.base
            && td != &self.key
            && td.starts_with(&self.base)
            && !overlaps(td, &self.key.join("main"))
            && !overlaps(td, &self.common)
            && !td.starts_with(&self.main)
            && !self.main.starts_with(td)
            && !entries
                .iter()
                .any(|e| e.path != entry.path && (overlaps(td, &e.target) || overlaps(td, &e.path)))
            && (entry.kind == Kind::Orphan || !entry.path.starts_with(td))
    }
    pub fn plan(&self, entries: &[Entry], args: &Args) -> Result<Plan> {
        let mut plan = Plan {
            items: vec![],
            errors: vec![],
            notes: vec![],
        };
        let mut selected = BTreeSet::new();
        for arg in &args.worktrees {
            let resolved = absolute(arg)?;
            let exact: Vec<_> = entries
                .iter()
                .enumerate()
                .filter(|(_, e)| e.path == resolved)
                .map(|(i, _)| i)
                .collect();
            let matches: Vec<_> = if !exact.is_empty() {
                exact
            } else {
                entries
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| {
                        arg.components().count() == 1 && e.path.file_name() == Some(arg.as_os_str())
                    })
                    .map(|(i, _)| i)
                    .collect()
            };
            if matches.len() == 1 {
                selected.insert(matches[0]);
            } else {
                plan.errors
                    .push(format!("{}: unknown or ambiguous worktree", arg.display()));
            }
        }
        for (i, e) in entries.iter().enumerate() {
            if (args.stale && e.kind == Kind::Stale) || (args.orphans && e.kind == Kind::Orphan) {
                selected.insert(i);
            }
        }
        for i in selected {
            let e = &entries[i];
            let check = (|| -> Result<()> {
                ensure!(e.kind != Kind::Main, "main checkout is protected");
                ensure!(!e.locked, "locked worktree; unlock it explicitly first");
                ensure!(
                    !self.root.starts_with(&e.path),
                    "cannot remove the current working directory or its ancestor; run from the main checkout"
                );
                ensure!(
                    !self.common.starts_with(&e.path) && !self.main.starts_with(&e.path),
                    "repository metadata is protected"
                );
                ensure!(
                    !entries
                        .iter()
                        .any(|other| other.path != e.path && other.path.starts_with(&e.path)),
                    "contains another worktree or target candidate"
                );
                match e.kind {
                    Kind::Worktree => {
                        ensure!(
                            e.path.is_dir(),
                            "missing worktree; use git worktree prune to clear its registration"
                        );
                        let dirty = self.dirty(e)?;
                        ensure!(
                            !dirty || args.force,
                            "uncommitted or untracked files; pass --force to discard"
                        );
                        let state = self.state(e)?;
                        ensure!(
                            args.force || state == "merged" || state == "pushed",
                            "commits are local-only; push them or pass --force"
                        );
                    }
                    Kind::Stale => ensure!(
                        args.force,
                        "stale files cannot be checked for uncommitted work; inspect them and pass --force"
                    ),
                    Kind::Orphan => {
                        ensure!(self.target_safe(e, entries), "unsafe orphan target path")
                    }
                    Kind::Main => unreachable!(),
                }
                if e.path.exists() {
                    preflight(&e.path, args.sudo)?;
                }
                Ok(())
            })();
            if let Err(err) = check {
                plan.errors.push(format!("{}: {err:#}", e.path.display()));
                continue;
            }
            let target = !args.keep_target
                && e.kind != Kind::Orphan
                && e.target.is_dir()
                && self.target_safe(e, entries);
            if target {
                if let Err(err) = preflight(&e.target, args.sudo) {
                    plan.errors.push(format!("{}: {err:#}", e.target.display()));
                    continue;
                }
            } else if e.kind != Kind::Orphan && e.target.is_dir() {
                plan.notes.push(format!(
                    "KEEP target (shared, protected, outside base or --keep-target): {}",
                    e.target.display()
                ));
            }
            if args.delete_branch
                && let Some(br) = &e.branch
            {
                plan.notes.push(format!("Delete local branch {br} after removing checkout (Git checks merge unless --force)."));
            }
            plan.items.push(Item {
                entry: e.clone(),
                target,
            });
        }
        Ok(plan)
    }
    pub fn execute(&self, plan: &Plan, args: &Args) -> Result<()> {
        ensure!(
            plan.errors.is_empty(),
            "plan blocked; nothing removed\n{}",
            plan.errors.join("\n")
        );
        if plan.items.is_empty() {
            return Ok(());
        }
        if !args.yes {
            if args.quiet {
                println!(
                    "worktree-prune: {} candidate(s); run without --quiet to review",
                    plan.items.len()
                );
            } else {
                println!("Dry run: nothing removed. Pass --yes to apply.");
            }
            return Ok(());
        }
        let mut freed = 0;
        for item in &plan.items {
            let e = &item.entry;
            match e.kind {
                Kind::Worktree => {
                    let mut git = self.git();
                    git.args(["worktree", "remove"]);
                    if args.force {
                        git.arg("--force");
                    }
                    let result = output(git.arg("--").arg(&e.path));
                    if let Err(err) = result {
                        if self.registered()?.iter().any(|wt| wt.path == e.path) {
                            return Err(err).context("Git refused removal; target kept");
                        }
                        remove(&e.path, args.sudo)?;
                    }
                }
                Kind::Stale => remove(&e.path, args.sudo)?,
                Kind::Orphan => {
                    freed += bytes(&e.path);
                    remove(&e.path, args.sudo)?;
                }
                Kind::Main => bail!("main checkout is protected"),
            }
            if item.target {
                // A concurrently created worktree may have claimed this target.
                let live = self.inventory(false)?;
                ensure!(
                    self.target_safe(e, &live),
                    "target became shared or protected; kept {}",
                    e.target.display()
                );
                freed += bytes(&e.target);
                remove(&e.target, args.sudo)?;
                if let Some(parent) = e.target.parent() {
                    let _ = fs::remove_dir(parent);
                }
            }
            if args.delete_branch
                && let Some(branch) = &e.branch
            {
                let result = output(self.git().args([
                    "branch",
                    if args.force { "-D" } else { "-d" },
                    "--",
                    branch,
                ]));
                if let Err(err) = result {
                    if !args.quiet {
                        eprintln!("Branch {branch} kept: {err}");
                    }
                    if args.force {
                        return Err(err);
                    }
                }
            }
            if !args.quiet {
                println!("Removed {}", e.path.display());
            }
        }
        // Do not globally prune unrelated missing or locked registrations.
        println!(
            "worktree-prune: removed {} item(s), freed {} of target data",
            plan.items.len(),
            human(freed)
        );
        Ok(())
    }
}
fn preflight(path: &Path, sudo: bool) -> Result<()> {
    // Conservative ownership check before Git can partially remove a checkout.
    let uid = unsafe { libc::geteuid() };
    let device = fs::symlink_metadata(path)?.dev();
    for entry in WalkDir::new(path)
        .follow_links(false)
        .same_file_system(true)
    {
        let entry = match entry {
            Ok(e) => e,
            Err(_) if sudo => continue,
            Err(e) => return Err(e.into()),
        };
        let meta = fs::symlink_metadata(entry.path())?;
        ensure!(
            meta.dev() == device,
            "nested mount point: {}",
            entry.path().display()
        );
        if !sudo {
            ensure!(
                meta.uid() == uid,
                "foreign-owned path {}; pass --sudo",
                entry.path().display()
            );
            if meta.is_dir() {
                let c = std::ffi::CString::new(entry.path().as_os_str().as_bytes())?;
                ensure!(
                    unsafe { libc::access(c.as_ptr(), libc::W_OK | libc::X_OK) } == 0,
                    "unwritable directory {}; pass --sudo",
                    entry.path().display()
                );
            }
        }
    }
    Ok(())
}
fn remove(path: &Path, sudo: bool) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    preflight(path, sudo)?;
    if sudo && preflight(path, false).is_err() {
        output(
            Command::new("sudo")
                .args(["rm", "-rf", "--one-file-system", "--"])
                .arg(path),
        )?;
    } else {
        fs::remove_dir_all(path).with_context(|| format!("cannot remove {}", path.display()))?;
    }
    Ok(())
}
pub fn bytes(path: &Path) -> u64 {
    if !path.is_dir() {
        return 0;
    }
    Command::new("du")
        .args(["-sb", "--"])
        .arg(path)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.split_whitespace().next()?.parse().ok())
        .unwrap_or(0)
}
pub fn human(bytes: u64) -> String {
    let mut n = bytes as f64;
    let mut i = 0;
    let units = ["B", "KiB", "MiB", "GiB", "TiB"];
    while n >= 1024. && i < units.len() - 1 {
        n /= 1024.;
        i += 1;
    }
    format!("{n:.1} {}", units[i])
}
