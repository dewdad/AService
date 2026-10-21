use super::*;
use crate::command::powershell;
use anyhow::Context;
use serde_json::Value;

const DISCOVER: &str = include_str!("windows_discover.ps1");

pub fn discover(snapshot: &mut Snapshot) {
    match powershell(DISCOVER, &[]) {
        Ok(json) => match serde_json::from_str::<Value>(&json) {
            Ok(data) => {
                if let Some(warnings) = data["warnings"].as_array() {
                    for w in warnings {
                        snapshot
                            .warnings
                            .push(w.as_str().unwrap_or_default().into());
                    }
                }
                if let Some(rows) = data["entries"].as_array() {
                    for row in rows {
                        let s = |key: &str| row[key].as_str().unwrap_or_default().to_string();
                        let origin = match s("kind").as_str() {
                            "run" | "runonce" => Origin::WindowsRun {
                                key: s("key"),
                                value: s("value"),
                                approved: s("approved"),
                            },
                            "startup" => Origin::StartupFile {
                                path: s("path").into(),
                            },
                            "task" => Origin::WindowsTask { path: s("path") },
                            "service" => Origin::WindowsService { name: s("value") },
                            _ => continue,
                        };
                        let mut e = Entry::new(s("id"), s("name"), origin, s("command"));
                        e.location = s("location");
                        e.detail = s("detail");
                        e.startup = row["enabled"].as_bool();
                        e.running = row["running"].as_bool();
                        e.pid = row["pid"].as_u64().filter(|p| *p > 0).map(|p| p as u32);
                        snapshot.entries.push(e);
                    }
                }
            }
            Err(e) => snapshot
                .warnings
                .push(format!("Windows discovery returned invalid data: {e}")),
        },
        Err(e) => snapshot
            .warnings
            .push(format!("Windows startup discovery: {e:#}")),
    }
}

pub fn apply(store: &Store, entry: &Entry, action: Action) -> Result<()> {
    let on = if action == Action::Enable { "1" } else { "0" };
    let backup = store
        .path("backups")
        .join(format!("native-{}.json", uuid::Uuid::new_v4()));
    let backup = backup.to_string_lossy();
    match &entry.origin {
        Origin::WindowsRun {
            key,
            value,
            approved,
        } => {
            powershell(
                r#"$ErrorActionPreference='Stop'; $current=(Get-Item -LiteralPath $env:AS_KEY).GetValue($env:AS_NAME); $approvedKey=Get-Item -LiteralPath $env:AS_APPROVED -ErrorAction SilentlyContinue; $original=if($approvedKey){$approvedKey.GetValue($env:AS_NAME,$null)}else{$null}; @{key=$env:AS_KEY;name=$env:AS_NAME;value=$current;approved=$original} | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $env:AS_BACKUP -Encoding UTF8; New-Item -Path $env:AS_APPROVED -Force | Out-Null; $bytes=New-Object byte[] 12; if($env:AS_ON -eq '1'){$bytes[0]=2}else{$bytes[0]=3}; New-ItemProperty -LiteralPath $env:AS_APPROVED -Name $env:AS_NAME -PropertyType Binary -Value $bytes -Force | Out-Null"#,
                &[
                    ("AS_KEY", key),
                    ("AS_NAME", value),
                    ("AS_APPROVED", approved),
                    ("AS_ON", on),
                    ("AS_BACKUP", &backup),
                ],
            )?;
        }
        Origin::StartupFile { path } => {
            let disabled = path.to_string_lossy().ends_with(".aservice-disabled");
            anyhow::ensure!(
                user_startup_path(path),
                "Only your own Startup folder can be modified"
            );
            let actual = if disabled {
                std::path::PathBuf::from(
                    path.to_string_lossy()
                        .trim_end_matches(".aservice-disabled"),
                )
            } else {
                path.clone()
            };
            if disabled && action == Action::Enable {
                anyhow::ensure!(
                    !actual.exists(),
                    "Destination already exists; refusing to overwrite"
                );
            }
            let filename = actual
                .file_name()
                .context("Startup file has no filename")?
                .to_string_lossy();
            powershell(
                r#"$ErrorActionPreference='Stop'; $key='HKCU:\Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\StartupFolder'; $k=Get-Item -LiteralPath $key -ErrorAction SilentlyContinue; $original=if($k){$k.GetValue($env:AS_NAME,$null)}else{$null}; @{key=$key;name=$env:AS_NAME;approved=$original} | ConvertTo-Json | Set-Content -LiteralPath $env:AS_BACKUP -Encoding UTF8; New-Item -Path $key -Force | Out-Null; $bytes=New-Object byte[] 12; if($env:AS_ON -eq '1'){$bytes[0]=2}else{$bytes[0]=3}; New-ItemProperty -LiteralPath $key -Name $env:AS_NAME -PropertyType Binary -Value $bytes -Force | Out-Null"#,
                &[
                    ("AS_NAME", &filename),
                    ("AS_ON", on),
                    ("AS_BACKUP", &backup),
                ],
            )?;
            if disabled && action == Action::Enable {
                std::fs::rename(path, actual)?;
            }
        }
        Origin::WindowsTask { path } => {
            let (folder, name) = path.rsplit_once('\\').unwrap_or(("\\", path));
            let folder = format!("{folder}\\");
            let script = match action {
                Action::Enable => "Enable-ScheduledTask -InputObject $task | Out-Null",
                Action::Disable => "Disable-ScheduledTask -InputObject $task | Out-Null",
                Action::Start => "Start-ScheduledTask -InputObject $task",
                Action::Stop => "Stop-ScheduledTask -InputObject $task",
            };
            powershell(
                &format!(
                    "$ErrorActionPreference='Stop'; $tasks=@(Get-ScheduledTask | Where-Object {{ $_.TaskPath -ceq $env:AS_FOLDER -and $_.TaskName -ceq $env:AS_NAME }}); if($tasks.Count -ne 1){{throw 'Task source changed; refresh before retrying'}}; $task=$tasks[0]; Export-ScheduledTask -TaskName $task.TaskName -TaskPath $task.TaskPath | Set-Content -LiteralPath $env:AS_BACKUP -Encoding Unicode; {script}"
                ),
                &[
                    ("AS_FOLDER", &folder),
                    ("AS_NAME", name),
                    ("AS_BACKUP", &backup),
                ],
            )?;
        }
        Origin::WindowsService { name } => {
            let script = match action {
                Action::Enable => "Set-Service -InputObject $service -StartupType Automatic",
                Action::Disable => "Set-Service -InputObject $service -StartupType Manual",
                Action::Start => "Start-Service -InputObject $service",
                Action::Stop => "Stop-Service -InputObject $service",
            };
            powershell(
                &format!(
                    "$ErrorActionPreference='Stop'; $services=@(Get-Service | Where-Object Name -ceq $env:AS_NAME); if($services.Count -ne 1){{throw 'Service source changed; refresh before retrying'}}; $service=$services[0]; Get-CimInstance Win32_Service | Where-Object Name -ceq $env:AS_NAME | Select-Object Name,StartMode,DelayedAutoStart,PathName | ConvertTo-Json | Set-Content -LiteralPath $env:AS_BACKUP -Encoding UTF8; {script}"
                ),
                &[("AS_NAME", name), ("AS_BACKUP", &backup)],
            )?;
        }
        _ => return unsupported(&entry.origin),
    }
    Ok(())
}

fn task_name(store: &Store) -> String {
    format!("AService Supervisor {}", registration_id(store))
}

pub fn startup_enabled(store: &Store) -> Result<bool> {
    let result = powershell(
        "$ErrorActionPreference='Stop'; $t=Get-ScheduledTask -TaskName $env:AS_NAME -ErrorAction SilentlyContinue; if($null -ne $t -and $t.Settings.Enabled){'yes'}else{'no'}",
        &[("AS_NAME", &task_name(store))],
    )?;
    Ok(result == "yes")
}

pub fn install_startup(store: &Store, enabled: bool) -> Result<()> {
    let name = task_name(store);
    if !enabled {
        powershell(
            "$ErrorActionPreference='Stop'; $t=Get-ScheduledTask -TaskName $env:AS_NAME -ErrorAction SilentlyContinue; if($null -ne $t){Unregister-ScheduledTask -InputObject $t -Confirm:$false}",
            &[("AS_NAME", &name)],
        )?;
        return Ok(());
    }
    let executable = std::env::current_exe()?;
    let sid = powershell(
        "[System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value",
        &[],
    )?;
    let args = format!("--data-dir \"{}\" daemon --login", store.root.display());
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<Task version="1.4" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
<RegistrationInfo><Description>AService per-user background supervisor</Description></RegistrationInfo>
<Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{sid}</UserId><Delay>PT10S</Delay></LogonTrigger></Triggers>
<Principals><Principal id="Author"><UserId>{sid}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>
<Settings><MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy><DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries><StopIfGoingOnBatteries>false</StopIfGoingOnBatteries><AllowHardTerminate>true</AllowHardTerminate><StartWhenAvailable>true</StartWhenAvailable><RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable><Enabled>true</Enabled><Hidden>true</Hidden><ExecutionTimeLimit>PT0S</ExecutionTimeLimit><RestartOnFailure><Interval>PT1M</Interval><Count>3</Count></RestartOnFailure></Settings>
<Actions Context="Author"><Exec><Command>{exe}</Command><Arguments>{args}</Arguments><WorkingDirectory>{cwd}</WorkingDirectory></Exec></Actions></Task>"#,
        sid = xml_escape(&sid),
        exe = xml_escape(&executable.to_string_lossy()),
        args = xml_escape(&args),
        cwd = xml_escape(&store.root.to_string_lossy())
    );
    powershell(
        "$ErrorActionPreference='Stop'; Register-ScheduledTask -TaskName $env:AS_NAME -Xml $env:AS_XML -Force | Out-Null",
        &[("AS_NAME", &name), ("AS_XML", &xml)],
    )?;
    Ok(())
}
