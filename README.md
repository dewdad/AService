# AService

A native Rust desktop app for finding and managing background applications across language runtimes and operating-system startup managers. The GUI uses [eframe/egui](https://docs.rs/eframe/0.37.0/eframe/); a separate headless process owns supervised applications. Closing the dashboard does not stop apps that have **Keep running after window closes** enabled.

## Run

Requires Rust 1.95 or newer and the platform's desktop graphics libraries.

```sh
cargo run --release
```

On Windows, launch `target/release/aservice.exe`. The executable uses the Windows GUI subsystem, so the supervisor, discovery helpers, and recovery launches do not flash a console. On Linux, install your distribution's X11/Wayland/OpenGL development dependencies before building (Ubuntu: `libx11-dev libxkbcommon-dev libwayland-dev libgl1-mesa-dev`). On macOS, build natively with the Xcode command-line tools installed.

For a portable release archive or macOS app bundle (packaging does not sign or notarize builds):

```sh
python scripts/package.py
```

Move the executable/bundle to its permanent location **before enabling login startup**. Startup definitions store its absolute path. If you later move it, disable and re-enable the login helper from the new location. The application creates no startup registration simply by opening or scanning.

## Use

1. Open **All applications**, filter by runtime or search a command/source. **Running processes** shows observations from the last scan. Refresh to rescan.
2. Select a record to inspect its command, owning manager, enabled/running state, PID when available, and source location. Records intentionally remain separate: an installed package, a service definition, and a process are different observations, not proof of a single application.
3. Use native controls on supported existing entries. A review dialog shows the source before applying a change; backups go in the local data directory. Disabling startup does not implicitly stop the app. Services/schedules keep their native recovery rules. Windows service auto-start toggles map to **Automatic / Manual**; the details show Disabled and delayed-start modes. Re-enabling chooses Automatic; it does not restore every previous service policy.
4. To supervise an installed tool, select **Create managed copy**, or **Add application**. Choose the runtime/native executable, literal arguments (one per line), optional working directory, and environment JSON. Save, then Start. Windows `.cmd`, `.bat`, `.ps1`, `.vbs`, and shortcut wrappers must be passed to their explicit interpreter. For npm CLIs the inventory resolves JS entry points to `node` plus the script path where possible.
5. Enable **Start at login** per managed app. This also registers the per-user supervisor login helper. **Keep running after window closes** is independent: disabling it permits execution only while this profile's GUI lock is held. A clean GUI exit or crash releases that lock and the supervisor stops those apps.
6. **Stop** persists intent before termination and cancels pending recovery. **Start / Restart** clears a paused crash loop. Logs show recent captured output. Remove stops the managed process tree and keeps its logs.

Creating a managed copy never stops or disables the original manager. Check the source details to avoid launching the same app twice. Interactive installers and commands requiring terminal input are not suitable background processes.

## Discovery and controls

| Source | Discovered | Controls |
|---|---|---|
| Node/npm, pnpm, Yarn | Known global roots, current npm prefix, `NODE_PATH`, package manifests and CLI bins | Create managed entry |
| Bun | Global package metadata under `BUN_INSTALL` / `~/.bun` | Create managed entry |
| Rust/Cargo | `CARGO_HOME` / `~/.cargo`, `.crates2.json`, bin fallback | Create managed entry |
| Go | `GOBIN`, `GOPATH`, `go env`, default `~/go/bin` | Create managed entry |
| Python | Known isolated pipx/uv tool environments and PATH runtimes | Create managed entry |
| Other runtimes | Deno/.NET on PATH; runtime hints on commands | Create managed entry |
| Windows | HKCU/HKLM Run, RunOnce, Run32; user/common Startup folders; scheduled tasks and CIM services | User Run approval/startup-folder toggles; task enable/disable/start/stop; service startup/start/stop |
| Linux | User/system systemd services and timers; user/system XDG autostart; user crontab, `/etc/crontab`, cron.d and periodic script folders | systemd enable/disable/start/stop; user XDG autostart toggle |
| macOS | User/system LaunchAgents and LaunchDaemons, launchd overrides, user crontab | User launchd enable/disable/start/stop |
| PM2 | Persisted `PM2_HOME/dump.pm2` definitions | Inspect and create managed copy |
| supervisord | Program definitions in standard Linux conf.d locations | Inspect |
| Processes | OS process snapshot, available executable/argv/PID | Inspect and create a separate managed entry |

Discovery is best effort, with notices visible in **Discovery report**. No process manager or installed package is started to inspect it. npm/Go path queries invoke only the detected manager/runtime with fixed read commands. Installed binaries are attributed from installation metadata/location; their implementation language cannot always be established.

Custom version-manager prefixes, project-local dependencies, alternate Python tool roots, containers, WSL, other users, and manager-specific private databases are not exhaustively enumerated. Run separately within each supported OS environment. PM2 dumps are saved configuration, not live state or proof of auto-start. Cron and system launchd definitions remain inspect-only. Modern macOS Login Items/SMAppService and Background Task Management require System Settings and are explicitly reported as a coverage gap. XDG `OnlyShowIn`, `NotShowIn`, and `TryExec` are displayed; session applicability is not inferred. systemd static/indirect units have unknown auto-start status because dependencies may activate them.

Windows startup approval uses the Windows Explorer `StartupApproved` registry format. It is not a stable public management API; verify discovery after changing an entry or upgrading Windows. RunOnce and machine Run entries are inspect-only. Privileged operations surface native permission errors and never trigger automatic elevation. Existing scheduled actions may have their own visibility behavior; their command definitions are preserved.

## Recovery defaults

| Setting | Default |
|---|---|
| Recovery | Restart on nonzero exit or launch failure; clean exit stays stopped |
| Retry delay | 2, 4, 8, 16, 32, then up to 60 seconds; up to 500 ms jitter within cap |
| Healthy reset | Failure streak resets after 60 seconds of continuous execution |
| Crash circuit | Pause after 5 failures in a rolling 5-minute window; explicit Start/Restart resets |
| Stop | Cancel retries; attempt graceful stop; force process-tree cleanup after 10 seconds |
| Output | stdout/stderr captured; 2 MiB per log, 3 rotated backups per app |
| Launch | Absolute executable + literal argv, null stdin, optional cwd/environment, no implicit shell |
| GUI / daemon ownership | Separate exclusive profile locks; single daemon; readiness handshake |
| State | Atomic JSON replacements and serialized configuration updates; corrupt/newer config preserved |

Windows processes start suspended, enter a kill-on-close Job Object, and then resume, preventing fast launchers from escaping job assignment. Windows has no universal graceful shutdown protocol for console-less apps; AService attempts `taskkill /T` without `/F`, then closes the job at the deadline. Unix launches have their own process group, receive SIGTERM, then SIGKILL; Linux also sets parent-death SIGKILL on the root. These controls target foreground server/CLI processes. Daemonizing, process-group escape, explicit job breakaway, and applications opening their own GUI windows are outside the silence/containment contract. Prefer native services for applications designed to daemonize.

The login helper uses a hidden least-privilege Task Scheduler task on Windows (3 supervisor failure retries, no execution/battery limit), a systemd user service on Linux (Restart=on-failure, rate limit), or a launchd agent on macOS (unsuccessful-exit recovery, throttling). Startup registrations are isolated by profile. Logout/OS session behavior still applies; AService does not enable Linux lingering or install a system service. Managed Start-at-login intent resets once per detected login session, and is preserved across supervisor retries within that session. If session identification is unavailable, it falls back to boot identity.

Crash counters/backoff are in supervisor memory, while desired run state and configuration are durable. A standalone Unix supervisor killed abruptly can leave descendants that deliberately escape its group; systemd/launchd provide stronger service ownership. Process liveness is monitored, not application-level readiness or HTTP health. Network availability and dependency ordering are the application's responsibility.

## Local data and CLI

Configuration, status, logs and pre-change backups live in the OS's per-user local application data directory (shown in Settings). No server, telemetry, cloud account, or network API is needed by the app. Child output and environment overrides can contain secrets; environment values are plaintext local configuration. Prefer OS secret stores.

```sh
aservice scan --output inventory.json
aservice status --output state.json
aservice --data-dir /absolute/profile daemon
aservice --data-dir /absolute/profile scan --output inventory.json
```

Use `--output` when scripting on Windows, because a GUI-subsystem executable may have no console stdout. `status` exports configuration including environment overrides. `last-error.txt` records startup errors. Backups are inspection/repair artifacts, not an automated rollback engine; task definitions are XML, registry/service metadata is JSON, and desktop backups preserve source text. No credentials are stored by AService for privileged operations.

## Validation

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked
```

Tests launch real processes in temporary profiles: start/stop/restart, singleton daemon locking, literal environment propagation, clean-exit behavior, failure backoff cancellation, GUI-only lifetime, descendant cleanup, bounded log rotation, and corrupt-config preservation. The ignored test is a child-process fixture invoked by the integration tests. Tests do not register any login helper or alter existing services/tasks. CI builds/tests Windows, Linux and macOS natively and uploads executables. The initial implementation was run and visually inspected on Windows; Linux and macOS were cross-checked for compilation, with native behavior validation left to the CI matrix/local platform runs.

Architecture: `discovery` builds source records; `platform` queries/controls native managers; `store` serializes configuration; `supervisor` owns child lifecycles and logs; `gui` dispatches scans/mutations off the UI thread. Adding a runtime inventory source does not require changing supervision.
