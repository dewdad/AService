#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
use anyhow::{Context, Result};
use aservice::{discovery, gui, store::Store, supervisor};
use clap::{Parser, Subcommand};
use std::{io::Write, path::PathBuf};

#[derive(Parser)]
#[command(version, about = "AService — local background application management")]
struct Cli {
    /// Isolate configuration, status, logs, and locks in this directory.
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Mode>,
}
#[derive(Subcommand)]
enum Mode {
    /// Inspect installed tools, startup sources and running processes. No mutations.
    Scan {
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Run the headless process supervisor (normally launched by the GUI/OS).
    Daemon {
        #[arg(long)]
        login: bool,
    },
    /// Export current configuration and supervisor status.
    Status {
        #[arg(long)]
        output: Option<PathBuf>,
    },
}
fn main() {
    if let Err(e) = execute() {
        eprintln!("{e:#}");
        // GUI-subsystem Windows executables may have no stderr. Persist a small
        // diagnostic without opening a terminal or modal during OS startup.
        if let Some(d) = directories::ProjectDirs::from("dev", "aservice", "AService") {
            let _ = std::fs::create_dir_all(d.data_local_dir());
            let _ = std::fs::write(
                d.data_local_dir().join("last-error.txt"),
                format!("{e:#}\n"),
            );
        }
        std::process::exit(1);
    }
}
fn execute() -> Result<()> {
    let cli = Cli::parse();
    let store = Store::open(cli.data_dir)?;
    let diagnostics = store.clone();
    let result = match cli.command {
        None => gui::run(store),
        Some(Mode::Daemon { login }) => supervisor::run(store, login),
        Some(Mode::Scan { output }) => emit(&discovery::scan(&store), output),
        Some(Mode::Status { output }) => emit(
            &serde_json::json!({"config":store.load()?,"supervisor":store.status()?,"supervisor_lock_held":store.is_locked("daemon.lock")}),
            output,
        ),
    };
    if let Err(e) = &result {
        let _ = std::fs::write(diagnostics.path("last-error.txt"), format!("{e:#}\n"));
    }
    result
}
fn emit(value: &impl serde::Serialize, output: Option<PathBuf>) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    if let Some(path) = output {
        std::fs::write(&path, &bytes)
            .with_context(|| format!("Cannot write {}", path.display()))?;
    } else {
        std::io::stdout().write_all(&bytes)?;
    }
    Ok(())
}
