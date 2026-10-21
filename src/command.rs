//! All helper processes are hidden on Windows, detached from terminal stdin,
//! and bounded by a deadline. Application argv is never interpreted as shell code.
use anyhow::{Context, Result, bail};
use std::{
    io::Read,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub fn hidden(command: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    command.stdin(Stdio::null());
    command
}

pub fn output(command: &mut Command, timeout: Duration) -> Result<String> {
    hidden(command)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let label = command.get_program().to_string_lossy().into_owned();
    let mut child = command
        .spawn()
        .with_context(|| format!("Unable to launch {label}"))?;
    // Drain both pipes concurrently to avoid blocking at a full pipe.
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let read = |mut stream: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let mut buffer = [0u8; 8192];
            while let Ok(n) = stream.read(&mut buffer) {
                if n == 0 {
                    break;
                }
                if bytes.len() < 8 * 1024 * 1024 {
                    bytes.extend_from_slice(&buffer[..n]);
                }
            }
            bytes
        })
    };
    let out = read(Box::new(stdout));
    let err = read(Box::new(stderr));
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            // Do not join pipes: a descendant could still hold them open.
            bail!("{label} timed out after {} seconds", timeout.as_secs());
        }
        std::thread::sleep(Duration::from_millis(40));
    };
    // Descendants must not be able to keep discovery blocked after the parent exits.
    let wait_until = Instant::now() + Duration::from_secs(2);
    while !(out.is_finished() && err.is_finished()) {
        if Instant::now() > wait_until {
            bail!("{label}: output pipes remained open after exit");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let stdout = String::from_utf8_lossy(&out.join().unwrap_or_default())
        .trim_start_matches('\u{feff}')
        .trim()
        .to_owned();
    let stderr = String::from_utf8_lossy(&err.join().unwrap_or_default())
        .trim()
        .to_owned();
    if !status.success() {
        bail!(
            "{label} failed ({status}): {}",
            stderr.chars().take(1500).collect::<String>()
        );
    }
    Ok(stdout)
}

pub fn run(program: &str, args: &[&str]) -> Result<String> {
    output(Command::new(program).args(args), Duration::from_secs(20))
}

#[cfg(windows)]
pub fn powershell(script: &str, env: &[(&str, &str)]) -> Result<String> {
    let mut command = Command::new("powershell.exe");
    command.args([
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        script,
    ]);
    for (key, value) in env {
        command.env(key, value);
    }
    output(&mut command, Duration::from_secs(45))
}
