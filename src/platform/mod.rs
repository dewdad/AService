use crate::{
    model::{Entry, Origin, Snapshot},
    store::Store,
};
use anyhow::{Result, bail};
use std::path::Path;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "linux")]
use linux as native;
#[cfg(target_os = "macos")]
use macos as native;
#[cfg(target_os = "windows")]
use windows as native;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Action {
    Enable,
    Disable,
    Start,
    Stop,
}

impl Action {
    pub fn label(self) -> &'static str {
        match self {
            Self::Enable => "Enable",
            Self::Disable => "Disable",
            Self::Start => "Start",
            Self::Stop => "Stop",
        }
    }
}

pub fn discover(snapshot: &mut Snapshot) {
    native::discover(snapshot);
}

pub fn apply(store: &Store, entry: &Entry, action: Action) -> Result<()> {
    anyhow::ensure!(
        if matches!(action, Action::Enable | Action::Disable) {
            entry.origin.can_toggle()
        } else {
            entry.origin.can_run()
        },
        "This source is available for inspection only"
    );
    // Save the source identity and displayed state before any native mutation.
    // Native adapters additionally save original task/registry/service definitions.
    let dir = store.path("backups");
    std::fs::create_dir_all(&dir)?;
    crate::store::atomic_json(
        &dir.join(format!(
            "{}-{}.json",
            crate::model::now(),
            uuid::Uuid::new_v4()
        )),
        entry,
    )?;
    native::apply(store, entry, action)
}

pub fn install_startup(store: &Store, enabled: bool) -> Result<()> {
    native::install_startup(store, enabled)?;
    store.update(|c| {
        c.supervisor_startup = enabled;
        Ok(())
    })
}

pub fn startup_enabled(store: &Store) -> Result<bool> {
    native::startup_enabled(store)
}

pub fn user_startup_path(path: &Path) -> bool {
    #[cfg(windows)]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return path.starts_with(
                Path::new(&appdata).join("Microsoft/Windows/Start Menu/Programs/Startup"),
            );
        }
    }
    let _ = path;
    false
}

pub fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub fn unsupported(origin: &Origin) -> Result<()> {
    bail!("{} is inspect-only", origin.label())
}

pub fn registration_id(store: &Store) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in store.root.to_string_lossy().bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}
