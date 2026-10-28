use anyhow::{Context, Result, ensure};
use std::{env, ffi::OsString, fs, io::ErrorKind, path::PathBuf};

fn config_path(xdg: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    xdg.map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            home.map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .map(|p| p.join(".config"))
        })
        .map(|p| p.join("worktree-prune/config.toml"))
}

fn read_threads(path: Option<PathBuf>) -> Result<Option<usize>> {
    let Some(path) = path else { return Ok(None) };
    let source = match fs::read_to_string(&path) {
        Ok(source) => source,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("cannot read {}", path.display())),
    };
    (|| -> Result<_> {
        let table: toml::Table = toml::from_str(&source)?;
        for key in table.keys() {
            ensure!(key == "threads", "unknown configuration key: {key}");
        }
        table
            .get("threads")
            .map(|value| {
                let number = value
                    .as_integer()
                    .context("threads must be a positive integer")?;
                ensure!(number > 0, "threads must be a positive integer");
                usize::try_from(number).context("threads is too large")
            })
            .transpose()
    })()
    .with_context(|| format!("invalid configuration {}", path.display()))
}

fn resolve_threads(configured: Option<usize>, override_value: Option<OsString>) -> Result<usize> {
    if let Some(value) = override_value {
        let number = value
            .to_str()
            .context("WTP_THREADS must be a positive integer")?
            .parse::<usize>()
            .context("WTP_THREADS must be a positive integer")?;
        ensure!(number > 0, "WTP_THREADS must be a positive integer");
        return Ok(number);
    }
    Ok(configured.unwrap_or_else(|| std::thread::available_parallelism().map_or(1, usize::from)))
}

pub fn init() -> Result<()> {
    let path = config_path(env::var_os("XDG_CONFIG_HOME"), env::var_os("HOME"));
    let configured = read_threads(path)?;
    // The application owns its thread-count environment variable.
    let threads = resolve_threads(configured, env::var_os("WTP_THREADS"))?;
    // An explicit positive count prevents Rayon from reading RAYON_NUM_THREADS.
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build_global()
        .context("cannot initialize the worktree-prune thread pool")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xdg_paths_follow_linux_conventions() {
        let home = Some(OsString::from("/home/test"));
        assert_eq!(
            config_path(Some("/custom".into()), home.clone()),
            Some("/custom/worktree-prune/config.toml".into())
        );
        for xdg in [None, Some("".into()), Some("relative".into())] {
            assert_eq!(
                config_path(xdg, home.clone()),
                Some("/home/test/.config/worktree-prune/config.toml".into())
            );
        }
        assert_eq!(config_path(None, None), None);
    }

    #[test]
    fn file_is_optional_but_malformed_values_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        assert_eq!(read_threads(Some(path.clone())).unwrap(), None);
        for (source, expected) in [("", None), ("threads = 4", Some(4))] {
            fs::write(&path, source).unwrap();
            assert_eq!(read_threads(Some(path.clone())).unwrap(), expected);
        }
        for source in [
            "threads = 0",
            "threads = -1",
            "threads = 1.5",
            "threads = '4'",
            "thread = 4",
            "not toml",
        ] {
            fs::write(&path, source).unwrap();
            assert!(read_threads(Some(path.clone())).is_err(), "{source}");
        }
    }

    #[test]
    fn environment_overrides_file_and_configures_rayon() {
        assert_eq!(resolve_threads(Some(4), Some("2".into())).unwrap(), 2);
        assert_eq!(resolve_threads(Some(4), None).unwrap(), 4);
        assert!(resolve_threads(None, None).unwrap() > 0);
        for value in ["0", "-1", "", "invalid"] {
            assert!(resolve_threads(Some(4), Some(value.into())).is_err());
        }
        let count = resolve_threads(Some(3), None).unwrap();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(count)
            .build()
            .unwrap();
        assert_eq!(pool.install(rayon::current_num_threads), 3);
    }
}
