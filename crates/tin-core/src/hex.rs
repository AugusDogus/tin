//! Voice notes through Hex (github.com/anomalyco/hex), the on-device dictation app.
//!
//! Tin drives the running Hex with its control CLI: `hex dictation start` opens the microphone,
//! and `hex dictation stop --print` ends the recording and hands the transcript back to Tin
//! instead of pasting it. Audio and transcription never leave the machine.

use std::process::{Command, Output};

use serde::Deserialize;

use crate::capture::VoiceNote;

#[derive(Debug, Clone)]
pub struct Hex {
    bin: String,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum HexError {
    #[error(
        "Tin couldn't run Hex (`{bin}`): {reason}. Voice notes need Hex: install it from \
         https://github.com/anomalyco/hex, or set `[hex] bin` in Tin's config. The capture itself still saves."
    )]
    NotInstalled { bin: String, reason: String },
    #[error("Hex isn't running, so there's no voice note. Start it with `hex start` and try again.")]
    NotRunning,
    #[error("Hex heard no speech, so there's no voice note.")]
    NoSpeech,
    #[error("Hex refused `{action}` ({code}): {message}")]
    Refused { action: &'static str, code: String, message: String },
    #[error("Hex answered `{action}` with something Tin doesn't understand (exit {exit}): {output}")]
    Unexpected { action: &'static str, exit: String, output: String },
}

#[derive(Deserialize)]
struct Reply {
    status: String,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    text: Option<String>,
}

impl Hex {
    pub fn new(bin: impl Into<String>) -> Hex {
        Hex { bin: bin.into() }
    }

    /// Starts recording; it keeps going until [`Hex::stop`] or [`Hex::cancel`].
    pub fn start(&self) -> Result<(), HexError> {
        self.run("dictation start", &["dictation", "start"]).map(|_| ())
    }

    /// Stops recording and returns the transcript.
    pub fn stop(&self) -> Result<VoiceNote, HexError> {
        let reply = self.run("dictation stop --print", &["dictation", "stop", "--print"])?;
        match reply.text.filter(|t| !t.trim().is_empty()) {
            Some(text) => Ok(VoiceNote { transcript: text.trim().to_owned(), engine: "hex".into() }),
            None => Err(HexError::NoSpeech),
        }
    }

    /// Discards the recording.
    pub fn cancel(&self) -> Result<(), HexError> {
        self.run("dictation cancel", &["dictation", "cancel"]).map(|_| ())
    }

    fn run(&self, action: &'static str, args: &[&str]) -> Result<Reply, HexError> {
        let output = Command::new(&self.bin)
            .args(args)
            .arg("--json")
            .output()
            .map_err(|e| HexError::NotInstalled { bin: self.bin.clone(), reason: e.to_string() })?;
        interpret(action, &output)
    }
}

fn interpret(action: &'static str, output: &Output) -> Result<Reply, HexError> {
    let code = output.status.code();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let unexpected = || HexError::Unexpected {
        action,
        exit: code.map_or("by signal".into(), |c| c.to_string()),
        output: format!("{}{}", stdout.trim(), String::from_utf8_lossy(&output.stderr).trim()),
    };
    // Exit codes are Hex's stable contract (docs/cli.md): 3 not running, 4 no speech.
    match code {
        Some(3) => return Err(HexError::NotRunning),
        Some(4) => return Err(HexError::NoSpeech),
        _ => {}
    }
    let reply: Reply = serde_json::from_str(stdout.trim()).map_err(|_| unexpected())?;
    match (code, reply.status.as_str()) {
        (_, "error") => match reply.code.as_deref() {
            Some("not-running") => Err(HexError::NotRunning),
            Some("no-speech") => Err(HexError::NoSpeech),
            other => Err(HexError::Refused {
                action,
                code: other.unwrap_or("unknown").to_owned(),
                message: reply.message.unwrap_or_default(),
            }),
        },
        (Some(0), _) => Ok(reply),
        _ => Err(unexpected()),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;

    use super::*;

    fn output(code: i32, stdout: &str) -> Output {
        Output { status: ExitStatus::from_raw(code << 8), stdout: stdout.into(), stderr: vec![] }
    }

    #[test]
    fn a_transcript_reply_is_a_voice_note() {
        let reply = interpret("stop", &output(0, r#"{"version": 4, "status": "transcript", "text": " Ship it. "}"#));
        assert_eq!(reply.ok().and_then(|r| r.text), Some(" Ship it. ".into()));
    }

    #[test]
    fn exit_codes_and_error_replies_map_to_errors() {
        assert_eq!(interpret("stop", &output(3, "")).err(), Some(HexError::NotRunning));
        assert_eq!(interpret("stop", &output(4, "")).err(), Some(HexError::NoSpeech));
        let refused = r#"{"version": 4, "status": "error", "code": "not-recording", "message": "nothing"}"#;
        assert_eq!(
            interpret("stop", &output(1, refused)).err(),
            Some(HexError::Refused { action: "stop", code: "not-recording".into(), message: "nothing".into() })
        );
        assert!(matches!(interpret("stop", &output(1, "boom")), Err(HexError::Unexpected { .. })));
    }

    #[test]
    fn a_missing_binary_says_how_to_get_hex() {
        let err = Hex::new("/nonexistent/hex").start().err();
        assert!(matches!(err, Some(HexError::NotInstalled { .. })), "{err:?}");
    }
}
