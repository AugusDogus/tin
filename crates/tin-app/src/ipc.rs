//! One Tin window per user: a second `tin-app` hands its request to the running one over a Unix
//! socket and exits. That's how a desktop shortcut bound to `tin-app --capture` reaches it.
//!
//! The protocol is one line each way: `capture <token>` or `show <token>` (the token is the
//! launcher's XDG activation token, or `-`), answered by `ok`.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    /// Read the screen and open the capture panel.
    Capture,
    /// Bring the window forward.
    Show,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Message {
    pub request: Request,
    /// Lets Wayland focus the window the request opens.
    pub activation_token: Option<String>,
}

impl Message {
    fn encode(&self) -> String {
        let verb = match self.request {
            Request::Capture => "capture",
            Request::Show => "show",
        };
        format!(
            "{verb} {}\n",
            self.activation_token.as_deref().filter(|t| !t.contains(char::is_whitespace)).unwrap_or("-")
        )
    }

    fn decode(line: &str) -> Option<Message> {
        let (verb, token) = line.trim().split_once(' ').unwrap_or((line.trim(), "-"));
        let request = match verb {
            "capture" => Request::Capture,
            "show" => Request::Show,
            _ => return None,
        };
        let activation_token = (token != "-" && !token.is_empty()).then(|| token.to_owned());
        Some(Message { request, activation_token })
    }
}

/// `$XDG_RUNTIME_DIR/tin.sock`, else a per-user file in the temp folder.
pub fn socket_path() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) => PathBuf::from(dir).join("tin.sock"),
        None => {
            let user = std::env::var("USER").unwrap_or_else(|_| "user".into());
            std::env::temp_dir().join(format!("tin-{user}.sock"))
        }
    }
}

/// Sends the message to a running Tin. `Ok(false)` means none is running.
pub fn send(message: &Message) -> std::io::Result<bool> {
    let mut stream = match UnixStream::connect(socket_path()) {
        Ok(s) => s,
        Err(e) if matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused) => {
            return Ok(false);
        }
        Err(e) => return Err(e),
    };
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.write_all(message.encode().as_bytes())?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply)?;
    Ok(reply.trim() == "ok")
}

/// Listens for other instances' messages on a thread, calling `on_message` for each.
pub fn listen(on_message: impl Fn(Message) + Send + 'static) -> std::io::Result<()> {
    let path = socket_path();
    // A socket left by a Tin that crashed: nothing answers it (`send` said so), so replace it.
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;
    std::thread::Builder::new().name("tin-ipc".into()).spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut line = String::new();
            let mut reader = BufReader::new(&stream);
            if reader.read_line(&mut line).is_err() {
                continue;
            }
            let reply: &[u8] = match Message::decode(&line) {
                Some(message) => {
                    on_message(message);
                    b"ok\n"
                }
                None => b"unknown request\n",
            };
            let _ = (&stream).write_all(reply);
        }
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_round_trip_and_drop_unsafe_tokens() {
        let m = Message { request: Request::Capture, activation_token: Some("abc_1".into()) };
        assert_eq!(Message::decode(&m.encode()), Some(m));
        let spaced = Message { request: Request::Show, activation_token: Some("a b".into()) };
        assert_eq!(Message::decode(&spaced.encode()), Some(Message { request: Request::Show, activation_token: None }));
        assert_eq!(Message::decode("show"), Some(Message { request: Request::Show, activation_token: None }));
        assert_eq!(Message::decode("rm -rf /"), None);
    }
}
