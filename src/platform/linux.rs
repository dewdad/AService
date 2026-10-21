use super::*;
use crate::{command::run, model::Origin};
use std::fs;

pub fn discover(snapshot: &mut Snapshot) {
    for user in [true, false] {
        let mut args = vec![
            "list-unit-files",
            "--type=service",
            "--type=timer",
            "--no-legend",
            "--no-pager",
        ];
        if user {
            args.push("--user");
        }
        match run("systemctl", &args) {
            Ok(text) => {
                let mut live = vec![
                    "list-units",
                    "--all",
                    "--type=service",
                    "--type=timer",
                    "--no-legend",
                    "--no-pager",
                    "--plain",
                ];
                if user {
                    live.push("--user");
                }
                let live = match run("systemctl", &live) {
                    Ok(s) => s,
                    Err(e) => {
                        snapshot.warnings.push(format!("systemd live state: {e}"));
                        String::new()
                    }
                };
                for line in text.lines() {
                    let cols: Vec<_> = line.split_whitespace().collect();
                    if cols.len() < 2 {
                        continue;
                    }
                    let unit = cols[0];
                    let state = cols[1];
                    let mut e = Entry::new(
                        format!("systemd:{user}:{unit}"),
                        unit.into(),
                        Origin::Systemd {
                            unit: unit.into(),
                            user,
                        },
                        unit.into(),
                    );
                    e.startup = match state {
                        "enabled" | "enabled-runtime" => Some(true),
                        "disabled" | "masked" | "masked-runtime" => Some(false),
                        _ => None,
                    };
                    e.running = live.lines().find_map(|l| {
                        let c: Vec<_> = l.split_whitespace().collect();
                        if c.len() > 3 && c[0] == unit {
                            Some(c[2] == "active")
                        } else {
                            None
                        }
                    });
                    e.detail = format!(
                        "Unit state: {state}. Native systemd owns restart and schedule policies."
                    );
                    e.location = if user {
                        "User service manager"
                    } else {
                        "System service manager (permissions may be required)"
                    }
                    .into();
                    snapshot.entries.push(e);
                }
            }
            Err(e) => snapshot
                .warnings
                .push(format!("systemd (user={user}): {e}")),
        }
    }
    let home = directories::BaseDirs::new()
        .map(|d| d.home_dir().to_path_buf())
        .unwrap_or_default();
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    for (dir, user) in [
        (config.join("autostart"), true),
        ("/etc/xdg/autostart".into(), false),
    ] {
        if let Ok(files) = fs::read_dir(&dir) {
            for file in files.flatten() {
                let path = file.path();
                if path.extension().is_none_or(|e| e != "desktop") {
                    continue;
                }
                if let Ok(text) = fs::read_to_string(&path) {
                    let get = |k: &str| {
                        text.lines()
                            .find_map(|l| l.strip_prefix(&format!("{k}=")))
                            .unwrap_or_default()
                    };
                    let name = get("Name");
                    let name = if name.is_empty() {
                        file.file_name().to_string_lossy().into_owned()
                    } else {
                        name.into()
                    };
                    let mut e = Entry::new(
                        format!("desktop:{}", path.display()),
                        name,
                        Origin::Desktop {
                            path: path.clone(),
                            user,
                        },
                        get("Exec").into(),
                    );
                    let mask = config.join("autostart").join(file.file_name());
                    e.startup = Some(
                        get("Hidden") != "true"
                            && get("X-GNOME-Autostart-enabled") != "false"
                            && (user || !mask.exists()),
                    );
                    e.location = path.display().to_string();
                    e.detail = format!(
                        "OnlyShowIn: {} · NotShowIn: {} · TryExec: {}",
                        get("OnlyShowIn"),
                        get("NotShowIn"),
                        get("TryExec")
                    );
                    snapshot.entries.push(e);
                }
            }
        }
    }
    match run("crontab", &["-l"]) {
        Ok(s) => cron(snapshot, &s, "User crontab"),
        Err(e) => snapshot
            .warnings
            .push(format!("User crontab unavailable or empty: {e}")),
    }
    if let Ok(s) = fs::read_to_string("/etc/crontab") {
        cron(snapshot, &s, "/etc/crontab");
    }
    if let Ok(files) = fs::read_dir("/etc/cron.d") {
        for file in files.flatten() {
            if let Ok(s) = fs::read_to_string(file.path()) {
                cron(snapshot, &s, &file.path().display().to_string());
            }
        }
    }
    for folder in [
        "/etc/cron.hourly",
        "/etc/cron.daily",
        "/etc/cron.weekly",
        "/etc/cron.monthly",
    ] {
        if let Ok(files) = fs::read_dir(folder) {
            for file in files.flatten() {
                let mut e = Entry::new(
                    format!("cron:{}", file.path().display()),
                    file.file_name().to_string_lossy().into_owned(),
                    Origin::Cron {
                        location: folder.into(),
                    },
                    file.path().display().to_string(),
                );
                e.location = folder.into();
                e.detail = "Periodic script; inspect-only".into();
                snapshot.entries.push(e);
            }
        }
    }
}

fn cron(snapshot: &mut Snapshot, text: &str, location: &str) {
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || (!line.contains(' ') && line.contains('=')) {
            continue;
        }
        if line
            .split_whitespace()
            .next()
            .is_some_and(|s| s.contains('='))
        {
            continue;
        }
        let mut e = Entry::new(
            format!("cron:{location}:{n}"),
            format!("Cron job {}", n + 1),
            Origin::Cron {
                location: location.into(),
            },
            line.into(),
        );
        e.location = location.into();
        e.startup = if line.starts_with("@reboot") {
            Some(true)
        } else {
            None
        };
        e.detail =
            "Existing cron schedule; inspect-only to preserve shared crontab formatting".into();
        snapshot.entries.push(e);
    }
}

pub fn apply(store: &Store, entry: &Entry, action: Action) -> Result<()> {
    match &entry.origin {
        Origin::Systemd { unit, user } => {
            anyhow::ensure!(
                !unit.starts_with('-') && !unit.contains('/'),
                "Invalid unit name"
            );
            let verb = match action {
                Action::Enable => "enable",
                Action::Disable => "disable",
                Action::Start => "start",
                Action::Stop => "stop",
            };
            let mut args = vec![verb, "--no-ask-password", unit];
            if *user {
                args.push("--user");
            }
            run("systemctl", &args)?;
        }
        Origin::Desktop { path, user: true } => {
            let text = fs::read_to_string(path)?;
            fs::write(
                store
                    .path("backups")
                    .join(format!("{}.desktop", uuid::Uuid::new_v4())),
                &text,
            )?;
            let mut output = String::new();
            let mut in_main = false;
            let mut found = false;
            for line in text.lines() {
                if line.starts_with('[') {
                    if in_main && !found {
                        output.push_str(&format!("Hidden={}\n", action == Action::Disable));
                        found = true;
                    }
                    in_main = line.trim() == "[Desktop Entry]";
                }
                if in_main && line.starts_with("Hidden=") {
                    output.push_str(&format!("Hidden={}\n", action == Action::Disable));
                    found = true;
                } else if in_main && line.starts_with("X-GNOME-Autostart-enabled=") {
                    output.push_str(&format!(
                        "X-GNOME-Autostart-enabled={}\n",
                        action == Action::Enable
                    ));
                } else {
                    output.push_str(line);
                    output.push('\n');
                }
            }
            if in_main && !found {
                output.push_str(&format!("Hidden={}\n", action == Action::Disable));
            }
            let mut temp = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
            use std::io::Write;
            temp.write_all(output.as_bytes())?;
            temp.as_file()
                .set_permissions(fs::metadata(path)?.permissions())?;
            temp.persist(path).map_err(|e| e.error)?;
        }
        _ => return unsupported(&entry.origin),
    }
    Ok(())
}

fn service_path(store: &Store) -> Result<std::path::PathBuf> {
    let d = directories::BaseDirs::new().ok_or_else(|| anyhow::anyhow!("No home directory"))?;
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| d.config_dir().into());
    Ok(config.join(format!(
        "systemd/user/aservice-supervisor-{}.service",
        registration_id(store)
    )))
}
fn quote(s: &str) -> String {
    format!(
        "\"{}\"",
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
            .replace('$', "$$")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
    )
}
pub fn startup_enabled(store: &Store) -> Result<bool> {
    let unit = format!("aservice-supervisor-{}.service", registration_id(store));
    let text = run(
        "systemctl",
        &[
            "--user",
            "list-unit-files",
            &unit,
            "--no-legend",
            "--no-pager",
        ],
    )?;
    Ok(text
        .lines()
        .any(|l| l.split_whitespace().nth(1) == Some("enabled")))
}
pub fn install_startup(store: &Store, enabled: bool) -> Result<()> {
    let path = service_path(store)?;
    let unit = format!("aservice-supervisor-{}.service", registration_id(store));
    if !enabled {
        run("systemctl", &["--user", "disable", &unit])?;
        return Ok(());
    }
    let exe = std::env::current_exe()?;
    fs::create_dir_all(path.parent().unwrap())?;
    let content = format!(
        "[Unit]\nDescription=AService background supervisor\nStartLimitIntervalSec=300\nStartLimitBurst=5\n[Service]\nType=simple\nExecStart={} --data-dir {} daemon --login\nRestart=on-failure\nRestartSec=5\nKillMode=control-group\nTimeoutStopSec=15\n[Install]\nWantedBy=default.target\n",
        quote(&exe.to_string_lossy()),
        quote(&store.root.to_string_lossy())
    );
    fs::write(&path, content)?;
    run("systemctl", &["--user", "daemon-reload"])?;
    run("systemctl", &["--user", "enable", &unit])?;
    Ok(())
}
