//! Hooks: commands Tin starts after a capture saves, so agents can act on it right away.
//!
//! Each gets the capture's JSON on stdin and `TIN_CAPTURE_ID` / `TIN_CAPTURE_FILE` in its
//! environment. Tin doesn't wait for it; it only reports a hook that couldn't start.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::capture::Capture;
use crate::config::expand_home;

#[derive(Debug, thiserror::Error)]
#[error("the hook `{command}` couldn't start: {reason}. The capture is saved; fix the hook in Tin's config.")]
pub struct HookError {
    pub command: String,
    pub reason: String,
}

pub fn spawn(command: &[String], capture: &Capture, file: &Path) -> Result<(), HookError> {
    let fail = |reason: String| HookError { command: command.join(" "), reason };
    let (program, args) = command.split_first().ok_or_else(|| fail("it's empty".into()))?;
    let json = serde_json::to_string(capture).map_err(|e| fail(e.to_string()))?;
    let mut child = Command::new(expand_home(program))
        .args(args.iter().map(|a| a.strip_prefix("~/").map_or(a.into(), |_| expand_home(a).into_os_string())))
        .env("TIN_CAPTURE_ID", capture.id.as_str())
        .env("TIN_CAPTURE_FILE", file)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .map_err(|e| fail(e.to_string()))?;
    let stdin = child.stdin.take();
    // Feed and reap it off the caller's thread; a hook may run for minutes.
    std::thread::spawn(move || {
        if let Some(mut stdin) = stdin {
            let _ = stdin.write_all(json.as_bytes());
        }
        let _ = child.wait();
    });
    Ok(())
}
