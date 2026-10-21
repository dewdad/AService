use super::*;

pub(super) fn background_configs(snapshot: &mut Snapshot) {
    let pm2 = std::env::var_os("PM2_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".pm2"));
    let dump = pm2.join("dump.pm2");
    if dump.is_file() {
        match fs::read(&dump)
            .map_err(anyhow::Error::from)
            .and_then(|b| Ok(serde_json::from_slice::<Value>(&b)?))
        {
            Ok(v) => {
                if let Some(apps) = v.as_array() {
                    for (i, app) in apps.iter().enumerate() {
                        let name = app["name"].as_str().unwrap_or("PM2 app");
                        let script = app["pm_exec_path"].as_str().unwrap_or_default();
                        let interpreter = app["exec_interpreter"].as_str().unwrap_or("node");
                        let args: Vec<String> = app["args"]
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .filter_map(|v| v.as_str().map(str::to_owned))
                                    .collect()
                            })
                            .unwrap_or_default();
                        let mut entry = Entry::new(
                            format!("pm2:{}:{i}", dump.display()),
                            name.into(),
                            Origin::BackgroundConfig {
                                manager: "PM2 saved apps".into(),
                                path: dump.clone(),
                            },
                            format!("{interpreter} {script} {}", args.join(" ")),
                        );
                        if interpreter == "none" {
                            entry.program = Some(script.into());
                            entry.args = args;
                        } else {
                            entry.program = if Path::new(interpreter).is_absolute() {
                                Some(interpreter.into())
                            } else {
                                find_executable(interpreter)
                            };
                            entry.args = std::iter::once(script.to_owned()).chain(args).collect();
                        }
                        entry.location = dump.display().to_string();
                        entry.detail = format!(
                            "Persisted PM2 definition, not live daemon state. Autorestart: {} · cron_restart: {} · instances: {} · cwd: {}. A saved dump alone does not prove startup registration. Creating a managed copy does not stop PM2.",
                            app["autorestart"],
                            app["cron_restart"],
                            app["instances"],
                            app["pm_cwd"]
                        );
                        snapshot.entries.push(entry);
                    }
                }
            }
            Err(e) => snapshot.warnings.push(format!("PM2 saved apps: {e}")),
        }
    }
    for root in [
        PathBuf::from("/etc/supervisor/conf.d"),
        PathBuf::from("/etc/supervisord.d"),
    ] {
        if let Ok(files) = fs::read_dir(root) {
            for file in files.flatten() {
                let path = file.path();
                let Ok(text) = fs::read_to_string(&path) else {
                    continue;
                };
                let mut section = String::new();
                let mut command = String::new();
                let mut policies = vec![];
                let finish =
                    |section: &str, command: &str, policies: &[String], snapshot: &mut Snapshot| {
                        if section.is_empty() {
                            return;
                        }
                        let mut entry = Entry::new(
                            format!("supervisord:{}:{section}", path.display()),
                            section.into(),
                            Origin::BackgroundConfig {
                                manager: "supervisord config".into(),
                                path: path.clone(),
                            },
                            command.into(),
                        );
                        entry.location = path.display().to_string();
                        entry.detail = format!(
                            "Configuration only; live state unknown. {}",
                            policies.join(" · ")
                        );
                        snapshot.entries.push(entry);
                    };
                for line in text.lines() {
                    let line = line.trim();
                    if line.starts_with('[') {
                        finish(&section, &command, &policies, snapshot);
                        section = line
                            .strip_prefix("[program:")
                            .and_then(|s| s.strip_suffix(']'))
                            .unwrap_or_default()
                            .into();
                        command.clear();
                        policies.clear();
                    } else if let Some((key, value)) = line.split_once('=') {
                        match key.trim() {
                            "command" => command = value.trim().into(),
                            "autostart" | "autorestart" | "startretries" => {
                                policies.push(line.into())
                            }
                            _ => {}
                        }
                    }
                }
                finish(&section, &command, &policies, snapshot);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn saved_configuration_is_not_reported_as_live() {
        let entry = Entry::new(
            "saved".into(),
            "saved app".into(),
            Origin::BackgroundConfig {
                manager: "PM2 saved apps".into(),
                path: "dump.pm2".into(),
            },
            "node app.js".into(),
        );
        assert!(entry.running.is_none());
        assert!(entry.startup.is_none());
        assert!(!entry.origin.can_toggle());
    }
}
