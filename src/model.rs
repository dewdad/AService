use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum RestartPolicy {
    Never,
    #[default]
    OnFailure,
    Always,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ManagedApp {
    pub id: String,
    pub name: String,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub working_dir: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
    pub start_on_login: bool,
    pub run_in_background: bool,
    pub desired_running: bool,
    pub restart: RestartPolicy,
    /// Incremented by an explicit start/restart to clear a crash circuit.
    pub generation: u64,
}

impl Default for ManagedApp {
    fn default() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: String::new(),
            program: PathBuf::new(),
            args: vec![],
            working_dir: None,
            env: BTreeMap::new(),
            start_on_login: false,
            run_in_background: true,
            desired_running: false,
            restart: RestartPolicy::OnFailure,
            generation: 0,
        }
    }
}

impl ManagedApp {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(!self.name.trim().is_empty(), "Give the app a name");
        anyhow::ensure!(
            self.program.is_absolute() && self.program.is_file(),
            "Choose an existing absolute executable path"
        );
        if let Some(dir) = &self.working_dir {
            anyhow::ensure!(
                dir.is_absolute() && dir.is_dir(),
                "Working directory must be an existing absolute directory"
            );
        }
        #[cfg(windows)]
        if let Some(ext) = self.program.extension().and_then(|e| e.to_str()) {
            anyhow::ensure!(
                !["cmd", "bat", "ps1", "vbs", "lnk"].contains(&ext.to_ascii_lowercase().as_str()),
                "Choose the runtime executable (node.exe, bun.exe, powershell.exe, etc.) and pass the script as an argument. Shell wrappers cannot be supervised directly."
            );
        }
        anyhow::ensure!(
            self.env
                .iter()
                .all(|(k, v)| !k.is_empty() && !k.contains(['=', '\0']) && !v.contains('\0')),
            "Invalid environment variable"
        );
        anyhow::ensure!(
            self.args.iter().all(|a| !a.contains('\0')),
            "Arguments cannot contain NUL"
        );
        Ok(())
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub schema: u32,
    pub apps: Vec<ManagedApp>,
    pub supervisor_startup: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind")]
pub enum Origin {
    Managed,
    Installed {
        manager: String,
    },
    Process,
    BackgroundConfig {
        manager: String,
        path: PathBuf,
    },
    WindowsRun {
        key: String,
        value: String,
        approved: String,
    },
    StartupFile {
        path: PathBuf,
    },
    WindowsTask {
        path: String,
    },
    WindowsService {
        name: String,
    },
    Systemd {
        unit: String,
        user: bool,
    },
    Desktop {
        path: PathBuf,
        user: bool,
    },
    Launchd {
        path: PathBuf,
        label: String,
        user: bool,
    },
    Cron {
        location: String,
    },
}

impl Origin {
    pub fn label(&self) -> &str {
        match self {
            Self::Managed => "AService",
            Self::Installed { manager } => manager,
            Self::Process => "Running process",
            Self::BackgroundConfig { manager, .. } => manager,
            Self::WindowsRun { .. } => "Registry startup",
            Self::StartupFile { .. } => "Startup folder",
            Self::WindowsTask { .. } => "Task Scheduler",
            Self::WindowsService { .. } => "Windows service",
            Self::Systemd { user: true, .. } => "systemd · user",
            Self::Systemd { .. } => "systemd · system",
            Self::Desktop { .. } => "XDG autostart",
            Self::Launchd { .. } => "launchd",
            Self::Cron { .. } => "cron",
        }
    }
    pub fn can_toggle(&self) -> bool {
        match self {
            Self::WindowsRun { key, .. } => key.starts_with("HKCU:") && !key.ends_with("RunOnce"),
            Self::StartupFile { path } => crate::platform::user_startup_path(path),
            Self::Desktop { user, .. } | Self::Launchd { user, .. } => *user,
            Self::WindowsTask { .. } | Self::WindowsService { .. } | Self::Systemd { .. } => true,
            _ => false,
        }
    }
    pub fn can_run(&self) -> bool {
        matches!(
            self,
            Self::WindowsTask { .. }
                | Self::WindowsService { .. }
                | Self::Systemd { .. }
                | Self::Launchd { user: true, .. }
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub runtime: String,
    pub origin: Origin,
    pub command: String,
    pub program: Option<PathBuf>,
    pub args: Vec<String>,
    pub location: String,
    pub startup: Option<bool>,
    pub running: Option<bool>,
    pub pid: Option<u32>,
    pub detail: String,
}

impl Entry {
    pub fn new(id: String, name: String, origin: Origin, command: String) -> Self {
        let runtime = runtime_for(&command);
        Self {
            id,
            name,
            origin,
            command,
            runtime,
            program: None,
            args: vec![],
            location: String::new(),
            startup: None,
            running: None,
            pid: None,
            detail: String::new(),
        }
    }
}

pub fn runtime_for(command: &str) -> String {
    let s = command.to_lowercase();
    for (markers, label) in [
        (&["bun", ".bun/"][..], "Bun"),
        (
            &["node", "npm", "pnpm", "yarn", ".js", ".mjs", ".cjs"][..],
            "Node.js",
        ),
        (&["cargo", ".cargo", "rustup"][..], "Rust"),
        (&["python", "pipx", ".py", "uv tool"][..], "Python"),
        (&["gobin", "/go/bin", "\\go\\bin", "golang"][..], "Go"),
        (&["dotnet", ".dll"][..], ".NET"),
        (&["ruby", "gem", ".rb"][..], "Ruby"),
        (&["java", ".jar"][..], "Java"),
        (&["deno"][..], "Deno"),
    ] {
        if markers.iter().any(|m| s.contains(m)) {
            return label.into();
        }
    }
    "Native / other".into()
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub entries: Vec<Entry>,
    pub warnings: Vec<String>,
    pub scanned_at: u64,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppStatus {
    pub state: String,
    pub pid: Option<u32>,
    pub restarts: u32,
    pub next_retry: Option<u64>,
    pub message: String,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct SupervisorStatus {
    pub pid: u32,
    pub updated_at: u64,
    pub apps: BTreeMap<String, AppStatus>,
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
