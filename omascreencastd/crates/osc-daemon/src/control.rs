//! The local control socket.
//!
//! # Why a second socket
//!
//! The event socket is a **one-way** notification stream so the panel can
//! follow state without being able to command the daemon. Pairing approval is
//! different: it is an operator action and it must never be reachable by a
//! remote client, or a client could approve itself. So approvals travel on a
//! separate Unix socket that only a local process can reach, and its file mode
//! is `0600` for the same reason as the event socket.
//!
//! # Protocol
//!
//! One request line (a [`IpcCommand`] JSON object), one reply line (a
//! [`CommandReply`]). The daemon closes the connection after replying, so a
//! caller does not have to track connection state. This is a stop-and-wait
//! protocol on purpose: the command rate is a human typing a PIN.
//!
//! # Execution
//!
//! The listener owns the socket and the accept loop; it hands each parsed
//! command to a [`CommandHandler`]. The handler is a closure so the protocol
//! (parsing, limits, one-line framing) stays testable without a daemon.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use gs_proto::ipc::{CommandReply, IpcCommand};

/// How long the accept loop sleeps between polls while shutting down.
pub const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Largest command line accepted, so a runaway writer cannot exhaust memory.
pub const MAX_COMMAND_LINE: usize = 64 * 1024;

/// How long a client may take to send its command before the connection is
/// dropped.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);

/// Mode applied to the control socket file.
const SOCKET_MODE: u32 = 0o600;

/// Why the control socket could not be set up or used.
#[derive(Debug, thiserror::Error)]
pub enum ControlError {
    /// The runtime directory is unset or empty.
    #[error("cannot locate a runtime directory: set XDG_RUNTIME_DIR")]
    NoRuntimeDir,
    /// The path is already served by a live daemon.
    #[error("another daemon is already listening on {}", .0.display())]
    AlreadyRunning(PathBuf),
    /// A filesystem or socket operation failed.
    #[error("control socket: {0}")]
    Io(#[from] std::io::Error),
}

/// Resolve the control socket path from a runtime directory.
#[must_use]
pub fn control_path_for(runtime_root: &Path) -> PathBuf {
    runtime_root.join("omascreencast").join("control.sock")
}

/// Resolve the control socket path from the process environment.
///
/// # Errors
///
/// [`ControlError::NoRuntimeDir`] when `XDG_RUNTIME_DIR` is unset or empty.
pub fn control_path() -> Result<PathBuf, ControlError> {
    resolve_control_path(std::env::var_os("XDG_RUNTIME_DIR"))
}

/// Pure form of [`control_path`], so tests do not mutate process globals.
///
/// # Errors
///
/// [`ControlError::NoRuntimeDir`] for an absent or empty value.
pub fn resolve_control_path(
    runtime_dir: Option<std::ffi::OsString>,
) -> Result<PathBuf, ControlError> {
    let dir = runtime_dir
        .filter(|value| !value.is_empty())
        .ok_or(ControlError::NoRuntimeDir)?;
    Ok(control_path_for(Path::new(&dir)))
}

/// Executes one command.
pub type CommandHandler = Arc<dyn Fn(IpcCommand) -> CommandReply + Send + Sync>;

/// A bound control socket and its accept thread.
pub struct ControlServer {
    path: PathBuf,
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for ControlServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ControlServer")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl ControlServer {
    /// Bind `path` and serve commands through `handler`.
    ///
    /// A stale socket is replaced; a live one produces
    /// [`ControlError::AlreadyRunning`]. As with the event socket, liveness is
    /// decided by connecting, not by the file existing.
    ///
    /// # Errors
    ///
    /// [`ControlError`] when the socket cannot be created.
    pub fn bind(path: PathBuf, handler: CommandHandler) -> Result<Self, ControlError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        }
        if path.exists() {
            match UnixStream::connect(&path) {
                Ok(_) => return Err(ControlError::AlreadyRunning(path)),
                Err(_) => {
                    std::fs::remove_file(&path)?;
                }
            }
        }

        let listener = UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(SOCKET_MODE))?;
        listener.set_nonblocking(true)?;

        let shutdown = Arc::new(AtomicBool::new(false));
        let thread = {
            let shutdown = Arc::clone(&shutdown);
            thread::Builder::new()
                .name(String::from("osc-control"))
                .spawn(move || accept_loop(&listener, &handler, &shutdown))?
        };

        Ok(Self {
            path,
            shutdown,
            thread: Some(thread),
        })
    }

    /// The socket path this server is bound to.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Stop accepting and remove the socket file. Idempotent.
    pub fn shutdown(&mut self) {
        if self.shutdown.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Drop for ControlServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn accept_loop(listener: &UnixListener, handler: &CommandHandler, shutdown: &AtomicBool) {
    while !shutdown.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _address)) => {
                let handler = Arc::clone(handler);
                let _ = thread::Builder::new()
                    .name(String::from("osc-control-client"))
                    .spawn(move || serve_connection(stream, &handler));
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(POLL_INTERVAL);
            }
            Err(_) => thread::sleep(POLL_INTERVAL),
        }
    }
}

/// Read one command, reply once, close.
fn serve_connection(stream: UnixStream, handler: &CommandHandler) {
    let _ = stream.set_read_timeout(Some(COMMAND_TIMEOUT));
    let _ = stream.set_write_timeout(Some(COMMAND_TIMEOUT));
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let reply = match reader
        .by_ref()
        .take(MAX_COMMAND_LINE as u64 + 1)
        .read_line(&mut line)
    {
        Ok(0) => return,
        Ok(count) if count > MAX_COMMAND_LINE => CommandReply::Error {
            detail: String::from("command line too long"),
        },
        Ok(_) => match serde_json::from_str::<IpcCommand>(&line) {
            Ok(command) => handler(command),
            Err(error) => CommandReply::Error {
                detail: format!("could not parse command: {error}"),
            },
        },
        Err(error) => CommandReply::Error {
            detail: format!("could not read command: {error}"),
        },
    };

    if let Ok(mut bytes) = serde_json::to_vec(&reply) {
        bytes.push(b'\n');
        let _ = writer.write_all(&bytes);
        let _ = writer.flush();
    }
}

/// Send one command to a daemon and read its reply.
///
/// Used by the CLI subcommands; the daemon never calls this.
///
/// # Errors
///
/// [`ControlError::Io`] when the socket cannot be reached or the reply cannot
/// be read, and [`ControlError::NoRuntimeDir`] when the path cannot be
/// resolved.
pub fn send_command(path: &Path, command: &IpcCommand) -> Result<CommandReply, ControlError> {
    let stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(COMMAND_TIMEOUT))?;
    stream.set_write_timeout(Some(COMMAND_TIMEOUT))?;
    let mut writer = stream.try_clone()?;
    let mut line = serde_json::to_vec(command).unwrap_or_default();
    line.push(b'\n');
    writer.write_all(&line)?;
    writer.flush()?;

    let mut reader = BufReader::new(stream);
    let mut reply = String::new();
    reader.read_line(&mut reply)?;
    serde_json::from_str(&reply).map_err(|error| {
        ControlError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("malformed reply: {error}"),
        ))
    })
}

/// A handler that always fails, for a daemon with controls disabled.
#[must_use]
pub fn disabled_handler() -> CommandHandler {
    Arc::new(|_command| CommandReply::Error {
        detail: String::from("this daemon was started with controls disabled"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn temp_server(handler: CommandHandler) -> (tempfile::TempDir, ControlServer, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = control_path_for(dir.path());
        let server = ControlServer::bind(path.clone(), handler).unwrap();
        (dir, server, path)
    }

    #[test]
    fn a_command_round_trips_through_the_socket() {
        let handler: CommandHandler = Arc::new(|command| match command {
            IpcCommand::PairingList => CommandReply::PairingList {
                pending: vec![gs_proto::ipc::PairingRequest {
                    id: String::from("abc"),
                    name: String::from("TV"),
                    address: String::from("10.0.0.2"),
                }],
            },
            IpcCommand::PairingApprove { id, pin } => CommandReply::Ack {
                ok: pin == "1234",
                detail: Some(id),
            },
            _ => CommandReply::Error {
                detail: String::from("unexpected"),
            },
        });
        let (_dir, _server, path) = temp_server(handler);
        // The accept loop is non-blocking; retry until it is ready.
        let reply = (0..100)
            .find_map(|_| match send_command(&path, &IpcCommand::PairingList) {
                Ok(reply) => Some(reply),
                Err(_) => {
                    thread::sleep(Duration::from_millis(5));
                    None
                }
            })
            .expect("the control socket should answer");
        assert!(matches!(
            reply,
            CommandReply::PairingList { pending } if pending.len() == 1
        ));
    }

    #[test]
    fn the_socket_is_owner_only_and_removed_on_shutdown() {
        let (_dir, mut server, path) = temp_server(disabled_handler());
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, SOCKET_MODE);
        server.shutdown();
        assert!(!path.exists());
        server.shutdown();
    }

    #[test]
    fn a_live_socket_blocks_a_second_bind() {
        let (_dir, server, path) = temp_server(disabled_handler());
        let result = ControlServer::bind(path, disabled_handler());
        assert!(matches!(result, Err(ControlError::AlreadyRunning(_))));
        drop(server);
    }

    #[test]
    fn control_path_requires_a_runtime_directory() {
        assert!(matches!(
            resolve_control_path(None),
            Err(ControlError::NoRuntimeDir)
        ));
        assert_eq!(
            resolve_control_path(Some(std::ffi::OsString::from("/run/user/1000"))).unwrap(),
            PathBuf::from("/run/user/1000/omascreencast/control.sock")
        );
    }

    #[test]
    fn malformed_commands_get_an_error_reply_not_a_drop() {
        let (_dir, _server, path) = temp_server(disabled_handler());
        let mut stream = loop {
            match UnixStream::connect(&path) {
                Ok(stream) => break stream,
                Err(_) => thread::sleep(Duration::from_millis(5)),
            }
        };
        stream.write_all(b"{\"command\":\"nope\"}\n").unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let reply: CommandReply = serde_json::from_str(&line).unwrap();
        assert!(matches!(reply, CommandReply::Error { .. }));
    }

    #[test]
    fn a_dead_socket_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = control_path_for(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        {
            let stale = UnixListener::bind(&path).unwrap();
            drop(stale);
        }
        let server = ControlServer::bind(path.clone(), disabled_handler()).unwrap();
        assert_eq!(server.path(), path);
    }

    #[test]
    fn a_mutex_guard_is_not_required_to_share_the_handler() {
        // The handler must be callable concurrently from the accept loop.
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_clone = Arc::clone(&seen);
        let handler: CommandHandler = Arc::new(move |command| {
            seen_clone.lock().unwrap().push(command);
            CommandReply::Ack {
                ok: true,
                detail: None,
            }
        });
        handler(IpcCommand::Status);
        assert_eq!(seen.lock().unwrap().len(), 1);
    }
}
