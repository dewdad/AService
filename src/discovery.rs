//! Best-effort inventory. Package metadata and process names are evidence, not a
//! universal installed-program database. Never execute package entry points.
use crate::{
    model::{Entry, Origin, Snapshot, now},
    platform,
    store::Store,
};
use serde_json::Value;
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};
mod background;
use background::background_configs;

pub fn scan(store: &Store) -> Snapshot {
    let mut snapshot = Snapshot {
        scanned_at: now(),
        ..Default::default()
    };
    inventory(&mut snapshot);
    background_configs(&mut snapshot);
    platform::discover(&mut snapshot);
    let system = sysinfo::System::new_all();
    for (pid, process) in system.processes() {
        let executable = process.exe().map(Path::to_path_buf);
        let argv: Vec<String> = process
            .cmd()
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        let command = if argv.is_empty() {
            process.name().to_string_lossy().into_owned()
        } else {
            argv.join(" ")
        };
        let mut e = Entry::new(
            format!("process:{}", pid.as_u32()),
            process.name().to_string_lossy().into_owned(),
            Origin::Process,
            command,
        );
        e.program = executable;
        e.args = argv.into_iter().skip(1).collect();
        e.pid = Some(pid.as_u32());
        e.running = Some(true);
        e.location = process
            .cwd()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        e.detail="Observed process; background status cannot be inferred from a PID. Inspect-only. Create a managed entry to supervise future launches.".into();
        snapshot.entries.push(e);
    }
    match store.load() {
        Ok(config) => {
            for app in config.apps {
                let mut e = Entry::new(
                    app.id.clone(),
                    app.name.clone(),
                    Origin::Managed,
                    format!("{} {}", app.program.display(), app.args.join(" ")),
                );
                e.program = Some(app.program);
                e.args = app.args;
                e.startup = Some(app.start_on_login);
                e.location = app
                    .working_dir
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                e.detail = format!(
                    "Restart: {:?} · Keep running when window closes: {}",
                    app.restart, app.run_in_background
                );
                snapshot.entries.push(e);
            }
        }
        Err(e) => snapshot
            .warnings
            .push(format!("Managed configuration: {e:#}")),
    }
    snapshot
        .entries
        .sort_by_key(|e| (e.origin.label().to_owned(), e.name.to_lowercase()));
    snapshot.warnings.push("Inventory covers known user/global package locations and the current PATH. Project-local installs, custom prefixes, containers, WSL, and other users may require separate scans. Running processes are observations, not proof of background behavior.".into());
    snapshot
}

pub fn find_executable(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let mut names = vec![name.to_owned()];
    if cfg!(windows) {
        names = vec![format!("{name}.exe"), name.to_owned()];
    }
    for dir in std::env::split_paths(&path) {
        if !dir.is_absolute() {
            continue;
        }
        for name in &names {
            let file = dir.join(name);
            if file.is_file() {
                return Some(crate::store::command_path(
                    fs::canonicalize(&file).unwrap_or(file),
                ));
            }
        }
    }
    None
}

fn home() -> PathBuf {
    directories::BaseDirs::new()
        .map(|d| d.home_dir().to_path_buf())
        .unwrap_or_default()
}

fn inventory(snapshot: &mut Snapshot) {
    let home = home();
    let mut roots = vec![];
    if cfg!(windows) {
        for var in ["APPDATA", "LOCALAPPDATA"] {
            if let Some(dir) = std::env::var_os(var) {
                let dir = PathBuf::from(dir);
                roots.push((dir.join("npm/node_modules"), "npm", "Node.js"));
                roots.push((dir.join("pnpm/global/5/node_modules"), "pnpm", "Node.js"));
                roots.push((dir.join("Yarn/Data/global/node_modules"), "Yarn", "Node.js"));
            }
        }
    } else {
        roots.extend([
            (
                PathBuf::from("/usr/local/lib/node_modules"),
                "npm",
                "Node.js",
            ),
            (PathBuf::from("/usr/lib/node_modules"), "npm", "Node.js"),
            (
                PathBuf::from("/opt/homebrew/lib/node_modules"),
                "npm",
                "Node.js",
            ),
        ]);
    }
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".cargo"));
    let bun_home = std::env::var_os("BUN_INSTALL")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".bun"));
    roots.extend([
        (home.join(".npm-global/lib/node_modules"), "npm", "Node.js"),
        (
            home.join(".local/share/pnpm/global/5/node_modules"),
            "pnpm",
            "Node.js",
        ),
        (
            home.join(".config/yarn/global/node_modules"),
            "Yarn",
            "Node.js",
        ),
        (
            bun_home.join("install/global/node_modules"),
            "Bun global",
            "Bun",
        ),
    ]);
    if let Some(prefix) = std::env::var_os("NPM_CONFIG_PREFIX") {
        let p = PathBuf::from(prefix);
        roots.push((
            if cfg!(windows) {
                p.join("node_modules")
            } else {
                p.join("lib/node_modules")
            },
            "npm",
            "Node.js",
        ));
    }
    // Resolve npm's actual prefix without invoking npm.cmd or a command shell.
    if let Some(node) = find_executable("node") {
        let mut cli = node
            .parent()
            .unwrap_or(Path::new("."))
            .join("node_modules/npm/bin/npm-cli.js");
        if !cli.exists()
            && let Some(prefix) = node.parent().and_then(Path::parent)
        {
            cli = prefix.join("lib/node_modules/npm/bin/npm-cli.js");
        }
        if cli.exists() {
            match crate::command::output(
                std::process::Command::new(node).arg(cli).args([
                    "root",
                    "--global",
                    "--ignore-scripts",
                ]),
                std::time::Duration::from_secs(15),
            ) {
                Ok(root) => roots.push((PathBuf::from(root.trim()), "npm", "Node.js")),
                Err(e) => snapshot.warnings.push(format!("npm prefix: {e}")),
            }
        }
    }
    let mut seen = HashSet::new();
    for (root, manager, runtime) in roots {
        if let Ok(root) = fs::canonicalize(root)
            && seen.insert(root.clone())
        {
            node_packages(snapshot, &root, manager, runtime);
        }
    }
    if let Some(path) = std::env::var_os("NODE_PATH") {
        for root in std::env::split_paths(&path) {
            if let Ok(root) = fs::canonicalize(root)
                && seen.insert(root.clone())
            {
                node_packages(snapshot, &root, "NODE_PATH", "Node.js");
            }
        }
    }
    let cargo_metadata = fs::read(cargo_home.join(".crates2.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    if let Some(installs) = cargo_metadata
        .as_ref()
        .and_then(|v| v["installs"].as_object())
    {
        for (package, record) in installs {
            if let Some(bins) = record["bins"].as_array() {
                for bin in bins.iter().filter_map(Value::as_str) {
                    let path =
                        cargo_home
                            .join("bin")
                            .join(if cfg!(windows) && !bin.ends_with(".exe") {
                                format!("{bin}.exe")
                            } else {
                                bin.into()
                            });
                    if path.is_file() {
                        installed_binary(snapshot, &path, "Cargo", "Rust", package);
                    }
                }
            }
        }
    } else {
        scan_bin(
            snapshot,
            &cargo_home.join("bin"),
            "Cargo bin (metadata unavailable)",
            "Rust",
        );
    }
    let mut go_bins = vec![];
    if let Some(bin) = std::env::var_os("GOBIN") {
        go_bins.push(PathBuf::from(bin));
    }
    if let Some(path) = std::env::var_os("GOPATH") {
        for p in std::env::split_paths(&path) {
            go_bins.push(p.join("bin"));
        }
    }
    if let Some(go) = find_executable("go") {
        match crate::command::output(
            std::process::Command::new(go).args(["env", "-json", "GOBIN", "GOPATH"]),
            std::time::Duration::from_secs(10),
        ) {
            Ok(s) => {
                if let Ok(v) = serde_json::from_str::<Value>(&s) {
                    if let Some(p) = v["GOBIN"].as_str().filter(|s| !s.is_empty()) {
                        go_bins.push(p.into());
                    }
                    if let Some(p) = v["GOPATH"].as_str() {
                        for p in std::env::split_paths(p) {
                            go_bins.push(p.join("bin"));
                        }
                    }
                }
            }
            Err(e) => snapshot.warnings.push(format!("Go paths: {e}")),
        }
    }
    go_bins.push(home.join("go/bin"));
    let mut seen = HashSet::new();
    for dir in go_bins {
        if let Ok(dir) = fs::canonicalize(dir)
            && seen.insert(dir.clone())
        {
            scan_bin(snapshot, &dir, "Go bin", "Go");
        }
    }
    // Python isolated application environments: no arbitrary site-packages walk.
    for dir in [
        home.join(".local/share/pipx/venvs"),
        home.join(".local/pipx/venvs"),
        home.join(".local/share/uv/tools"),
    ] {
        if let Ok(venvs) = fs::read_dir(dir) {
            for env in venvs.flatten() {
                let path = env
                    .path()
                    .join(if cfg!(windows) { "Scripts" } else { "bin" });
                scan_bin(snapshot, &path, "Python tools", "Python");
            }
        }
    }
    let runtimes = [
        ("node", "Node.js"),
        ("bun", "Bun"),
        ("go", "Go"),
        ("cargo", "Rust"),
        ("python", "Python"),
        ("python3", "Python"),
        ("deno", "Deno"),
        ("dotnet", ".NET"),
    ];
    for (bin, runtime) in runtimes {
        if let Some(path) = find_executable(bin) {
            let mut e = Entry::new(
                format!("runtime:{}", path.display()),
                format!("{runtime} runtime"),
                Origin::Installed {
                    manager: "Runtime".into(),
                },
                path.display().to_string(),
            );
            e.runtime = runtime.into();
            e.program = Some(path.clone());
            e.location = path.display().to_string();
            e.detail="Runtime executable found on PATH. Add a script/module argument when creating a managed app.".into();
            snapshot.entries.push(e);
        }
    }
}

fn node_packages(snapshot: &mut Snapshot, root: &Path, manager: &str, runtime: &str) {
    let Ok(items) = fs::read_dir(root) else {
        return;
    };
    for item in items.flatten() {
        let name = item.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') {
            continue;
        }
        if name.starts_with('@') {
            if let Ok(scoped) = fs::read_dir(item.path()) {
                for sub in scoped.flatten() {
                    node_package(snapshot, &sub.path(), manager, runtime);
                }
            }
        } else {
            node_package(snapshot, &item.path(), manager, runtime);
        }
    }
}
fn node_package(snapshot: &mut Snapshot, dir: &Path, manager: &str, runtime: &str) {
    let Ok(bytes) = fs::read(dir.join("package.json")) else {
        return;
    };
    let Ok(v) = serde_json::from_slice::<Value>(&bytes) else {
        return;
    };
    let name = v["name"].as_str().unwrap_or("unknown");
    let version = v["version"].as_str().unwrap_or("?");
    let bins: Vec<(String, String)> = if let Some(bin) = v["bin"].as_str() {
        vec![(name.rsplit('/').next().unwrap_or(name).into(), bin.into())]
    } else {
        v["bin"]
            .as_object()
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.into())))
                    .collect()
            })
            .unwrap_or_default()
    };
    let add = |bin: Option<&str>, script: Option<&str>, snapshot: &mut Snapshot| {
        let label = bin
            .map(|b| format!("{b}  ·  {name}"))
            .unwrap_or_else(|| name.into());
        let mut e = Entry::new(
            format!("package:{}:{}", dir.display(), bin.unwrap_or("")),
            label,
            Origin::Installed {
                manager: manager.into(),
            },
            format!("{name}@{version}"),
        );
        e.runtime = runtime.into();
        e.location = dir.display().to_string();
        e.detail = format!(
            "{manager} · {name}@{version}. {}",
            v["description"].as_str().unwrap_or_default()
        );
        if let Some(script) = script {
            let target = dir.join(script);
            if let Ok(target) = fs::canonicalize(&target).map(crate::store::command_path) {
                // bin metadata can refer to a native binary or a shell script; only
                // choose a JS runtime for explicit JS or a node/bun shebang.
                let header = fs::File::open(&target)
                    .ok()
                    .and_then(|mut f| {
                        use std::io::Read;
                        let mut b = [0; 128];
                        f.read(&mut b)
                            .ok()
                            .map(|n| String::from_utf8_lossy(&b[..n]).into_owned())
                    })
                    .unwrap_or_default();
                let js = target
                    .extension()
                    .is_some_and(|e| e == "js" || e == "mjs" || e == "cjs" || e == "ts")
                    || (header.starts_with("#!")
                        && (header.lines().next().unwrap_or_default().contains("node")
                            || header.lines().next().unwrap_or_default().contains("bun")));
                if js {
                    e.program = find_executable(if runtime == "Bun" { "bun" } else { "node" });
                    e.args = vec![target.display().to_string()];
                } else if target.extension().is_some_and(|e| e == "exe")
                    || cfg!(unix) && !header.starts_with("#!")
                {
                    e.program = Some(target);
                }
                e.command = e
                    .program
                    .as_ref()
                    .map(|p| format!("{} {}", p.display(), e.args.join(" ")))
                    .unwrap_or_else(|| {
                        format!("{} (select interpreter)", dir.join(script).display())
                    });
            }
        }
        snapshot.entries.push(e);
    };
    if bins.is_empty() {
        add(None, None, snapshot);
    } else {
        for (bin, script) in bins {
            add(Some(&bin), Some(&script), snapshot);
        }
    }
}
fn scan_bin(snapshot: &mut Snapshot, dir: &Path, manager: &str, runtime: &str) {
    if let Ok(files) = fs::read_dir(dir) {
        for file in files.flatten() {
            let path = file.path();
            if !path.is_file() {
                continue;
            }
            if cfg!(windows) && path.extension().is_none_or(|e| e != "exe") {
                continue;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if fs::metadata(&path).is_ok_and(|m| m.permissions().mode() & 0o111 == 0) {
                    continue;
                }
            }
            installed_binary(snapshot, &path, manager, runtime, "");
        }
    }
}
fn installed_binary(
    snapshot: &mut Snapshot,
    path: &Path,
    manager: &str,
    runtime: &str,
    package: &str,
) {
    let mut e = Entry::new(
        format!("binary:{}", path.display()),
        path.file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        Origin::Installed {
            manager: manager.into(),
        },
        path.display().to_string(),
    );
    e.runtime = runtime.into();
    e.program = Some(path.into());
    e.location = path.display().to_string();
    e.detail = format!(
        "{manager} · {package}. Runtime attribution comes from install location; native binaries may not expose their original language."
    );
    snapshot.entries.push(e);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn package_cli_uses_runtime_and_literal_script_path() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("package.json"),
            r#"{"name":"@test/tool","version":"1.2.3","bin":{"tool":"cli.js"}}"#,
        )
        .unwrap();
        fs::write(dir.path().join("cli.js"), "#!/usr/bin/env node\n").unwrap();
        let mut s = Snapshot::default();
        node_package(&mut s, dir.path(), "npm", "Node.js");
        assert_eq!(s.entries.len(), 1);
        assert_eq!(s.entries[0].runtime, "Node.js");
        assert!(s.entries[0].args[0].ends_with("cli.js"));
        assert_eq!(s.entries[0].startup, None);
    }
}
