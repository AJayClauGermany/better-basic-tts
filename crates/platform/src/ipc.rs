//! One running instance, remote-controlled from the command line.
//!
//! The first instance listens on a per-user local socket (a Unix domain
//! socket, or a named pipe on Windows). Later invocations such as
//! `better-basic-tts speak-selection` connect, send one request and exit.
//! This is also how shortcuts work on Wayland, where applications cannot
//! register global shortcuts themselves: the desktop's own shortcut settings
//! run the command.
//!
//! Wire format: the first line is the verb; for `speak` the rest of the
//! stream is the text. The listener only ever *reads*.

use anyhow::{anyhow, Context, Result};
use interprocess::local_socket::{prelude::*, ListenerOptions, Name, Stream};
use std::io::{Read, Write};

/// Upper bound for one request (the text of a long document fits easily).
const MAX_REQUEST_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Bring the control window to the front.
    Show,
    SpeakSelection,
    SpeakClipboard,
    /// Read this text.
    Speak(String),
    PauseResume,
    Stop,
    Previous,
    Next,
    Slower,
    Faster,
}

impl Request {
    fn encode(&self) -> Vec<u8> {
        let verb = match self {
            Request::Show => "show",
            Request::SpeakSelection => "speak-selection",
            Request::SpeakClipboard => "speak-clipboard",
            Request::Speak(_) => "speak",
            Request::PauseResume => "pause",
            Request::Stop => "stop",
            Request::Previous => "previous",
            Request::Next => "next",
            Request::Slower => "slower",
            Request::Faster => "faster",
        };
        let mut out = verb.as_bytes().to_vec();
        out.push(b'\n');
        if let Request::Speak(text) = self {
            out.extend_from_slice(text.as_bytes());
        }
        out
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let text = String::from_utf8_lossy(bytes);
        let (verb, rest) = text.split_once('\n').unwrap_or((&text, ""));
        Some(match verb.trim() {
            "show" => Request::Show,
            "speak-selection" => Request::SpeakSelection,
            "speak-clipboard" => Request::SpeakClipboard,
            "speak" => Request::Speak(rest.to_string()),
            "pause" => Request::PauseResume,
            "stop" => Request::Stop,
            "previous" => Request::Previous,
            "next" => Request::Next,
            "slower" => Request::Slower,
            "faster" => Request::Faster,
            _ => return None,
        })
    }
}

#[cfg(unix)]
fn socket_path() -> std::path::PathBuf {
    // XDG_RUNTIME_DIR is per-user and mode 0700; the macOS temp dir is
    // per-user as well.
    let dir = dirs::runtime_dir().unwrap_or_else(std::env::temp_dir);
    let user = std::env::var("USER").unwrap_or_default();
    let user: String = user
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    dir.join(format!("better-basic-tts-{user}.sock"))
}

#[cfg(unix)]
fn socket_name() -> Result<Name<'static>> {
    use interprocess::local_socket::GenericFilePath;
    Ok(socket_path().to_fs_name::<GenericFilePath>()?.into_owned())
}

#[cfg(windows)]
fn socket_name() -> Result<Name<'static>> {
    use interprocess::local_socket::GenericNamespaced;
    let user = std::env::var("USERNAME").unwrap_or_default();
    let user: String = user.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    Ok(format!("better-basic-tts-{user}")
        .to_ns_name::<GenericNamespaced>()?
        .into_owned())
}

/// Send a request to the running instance. Returns `Ok(false)` if no
/// instance is running.
pub fn send(request: &Request) -> Result<bool> {
    let mut stream = match Stream::connect(socket_name()?) {
        Ok(s) => s,
        Err(_) => return Ok(false),
    };
    stream
        .write_all(&request.encode())
        .context("could not talk to the running instance")?;
    stream.flush().ok();
    Ok(true)
}

/// Keeps the socket alive; dropping it removes the socket file.
pub struct Server {
    #[cfg(unix)]
    path: std::path::PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        #[cfg(unix)]
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Become the single instance. `on_request` is called from a background
/// thread for every request received.
///
/// Fails if another instance is already listening.
pub fn serve(on_request: impl Fn(Request) + Send + 'static) -> Result<Server> {
    let listener = match ListenerOptions::new().name(socket_name()?).create_sync() {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            // Either another instance is alive, or a previous one crashed and
            // left its socket file behind.
            if Stream::connect(socket_name()?).is_ok() {
                return Err(anyhow!("another instance is already running"));
            }
            #[cfg(unix)]
            {
                let _ = std::fs::remove_file(socket_path());
            }
            ListenerOptions::new()
                .name(socket_name()?)
                .create_sync()
                .context("could not create the control socket")?
        }
        Err(e) => return Err(e).context("could not create the control socket"),
    };
    std::thread::Builder::new()
        .name("bbtts-ipc".into())
        .spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { continue };
                let mut bytes = Vec::new();
                if conn.take(MAX_REQUEST_BYTES).read_to_end(&mut bytes).is_err() {
                    continue;
                }
                // The liveness probe in `serve` connects without sending.
                if bytes.is_empty() {
                    continue;
                }
                match Request::decode(&bytes) {
                    Some(request) => on_request(request),
                    None => log::warn!("ignoring unknown control request"),
                }
            }
        })
        .context("could not start the control thread")?;
    Ok(Server {
        #[cfg(unix)]
        path: socket_path(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_roundtrip() {
        for request in [
            Request::Show,
            Request::SpeakSelection,
            Request::SpeakClipboard,
            Request::Speak("line one\nline <two>\n".into()),
            Request::Speak(String::new()),
            Request::PauseResume,
            Request::Stop,
            Request::Previous,
            Request::Next,
            Request::Slower,
            Request::Faster,
        ] {
            assert_eq!(Request::decode(&request.encode()), Some(request));
        }
        assert_eq!(Request::decode(b"rm -rf\n"), None);
    }

    #[cfg(unix)]
    #[test]
    fn single_instance_and_delivery() {
        // Isolate from a real running instance.
        let dir = std::env::temp_dir().join(format!("bbtts-ipc-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("XDG_RUNTIME_DIR", &dir);

        assert!(!send(&Request::Stop).unwrap(), "nothing is listening yet");

        let (tx, rx) = flume::unbounded();
        let server = serve(move |r| {
            let _ = tx.send(r);
        })
        .unwrap();
        assert!(serve(|_| {}).is_err(), "second instance must be refused");

        assert!(send(&Request::Speak("hello <world>".into())).unwrap());
        assert!(send(&Request::Next).unwrap());
        let timeout = std::time::Duration::from_secs(5);
        assert_eq!(
            rx.recv_timeout(timeout).unwrap(),
            Request::Speak("hello <world>".into())
        );
        assert_eq!(rx.recv_timeout(timeout).unwrap(), Request::Next);

        drop(server);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
