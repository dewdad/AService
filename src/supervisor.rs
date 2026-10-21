use crate::{
    command,
    model::{AppStatus, ManagedApp, RestartPolicy, SupervisorStatus, now},
    store::Store,
};
use anyhow::{Context, Result};
use std::{
    collections::{BTreeMap, VecDeque},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const HEALTHY_AFTER: Duration = Duration::from_secs(60);
const FAILURE_WINDOW: Duration = Duration::from_secs(300);
const MAX_FAILURES: usize = 5;
const STOP_GRACE: Duration = Duration::from_secs(10);
const LOG_LIMIT: u64 = 2 * 1024 * 1024;

#[derive(Default)]
struct Slot {
    child: Option<Child>,
    tree: Option<ProcessTree>,
    started: Option<Instant>,
    stopping: Option<Instant>,
    failures: VecDeque<Instant>,
    streak: u32,
    retry: Option<Instant>,
    blocked: bool,
    completed: bool,
    generation: u64,
    definition: Option<String>,
    status: AppStatus,
}

/// Retry 2, 4, 8... seconds, capped at 60, with deterministic per-app jitter.
pub fn retry_delay(streak: u32, entropy: u64) -> Duration {
    let base = 2u64.saturating_pow(streak.min(6)).min(60);
    Duration::from_millis((base * 1000 + entropy % 501).min(60_000))
}

pub fn ensure_running(store: &Store) -> Result<()> {
    if store.is_locked("daemon.lock") {
        return await_ready(store);
    }
    let mut command = Command::new(std::env::current_exe()?);
    command.arg("--data-dir").arg(&store.root).arg("daemon");
    command::hidden(&mut command)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid is async-signal-safe and does not access Rust state.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    let mut child = command
        .spawn()
        .context("Could not launch the background supervisor")?;
    // Reap the helper when it eventually exits; never wait on the UI thread.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    await_ready(store)
}

fn await_ready(store: &Store) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        if store.is_locked("daemon.lock")
            && store
                .status()
                .is_ok_and(|s| s.pid > 0 && now().saturating_sub(s.updated_at) < 3)
        {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(80));
    }
    anyhow::bail!(
        "Supervisor did not become ready within 8 seconds; see last-error.txt and the discovery report"
    )
}

pub fn run(store: Store, login: bool) -> Result<()> {
    let _lock = match store.lock("daemon.lock", true) {
        Ok(lock) => lock,
        Err(e) if crate::store::lock_contended(&e) => {
            return Ok(());
        }
        Err(e) => return Err(e.context("Cannot acquire supervisor lock")),
    };
    if login {
        let token = login_token();
        let previous: String = store.read("login-session.json")?;
        if token != previous {
            store.update(|c| {
                for app in &mut c.apps {
                    app.desired_running = app.start_on_login;
                    if app.desired_running {
                        app.generation = app.generation.wrapping_add(1);
                    }
                }
                Ok(())
            })?;
            store.write("login-session.json", &token)?;
        }
    }
    let stop = Arc::new(AtomicBool::new(false));
    let signal = stop.clone();
    ctrlc::set_handler(move || signal.store(true, Ordering::SeqCst))?;
    fs::create_dir_all(store.path("logs"))?;
    let mut slots: BTreeMap<String, Slot> = BTreeMap::new();
    let mut error: Option<anyhow::Error> = None;
    let mut last_states = BTreeMap::new();
    let mut last_write = Instant::now() - Duration::from_secs(2);
    loop {
        let config = match store.load() {
            Ok(c) => c,
            Err(e) => {
                error = Some(e);
                stop.store(true, Ordering::SeqCst);
                Default::default()
            }
        };
        let shutdown = stop.load(Ordering::SeqCst);
        let gui_open = store.is_locked("gui.lock");
        let mut active_ids = std::collections::HashSet::new();
        for app in &config.apps {
            active_ids.insert(app.id.clone());
            let slot = slots.entry(app.id.clone()).or_default();
            let desired = !shutdown && app.desired_running && (app.run_in_background || gui_open);
            tick(&store, app, slot, desired);
        }
        for (id, slot) in &mut slots {
            if !active_ids.contains(id) {
                stop_slot(slot);
            }
        }
        slots.retain(|id, s| active_ids.contains(id) || s.child.is_some());
        let status = SupervisorStatus {
            pid: std::process::id(),
            updated_at: now(),
            apps: slots
                .iter()
                .map(|(id, s)| (id.clone(), s.status.clone()))
                .collect(),
        };
        if status.apps != last_states || last_write.elapsed() >= Duration::from_secs(1) || shutdown
        {
            if let Err(e) = store.write("status.json", &status) {
                error = Some(e);
                stop.store(true, Ordering::SeqCst);
            } else {
                last_states = status.apps;
                last_write = Instant::now();
            }
        }
        if shutdown && slots.values().all(|s| s.child.is_none()) {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    if let Some(e) = error {
        return Err(e);
    }
    Ok(())
}

fn login_token() -> String {
    let boot = sysinfo::System::boot_time();
    #[cfg(windows)]
    {
        if let Ok(s) = command::powershell(
            "([System.Security.Principal.WindowsIdentity]::GetCurrent().Groups | Where-Object Value -like 'S-1-5-5-*' | Select-Object -First 1).Value",
            &[],
        ) && !s.is_empty()
        {
            return format!("{boot}:{s}");
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(s) = command::run(
            "loginctl",
            &[
                "show-user",
                &unsafe { libc::getuid() }.to_string(),
                "--property=TimestampMonotonic",
                "--value",
            ],
        ) && !s.is_empty()
        {
            return format!("{boot}:{s}");
        }
        if let Ok(s) = std::env::var("XDG_SESSION_ID") {
            return format!("{boot}:{s}");
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(s) = command::run("launchctl", &["managerpid"]) {
            return format!("{boot}:{s}");
        }
    }
    format!("{boot}")
}

fn definition(app: &ManagedApp) -> String {
    format!(
        "{:?}|{:?}|{:?}|{:?}",
        app.program, app.args, app.working_dir, app.env
    )
}

fn tick(store: &Store, app: &ManagedApp, slot: &mut Slot, desired: bool) {
    let def = definition(app);
    let changed = slot.generation != app.generation || slot.definition.as_ref() != Some(&def);
    if changed {
        slot.generation = app.generation;
        slot.definition = Some(def);
        slot.blocked = false;
        slot.completed = false;
        slot.retry = None;
        slot.failures.clear();
        slot.streak = 0;
        if slot.child.is_some() {
            begin_stop(slot);
        }
    }
    if !desired {
        slot.retry = None;
        slot.completed = false;
        stop_slot(slot);
        return;
    }
    if slot.stopping.is_some() {
        stop_slot(slot);
        return;
    }
    if let Some(child) = &mut slot.child {
        match child.try_wait() {
            Ok(Some(exit)) => {
                let healthy = slot.started.is_some_and(|i| i.elapsed() >= HEALTHY_AFTER);
                // Closing a Windows job kills descendants; on Unix clean up the
                // process group even if its root exited before its children.
                slot.tree.take();
                slot.child.take();
                slot.status.pid = None;
                if healthy {
                    slot.failures.clear();
                    slot.streak = 0;
                }
                slot.status.message = format!("Exited: {exit}");
                let retry = match app.restart {
                    RestartPolicy::Never => false,
                    RestartPolicy::OnFailure => !exit.success(),
                    RestartPolicy::Always => true,
                };
                if retry {
                    record_failure(slot);
                } else {
                    slot.completed = true;
                    slot.status.state = "Exited".into();
                }
            }
            Ok(None) => {
                slot.status.state = "Running".into();
                return;
            }
            Err(e) => {
                slot.status.message = format!("Cannot read child state: {e}");
                begin_stop(slot);
                return;
            }
        }
    }
    if slot.blocked {
        slot.status.state = "Paused · crash loop".into();
        return;
    }
    if slot.completed {
        return;
    }
    if let Some(retry) = slot.retry
        && Instant::now() < retry
    {
        slot.status.state = "Backing off".into();
        slot.status.next_retry =
            Some(now() + retry.saturating_duration_since(Instant::now()).as_secs());
        return;
    }
    slot.retry = None;
    slot.status.next_retry = None;
    match spawn(store, app) {
        Ok((child, tree)) => {
            slot.status.pid = Some(child.id());
            slot.child = Some(child);
            slot.tree = Some(tree);
            slot.started = Some(Instant::now());
            slot.status.state = "Running".into();
            slot.status.message = "Supervised · silent launch".into();
        }
        Err(e) => {
            slot.status.message = format!("Launch failed: {e:#}");
            if app.restart == RestartPolicy::Never {
                slot.completed = true;
                slot.status.state = "Launch failed".into();
            } else {
                record_failure(slot);
            }
        }
    }
}

fn record_failure(slot: &mut Slot) {
    let instant = Instant::now();
    while slot
        .failures
        .front()
        .is_some_and(|t| instant.duration_since(*t) > FAILURE_WINDOW)
    {
        slot.failures.pop_front();
    }
    slot.failures.push_back(instant);
    slot.streak = slot.streak.saturating_add(1);
    slot.status.restarts = slot.status.restarts.saturating_add(1);
    if slot.failures.len() >= MAX_FAILURES {
        slot.blocked = true;
        slot.retry = None;
        slot.status.next_retry = None;
        slot.status.state = "Paused · crash loop".into();
        slot.status
            .message
            .push_str(". Five exits/launch failures in five minutes. Start or Restart to retry.");
    } else {
        slot.retry = Some(instant + retry_delay(slot.streak, now()));
        slot.status.state = "Backing off".into();
    }
}

fn begin_stop(slot: &mut Slot) {
    if slot.stopping.is_some() {
        return;
    }
    if let Some(child) = &mut slot.child {
        slot.stopping = Some(Instant::now());
        slot.status.state = "Stopping".into();
        #[cfg(unix)]
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGTERM);
        }
        #[cfg(windows)]
        {
            let mut cmd = Command::new("taskkill.exe");
            cmd.args(["/PID", &child.id().to_string(), "/T"]);
            // Windows has no universal graceful protocol for a console-less app.
            // taskkill attempts window close; the job is force-closed at deadline.
            let _ = command::hidden(&mut cmd)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map(|mut c| {
                    std::thread::spawn(move || {
                        let _ = c.wait();
                    })
                });
        }
    }
}
fn stop_slot(slot: &mut Slot) {
    begin_stop(slot);
    if let Some(child) = &mut slot.child {
        let done = child.try_wait().ok().flatten().is_some();
        if done || slot.stopping.is_some_and(|t| t.elapsed() >= STOP_GRACE) {
            slot.tree.take();
            let _ = child.kill();
            let _ = child.wait();
            slot.child = None;
            slot.stopping = None;
            slot.status.pid = None;
            slot.status.next_retry = None;
            slot.status.state = "Stopped".into();
        }
    } else {
        slot.status.pid = None;
        slot.status.next_retry = None;
        slot.status.state = "Stopped".into();
        slot.stopping = None;
    }
}

fn spawn(store: &Store, app: &ManagedApp) -> Result<(Child, ProcessTree)> {
    app.validate()?;
    let mut command = Command::new(&app.program);
    command.args(&app.args).envs(&app.env);
    if let Some(dir) = &app.working_dir {
        command.current_dir(dir);
    } else {
        command.current_dir(&store.root);
    }
    command::hidden(&mut command)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Suspend the primary thread until it belongs to the kill-on-close job.
        // Otherwise a fast launcher can create descendants before assignment.
        command.creation_flags(0x08000000 | 0x00000200 | 0x00000004);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        let parent = std::process::id() as libc::pid_t;
        // SAFETY: only async-signal-safe syscalls execute between fork and exec.
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::getppid() != parent {
                    libc::_exit(1);
                }
                Ok(())
            });
        }
    }
    // Open logs before launching, so an unwritable log directory cannot leave a
    // partially launched child behind.
    let logger = Arc::new(Mutex::new(RotatingLog::new(store.log_path(&app.id))?));
    let mut child = command.spawn()?;
    let tree = match ProcessTree::new(&child) {
        Ok(t) => t,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
    };
    for pipe in [
        Box::new(child.stdout.take().unwrap()) as Box<dyn Read + Send>,
        Box::new(child.stderr.take().unwrap()) as Box<dyn Read + Send>,
    ] {
        let logger = logger.clone();
        std::thread::spawn(move || {
            let mut pipe = pipe;
            let mut buf = [0; 8192];
            while let Ok(n) = pipe.read(&mut buf) {
                if n == 0 {
                    break;
                }
                if let Ok(mut l) = logger.lock()
                    && l.write(&buf[..n]).is_err()
                {
                    break;
                }
            }
        });
    }
    Ok((child, tree))
}

struct RotatingLog {
    path: PathBuf,
    file: Option<File>,
    size: u64,
}
impl RotatingLog {
    fn new(path: PathBuf) -> Result<Self> {
        fs::create_dir_all(path.parent().unwrap())?;
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let size = file.metadata()?.len();
        Ok(Self {
            path,
            file: Some(file),
            size,
        })
    }
    fn write(&mut self, bytes: &[u8]) -> Result<()> {
        if self.size + bytes.len() as u64 > LOG_LIMIT {
            self.file.take();
            for i in (1..=2).rev() {
                let from = self.path.with_extension(format!("log.{i}"));
                let to = self.path.with_extension(format!("log.{}", i + 1));
                if to.exists() {
                    fs::remove_file(&to)?;
                }
                if from.exists() {
                    fs::rename(from, to)?;
                }
            }
            let one = self.path.with_extension("log.1");
            if one.exists() {
                fs::remove_file(&one)?;
            }
            fs::rename(&self.path, one)?;
            self.file = Some(
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.path)?,
            );
            self.size = 0;
        }
        self.file
            .as_mut()
            .context("Log file unavailable")?
            .write_all(bytes)?;
        self.size += bytes.len() as u64;
        Ok(())
    }
}

#[cfg(unix)]
struct ProcessTree {
    i: i32,
}
#[cfg(unix)]
impl ProcessTree {
    fn new(child: &Child) -> Result<Self> {
        Ok(Self {
            i: child.id() as i32,
        })
    }
}
#[cfg(unix)]
impl Drop for ProcessTree {
    fn drop(&mut self) {
        unsafe {
            libc::kill(-self.i, libc::SIGKILL);
        }
    }
}

#[cfg(windows)]
struct ProcessTree {
    handle: windows_sys::Win32::Foundation::HANDLE,
}
#[cfg(windows)]
impl ProcessTree {
    fn new(child: &Child) -> Result<Self> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::{Foundation::CloseHandle, System::JobObjects::*};
        // SAFETY: structure size and handles match the Win32 API. The owning
        // ProcessTree closes the job once; the child handle remains borrowed.
        unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            anyhow::ensure!(
                !handle.is_null(),
                "CreateJobObject failed: {}",
                std::io::Error::last_os_error()
            );
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                std::mem::size_of_val(&info) as u32,
            ) == 0
                || AssignProcessToJobObject(handle, child.as_raw_handle()) == 0
            {
                let err = std::io::Error::last_os_error();
                CloseHandle(handle);
                return Err(err.into());
            }
            let tree = Self { handle };
            use windows_sys::Win32::{
                Foundation::INVALID_HANDLE_VALUE,
                System::{
                    Diagnostics::ToolHelp::*,
                    Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME},
                },
            };
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
            if snapshot == INVALID_HANDLE_VALUE {
                return Err(std::io::Error::last_os_error().into());
            }
            let mut entry: THREADENTRY32 = std::mem::zeroed();
            entry.dwSize = std::mem::size_of_val(&entry) as u32;
            let mut has_entry = Thread32First(snapshot, &mut entry) != 0;
            let mut resumed = false;
            while has_entry {
                if entry.th32OwnerProcessID == child.id() {
                    let thread = OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID);
                    if !thread.is_null() {
                        resumed = ResumeThread(thread) != u32::MAX;
                        CloseHandle(thread);
                    }
                    break;
                }
                has_entry = Thread32Next(snapshot, &mut entry) != 0;
            }
            CloseHandle(snapshot);
            anyhow::ensure!(
                resumed,
                "Could not resume the application after assigning its process job"
            );
            Ok(tree)
        }
    }
}
#[cfg(windows)]
impl Drop for ProcessTree {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retry_is_capped_and_crash_loop_opens_circuit() {
        assert!(retry_delay(20, 500) <= Duration::from_secs(60));
        let mut slot = Slot::default();
        for _ in 0..5 {
            record_failure(&mut slot);
        }
        assert!(slot.blocked);
        assert!(slot.retry.is_none());
    }
    #[test]
    fn logs_are_bounded_and_preserve_recent_output() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("test.log");
        let mut log = RotatingLog::new(path.clone()).unwrap();
        let bytes = vec![b'x'; 8192];
        for _ in 0..1100 {
            log.write(&bytes).unwrap();
        }
        drop(log);
        let total: u64 = fs::read_dir(d.path())
            .unwrap()
            .map(|p| p.unwrap().metadata().unwrap().len())
            .sum();
        assert!(total <= 4 * LOG_LIMIT);
        assert!(path.with_extension("log.3").exists());
    }

    #[test]
    fn launch_failures_respect_never_and_explicit_start_clears_circuit() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(Some(dir.path().into())).unwrap();
        let file = store.path("not-an-executable.txt");
        fs::write(&file, b"invalid executable").unwrap();
        let mut app = ManagedApp {
            name: "Invalid executable".into(),
            program: file,
            restart: RestartPolicy::Never,
            ..Default::default()
        };
        let mut slot = Slot::default();
        tick(&store, &app, &mut slot, true);
        assert_eq!(slot.status.state, "Launch failed");
        assert_eq!(slot.status.restarts, 0);
        assert!(slot.retry.is_none());
        app.restart = RestartPolicy::OnFailure;
        app.generation += 1;
        for _ in 0..5 {
            slot.retry = None;
            tick(&store, &app, &mut slot, true);
        }
        assert!(slot.blocked);
        app.restart = RestartPolicy::Never;
        app.generation += 1;
        tick(&store, &app, &mut slot, true);
        assert!(!slot.blocked);
        assert!(slot.completed);
    }
}
