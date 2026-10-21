use super::*;
use crate::{command::run, model::Origin};
use std::{fs, path::PathBuf};

fn domain() -> String {
    format!("gui/{}", unsafe { libc::getuid() })
}
pub fn discover(snapshot: &mut Snapshot) {
    let home = directories::BaseDirs::new()
        .map(|d| d.home_dir().to_path_buf())
        .unwrap_or_default();
    let live = run("launchctl", &["list"]).unwrap_or_else(|e| {
        snapshot.warnings.push(format!("launchctl: {e}"));
        String::new()
    });
    let disabled = run("launchctl", &["print-disabled", &domain()]).unwrap_or_else(|e| {
        snapshot.warnings.push(format!("launchd overrides: {e}"));
        String::new()
    });
    for (dir, user) in [
        (home.join("Library/LaunchAgents"), true),
        (PathBuf::from("/Library/LaunchAgents"), false),
        (PathBuf::from("/Library/LaunchDaemons"), false),
        (PathBuf::from("/System/Library/LaunchAgents"), false),
        (PathBuf::from("/System/Library/LaunchDaemons"), false),
    ] {
        if let Ok(files) = fs::read_dir(&dir) {
            for file in files.flatten() {
                let path = file.path();
                if path.extension().is_none_or(|e| e != "plist") {
                    continue;
                }
                match plist::Value::from_file(&path) {
                    Ok(v) => {
                        if let Some(d) = v.as_dictionary() {
                            let label = d
                                .get("Label")
                                .and_then(|v| v.as_string())
                                .unwrap_or_default();
                            if label.is_empty() {
                                continue;
                            }
                            let args: Vec<String> = d
                                .get("ProgramArguments")
                                .and_then(|v| v.as_array())
                                .map(|a| {
                                    a.iter()
                                        .filter_map(|v| v.as_string().map(str::to_owned))
                                        .collect()
                                })
                                .unwrap_or_default();
                            let program = d
                                .get("Program")
                                .and_then(|v| v.as_string())
                                .map(str::to_owned)
                                .or_else(|| args.first().cloned())
                                .unwrap_or_default();
                            let command = if args.is_empty() {
                                program.clone()
                            } else {
                                args.join(" ")
                            };
                            let mut e = Entry::new(
                                format!("launchd:{}", path.display()),
                                label.into(),
                                Origin::Launchd {
                                    path: path.clone(),
                                    label: label.into(),
                                    user,
                                },
                                command,
                            );
                            let overridden = if user {
                                disabled
                                    .lines()
                                    .find(|l| l.contains(&format!("\"{label}\"")))
                                    .and_then(|l| l.split("=>").nth(1))
                                    .map(|s| s.trim().starts_with("true"))
                            } else {
                                None
                            };
                            e.startup = Some(!overridden.unwrap_or_else(|| {
                                d.get("Disabled")
                                    .and_then(|v| v.as_boolean())
                                    .unwrap_or(false)
                            }));
                            e.pid = if user {
                                live.lines().find_map(|l| {
                                    let c: Vec<_> = l.split_whitespace().collect();
                                    if c.len() > 2 && c[2] == label {
                                        c[0].parse().ok()
                                    } else {
                                        None
                                    }
                                })
                            } else {
                                None
                            };
                            e.running = if user { Some(e.pid.is_some()) } else { None };
                            e.location = path.display().to_string();
                            e.detail = format!(
                                "RunAtLoad: {:?} · KeepAlive: {:?} · Calendar: {:?} · Interval: {:?}",
                                d.get("RunAtLoad"),
                                d.get("KeepAlive"),
                                d.get("StartCalendarInterval"),
                                d.get("StartInterval")
                            );
                            snapshot.entries.push(e);
                        }
                    }
                    Err(e) => snapshot.warnings.push(format!("{}: {e}", path.display())),
                }
            }
        }
    }
    snapshot.warnings.push("macOS Login Items / Background Task Management (SMAppService) is managed in System Settings. launchd discovery does not enumerate every modern login item.".into());
    if let Ok(text) = run("crontab", &["-l"]) {
        for (n, line) in text.lines().enumerate() {
            if line.trim().is_empty() || line.trim().starts_with('#') {
                continue;
            }
            let mut e = Entry::new(
                format!("cron:{n}"),
                format!("Cron job {}", n + 1),
                Origin::Cron {
                    location: "User crontab".into(),
                },
                line.into(),
            );
            e.location = "User crontab".into();
            e.detail = "Inspect-only schedule".into();
            snapshot.entries.push(e);
        }
    }
}
pub fn apply(_store: &Store, entry: &Entry, action: Action) -> Result<()> {
    if let Origin::Launchd {
        path,
        label,
        user: true,
    } = &entry.origin
    {
        let target = format!("{}/{label}", domain());
        match action {
            Action::Enable => {
                run("launchctl", &["enable", &target])?;
            }
            Action::Disable => {
                run("launchctl", &["disable", &target])?;
            }
            Action::Start => {
                if run("launchctl", &["print", &target]).is_err() {
                    run(
                        "launchctl",
                        &["bootstrap", &domain(), &path.to_string_lossy()],
                    )?;
                }
                run("launchctl", &["kickstart", &target])?;
            }
            Action::Stop => {
                run("launchctl", &["bootout", &target])?;
            }
        }
        Ok(())
    } else {
        unsupported(&entry.origin)
    }
}
fn service_path(store: &Store) -> Result<PathBuf> {
    Ok(directories::BaseDirs::new()
        .ok_or_else(|| anyhow::anyhow!("No home"))?
        .home_dir()
        .join(format!(
            "Library/LaunchAgents/dev.aservice.supervisor.{}.plist",
            registration_id(store)
        )))
}
pub fn startup_enabled(store: &Store) -> Result<bool> {
    let path = service_path(store)?;
    if !path.exists() {
        return Ok(false);
    }
    let s = run("launchctl", &["print-disabled", &domain()])?;
    Ok(!s.lines().any(|l| {
        l.contains(&format!(
            "\"dev.aservice.supervisor.{}\"",
            registration_id(store)
        )) && l.contains("=> true")
    }))
}
pub fn install_startup(store: &Store, enabled: bool) -> Result<()> {
    let target = format!(
        "{}/dev.aservice.supervisor.{}",
        domain(),
        registration_id(store)
    );
    if !enabled {
        run("launchctl", &["disable", &target])?;
        return Ok(());
    }
    let path = service_path(store)?;
    fs::create_dir_all(path.parent().unwrap())?;
    let exe = std::env::current_exe()?;
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd"><plist version="1.0"><dict><key>Label</key><string>dev.aservice.supervisor.{}</string><key>ProgramArguments</key><array><string>{}</string><string>--data-dir</string><string>{}</string><string>daemon</string><string>--login</string></array><key>RunAtLoad</key><true/><key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict><key>ThrottleInterval</key><integer>30</integer><key>ProcessType</key><string>Background</string><key>StandardOutPath</key><string>/dev/null</string><key>StandardErrorPath</key><string>/dev/null</string></dict></plist>"#,
        registration_id(store),
        xml_escape(&exe.to_string_lossy()),
        xml_escape(&store.root.to_string_lossy())
    );
    fs::write(&path, xml)?;
    run("launchctl", &["enable", &target])?;
    // Enabling the next-login registration does not replace a running supervisor.
    Ok(())
}
