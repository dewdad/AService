use crate::model::{Config, SupervisorStatus};
use anyhow::{Context, Result};
use fs2::FileExt;
use serde::{Serialize, de::DeserializeOwned};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone)]
pub struct Store {
    pub root: PathBuf,
}

impl Store {
    pub fn open(root: Option<PathBuf>) -> Result<Self> {
        let root = root.unwrap_or_else(|| {
            directories::ProjectDirs::from("dev", "aservice", "AService")
                .expect("No user data directory")
                .data_local_dir()
                .into()
        });
        fs::create_dir_all(&root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self {
            root: command_path(fs::canonicalize(root)?),
        })
    }
    pub fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }
    pub fn load(&self) -> Result<Config> {
        let config: Config = self.read("config.json")?;
        anyhow::ensure!(
            config.schema <= 1,
            "Configuration was written by a newer AService; refusing to overwrite it"
        );
        let mut ids = std::collections::HashSet::new();
        anyhow::ensure!(
            config
                .apps
                .iter()
                .all(|a| !a.id.is_empty() && safe_id(&a.id) == a.id && ids.insert(a.id.clone())),
            "Configuration contains an invalid or duplicate app id"
        );
        Ok(config)
    }
    pub fn read<T: DeserializeOwned + Default>(&self, name: &str) -> Result<T> {
        match fs::read(self.path(name)) {
            Ok(bytes) => serde_json::from_slice(&bytes).with_context(|| {
                format!(
                    "Invalid {}. Original file preserved.",
                    self.path(name).display()
                )
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
            Err(e) => Err(e.into()),
        }
    }
    pub fn write<T: Serialize>(&self, name: &str, value: &T) -> Result<()> {
        atomic_json(&self.path(name), value)
    }
    pub fn update(&self, f: impl FnOnce(&mut Config) -> Result<()>) -> Result<()> {
        let lock = self.lock("config.lock", false)?;
        let mut config = self.load()?;
        f(&mut config)?;
        config.schema = 1;
        self.write("config.json", &config)?;
        drop(lock);
        Ok(())
    }
    pub fn lock(&self, name: &str, try_only: bool) -> Result<File> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.path(name))?;
        if try_only {
            FileExt::try_lock_exclusive(&file)?;
        } else {
            FileExt::lock_exclusive(&file)?;
        }
        Ok(file)
    }
    pub fn is_locked(&self, name: &str) -> bool {
        self.lock(name, true)
            .err()
            .is_some_and(|e| lock_contended(&e))
    }
    pub fn status(&self) -> Result<SupervisorStatus> {
        self.read("status.json")
    }
    pub fn log_path(&self, id: &str) -> PathBuf {
        self.path("logs").join(format!("{}.log", safe_id(id)))
    }
}

pub fn safe_id(id: &str) -> String {
    id.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(80)
        .collect()
}

/// Rust canonicalization uses Windows verbatim prefixes; many language runtimes
/// and login managers reject those prefixes in script arguments.
pub fn command_path(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        let s = path.to_string_lossy();
        if let Some(unc) = s.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{unc}"));
        }
        if let Some(local) = s.strip_prefix(r"\\?\") {
            return PathBuf::from(local);
        }
    }
    path
}

pub fn atomic_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = path.parent().context("Missing parent directory")?;
    fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut temp, value)?;
    temp.write_all(b"\n")?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|e| e.error)?;
    #[cfg(unix)]
    File::open(parent)?.sync_all()?;
    Ok(())
}

pub fn lock_contended(error: &anyhow::Error) -> bool {
    error.downcast_ref::<std::io::Error>().is_some_and(|e| {
        e.kind() == std::io::ErrorKind::WouldBlock
            || e.raw_os_error() == fs2::lock_contended_error().raw_os_error()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn corrupt_config_is_not_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(Some(dir.path().into())).unwrap();
        fs::write(store.path("config.json"), b"broken").unwrap();
        assert!(store.update(|_| Ok(())).is_err());
        assert_eq!(fs::read(store.path("config.json")).unwrap(), b"broken");
    }
    #[test]
    fn atomic_replace_and_exclusive_lock() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(Some(dir.path().into())).unwrap();
        let lock = store.lock("daemon.lock", true).unwrap();
        assert!(store.is_locked("daemon.lock"));
        drop(lock);
        store
            .update(|c| {
                c.supervisor_startup = true;
                Ok(())
            })
            .unwrap();
        store
            .update(|c| {
                c.supervisor_startup = false;
                Ok(())
            })
            .unwrap();
        assert!(!store.load().unwrap().supervisor_startup);
    }
}
