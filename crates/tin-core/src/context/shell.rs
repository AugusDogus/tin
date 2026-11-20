//! Runs the small platform tools context comes from (`wl-paste`, `hyprctl`, `osascript`, …).

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long one probe may take. A selection owner that hangs mustn't hang the capture.
const TIMEOUT: Duration = Duration::from_secs(2);

/// What a tool printed, or why it couldn't be run (said for a person).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ran {
    /// Exit 0, with stdout.
    Ok(String),
    /// A nonzero exit, with stderr (often "nothing selected").
    Failed(String),
}

pub fn run(program: &str, args: &[&str]) -> Result<Ran, String> {
    run_with_input(program, args, None)
}

pub fn run_with_input(program: &str, args: &[&str], input: Option<&str>) -> Result<Ran, String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => format!("`{program}` isn't installed"),
            _ => format!("couldn't run `{program}`: {e}"),
        })?;
    if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
        stdin.write_all(input.as_bytes()).map_err(|e| format!("couldn't write to `{program}`: {e}"))?;
    }
    // Read output on threads so a chatty tool can't fill its pipe and stall.
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let out = std::thread::spawn(move || {
        let mut s = Vec::new();
        stdout.as_mut().map(|o| o.read_to_end(&mut s));
        s
    });
    let err = std::thread::spawn(move || {
        let mut s = Vec::new();
        stderr.as_mut().map(|o| o.read_to_end(&mut s));
        s
    });
    let deadline = Instant::now() + TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("`{program}` didn't answer within {}s", TIMEOUT.as_secs()));
            }
            Err(e) => return Err(format!("couldn't wait for `{program}`: {e}")),
        }
    };
    let text =
        |h: std::thread::JoinHandle<Vec<u8>>| String::from_utf8_lossy(&h.join().unwrap_or_default()).into_owned();
    let (out, err) = (text(out), text(err));
    Ok(if status.success() { Ran::Ok(out) } else { Ran::Failed(err.trim().to_owned()) })
}

/// Whether a program is on `PATH`.
#[cfg(target_os = "linux")]
pub fn exists(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
}
