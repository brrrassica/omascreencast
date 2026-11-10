//! The daemon's Unix-socket event stream (M1-T10).
//!
//! # Contract
//!
//! - Socket path: `${XDG_RUNTIME_DIR}/omascreencast/daemon.sock`.
//! - Transport: newline-delimited JSON, one [`IpcEvent`] per line, as in plan
//!   §5.1.
//! - Direction: **daemon to panel only.** The socket is a notification stream,
//!   not a request/response API; commands go over HTTPS so they stay behind the
//!   same pairing checks as everything else. A subscriber that writes is
//!   ignored.
//!
//! # Lifecycle decisions worth naming
//!
//! - **`XDG_RUNTIME_DIR`, not `XDG_DATA_HOME`.** A socket is runtime state: it
//!   is meaningless after a reboot, it must not survive into a backup, and the
//!   runtime directory is already user-private (`0700`) on a correct system.
//!   There is deliberately no fallback to a data directory — a socket in a
//!   world-traversable path is a local denial-of-service target.
//! - **Stale sockets are removed, live ones are respected.** After a crash the
//!   socket file remains but nothing is listening. Detecting that by connecting
//!   rather than by assuming is what makes restart-after-crash work without a
//!   manual `rm`.
//! - **The socket file is `0600`.** The runtime directory is usually already
//!   private, but a daemon should not depend on the system being configured
//!   correctly.
//! - **A slow or dead subscriber never blocks the daemon.** Fan-out uses
//!   bounded channels and drops a subscriber that is not reading. The daemon
//!   must not stall because a panel process went to sleep.
//!
//! # Why polling instead of an async runtime
//!
//! The M1 scope does not need async, and pulling in a runtime for one accept
//! loop would be a large dependency for a small job. The listener is
//! non-blocking and checks a shutdown flag between accepts; the latency that
//! adds is bounded by [`POLL_INTERVAL`] and only matters at shutdown.

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use gs_proto::ipc::IpcEvent;

/// How long the accept loop sleeps between polls while shutting down.
pub const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Mode applied to the socket file.
const SOCKET_MODE: u32 = 0o600;

/// Mode applied to the directory holding the socket.
const DIR_MODE: u32 = 0o700;

/// How many queued events a subscriber may fall behind before it is dropped.
///
/// Kept small on purpose: a subscriber this far behind has stopped reading, and
/// buffering more would convert a panel bug into daemon memory growth.
pub const SUBSCRIBER_QUEUE: usize = 64;

/// Why the IPC socket could not be set up or used.
#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    /// `XDG_RUNTIME_DIR` is unset or empty, so there is nowhere safe to put a
    /// socket.
    #[error("cannot locate a runtime directory: set XDG_RUNTIME_DIR")]
    NoRuntimeDir,
    /// The path is already served by a live daemon.
    #[error("another daemon is already listening on {}", .0.display())]
    AlreadyRunning(PathBuf),
    /// A filesystem or socket operation failed.
    #[error("ipc: {0}")]
    Io(#[from] std::io::Error),
}

/// Resolve the socket path from the process environment.
///
/// # Errors
///
/// Returns [`IpcError::NoRuntimeDir`] when `XDG_RUNTIME_DIR` is unset or empty.
pub fn socket_path() -> Result<PathBuf, IpcError> {
    resolve_socket_path(std::env::var_os("XDG_RUNTIME_DIR"))
}

/// Pure form of [`socket_path`], so tests do not mutate process globals.
///
/// # Errors
///
/// Returns [`IpcError::NoRuntimeDir`] for an absent or empty value.
pub fn resolve_socket_path(runtime_dir: Option<std::ffi::OsString>) -> Result<PathBuf, IpcError> {
    let dir = runtime_dir
        .filter(|value| !value.is_empty())
        .ok_or(IpcError::NoRuntimeDir)?;
    Ok(PathBuf::from(dir).join("omascreencast").join("daemon.sock"))
}

/// The socket path under an explicit runtime root, for tests.
#[must_use]
pub fn socket_path_for(runtime_root: &Path) -> PathBuf {
    runtime_root.join("omascreencast").join("daemon.sock")
}

/// Fans an event out to every connected subscriber.
///
/// Cheap to clone; every clone refers to the same subscriber set.
#[derive(Debug, Clone, Default)]
pub struct IpcHub {
    subscribers: Arc<Mutex<Vec<SyncSender<IpcEvent>>>>,
}

impl IpcHub {
    /// An empty hub with no subscribers.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a subscriber, returning the receiving end.
    ///
    /// The receiver is disconnected when the daemon shuts down or when the
    /// subscriber falls more than [`SUBSCRIBER_QUEUE`] events behind.
    #[must_use]
    pub fn subscribe(&self) -> Receiver<IpcEvent> {
        // A bounded channel is what makes `try_send` meaningful: an unbounded
        // queue would accept forever and turn a stuck panel into daemon memory
        // growth, which the drop-a-slow-subscriber rule exists to prevent.
        let (sender, receiver) = mpsc::sync_channel(SUBSCRIBER_QUEUE);
        if let Ok(mut subscribers) = self.subscribers.lock() {
            subscribers.push(sender);
        }
        receiver
    }

    /// Send an event to every subscriber that is still reading.
    ///
    /// A subscriber whose queue is full is dropped rather than waited on, for
    /// the reason in the module note. If the lock is poisoned the event is
    /// dropped: a panic while holding it means the daemon is already in a bad
    /// state and must not block the protocol path.
    pub fn emit(&self, event: &IpcEvent) {
        let Ok(mut subscribers) = self.subscribers.lock() else {
            return;
        };
        subscribers.retain(|sender| match sender.try_send(event.clone()) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => false,
            Err(TrySendError::Disconnected(_)) => false,
        });
    }

    /// How many subscribers are currently attached.
    #[must_use]
    pub fn subscriber_count(&self) -> usize {
        self.subscribers
            .lock()
            .map(|subscribers| subscribers.len())
            .unwrap_or(0)
    }

    /// Drop every subscriber, waking their readers.
    pub fn close(&self) {
        if let Ok(mut subscribers) = self.subscribers.lock() {
            subscribers.clear();
        }
    }
}

/// A bound, listening Unix socket and its accept thread.
///
/// Dropping the server shuts it down and removes the socket file, so a crash
/// does not leave a socket that the next start has to dislodge. The explicit
/// [`Self::shutdown`] is the deterministic path; `Drop` is the safety net.
pub struct IpcServer {
    path: PathBuf,
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    hub: IpcHub,
}

impl std::fmt::Debug for IpcServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IpcServer")
            .field("path", &self.path)
            .field("subscribers", &self.hub.subscriber_count())
            .finish_non_exhaustive()
    }
}

impl IpcServer {
    /// Bind the socket at `path` and start accepting.
    ///
    /// A stale socket left by a crashed daemon is removed; a socket that is
    /// actually being served produces [`IpcError::AlreadyRunning`]. The
    /// distinction is made by attempting a connection, not by trusting the
    /// file's existence.
    ///
    /// # Errors
    ///
    /// - [`IpcError::AlreadyRunning`] when a live daemon owns the path;
    /// - [`IpcError::Io`] when the directory or socket cannot be created.
    pub fn bind(path: PathBuf, hub: IpcHub) -> Result<Self, IpcError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
            // Set the mode explicitly; a pre-existing directory must not be
            // assumed private.
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(DIR_MODE))?;
        }

        if path.exists() {
            match UnixStream::connect(&path) {
                // Something answered: refuse to steal a live socket.
                Ok(_) => return Err(IpcError::AlreadyRunning(path)),
                // Nothing is listening: the file is stale and safe to replace.
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
            let hub = hub.clone();
            thread::Builder::new()
                .name(String::from("osc-ipc"))
                .spawn(move || accept_loop(&listener, &hub, &shutdown))?
        };

        Ok(Self {
            path,
            shutdown,
            thread: Some(thread),
            hub,
        })
    }

    /// The socket path this server is bound to.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The hub events are published through.
    #[must_use]
    pub fn hub(&self) -> &IpcHub {
        &self.hub
    }

    /// Stop accepting, disconnect subscribers, and remove the socket file.
    ///
    /// Idempotent: calling it twice is harmless.
    pub fn shutdown(&mut self) {
        if self.shutdown.swap(true, Ordering::SeqCst) {
            return;
        }
        self.hub.close();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        // Removing the file is best effort: if it is already gone, the goal is
        // met.
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Accept connections until the shutdown flag is set.
fn accept_loop(listener: &UnixListener, hub: &IpcHub, shutdown: &AtomicBool) {
    while !shutdown.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _address)) => {
                let receiver = hub.subscribe();
                // A subscriber that fails to start is not fatal; the daemon
                // keeps serving the others.
                let _ = thread::Builder::new()
                    .name(String::from("osc-ipc-client"))
                    .spawn(move || serve_connection(stream, receiver));
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(POLL_INTERVAL);
            }
            Err(_) => {
                // The listener is unusable. Sleeping rather than spinning keeps
                // a persistent failure from pinning a core; shutdown still
                // works because the flag is checked each iteration.
                thread::sleep(POLL_INTERVAL);
            }
        }
    }
}

/// Write one JSONL line per event until the subscriber goes away.
///
/// Input from the panel is drained and discarded so a chatty client cannot fill
/// a kernel buffer and stall the daemon while it writes.
fn serve_connection(stream: UnixStream, receiver: Receiver<IpcEvent>) {
    let mut writer = match stream.try_clone() {
        Ok(writer) => writer,
        Err(_) => return,
    };
    let _reader = thread::Builder::new()
        .name(String::from("osc-ipc-drain"))
        .spawn(move || {
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            while matches!(reader.read_line(&mut line), Ok(count) if count > 0) {
                line.clear();
            }
        });

    while let Ok(event) = receiver.recv() {
        let Ok(mut line) = serde_json::to_vec(&event) else {
            continue;
        };
        line.push(b'\n');
        if writer.write_all(&line).is_err() || writer.flush().is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gs_proto::ipc::phase;

    fn temp_server() -> (tempfile::TempDir, IpcServer, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = socket_path_for(dir.path());
        let hub = IpcHub::new();
        let server = IpcServer::bind(path.clone(), hub).unwrap();
        (dir, server, path)
    }

    fn read_one_line(stream: &UnixStream) -> IpcEvent {
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }

    #[test]
    fn socket_path_requires_a_runtime_directory() {
        assert!(matches!(
            resolve_socket_path(None),
            Err(IpcError::NoRuntimeDir)
        ));
        assert!(matches!(
            resolve_socket_path(Some(std::ffi::OsString::new())),
            Err(IpcError::NoRuntimeDir)
        ));
        assert_eq!(
            resolve_socket_path(Some(std::ffi::OsString::from("/run/user/1000"))).unwrap(),
            PathBuf::from("/run/user/1000/omascreencast/daemon.sock")
        );
    }

    #[test]
    fn binding_creates_a_private_socket() {
        let (_dir, server, path) = temp_server();
        assert!(path.exists());
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, SOCKET_MODE);
        let parent_mode = std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(parent_mode, DIR_MODE);
        assert_eq!(server.path(), path);
    }

    #[test]
    fn a_subscriber_receives_emitted_events_as_jsonl() {
        let (_dir, server, path) = temp_server();
        let stream = UnixStream::connect(&path).unwrap();
        // Give the accept loop a moment to attach the subscriber, then emit a
        // few events; ordering within a single connection is guaranteed.
        let hub = server.hub().clone();
        let handle = thread::spawn(move || {
            for _ in 0..50 {
                if hub.subscriber_count() > 0 {
                    break;
                }
                thread::sleep(Duration::from_millis(5));
            }
            hub.emit(&IpcEvent::session_phase(
                "HEADLESS-1",
                phase::CASTING,
                Some(String::from("PLAY")),
            ));
            hub.emit(&IpcEvent::PairingResult {
                id: String::from("abc"),
                ok: true,
            });
        });

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let first: IpcEvent = serde_json::from_str(&line).unwrap();
        assert_eq!(
            first,
            IpcEvent::session_phase("HEADLESS-1", phase::CASTING, Some(String::from("PLAY")))
        );
        line.clear();
        reader.read_line(&mut line).unwrap();
        let second: IpcEvent = serde_json::from_str(&line).unwrap();
        assert_eq!(
            second,
            IpcEvent::PairingResult {
                id: String::from("abc"),
                ok: true
            }
        );
        handle.join().unwrap();
    }

    #[test]
    fn an_event_reaches_every_subscriber() {
        let (_dir, server, path) = temp_server();
        let first = UnixStream::connect(&path).unwrap();
        let second = UnixStream::connect(&path).unwrap();
        let hub = server.hub().clone();
        thread::spawn(move || {
            for _ in 0..50 {
                if hub.subscriber_count() >= 2 {
                    break;
                }
                thread::sleep(Duration::from_millis(5));
            }
            hub.emit(&IpcEvent::phase(phase::DISCONNECTED));
        });

        assert_eq!(read_one_line(&first), IpcEvent::phase(phase::DISCONNECTED));
        assert_eq!(read_one_line(&second), IpcEvent::phase(phase::DISCONNECTED));
    }

    #[test]
    fn a_dead_subscriber_is_dropped_rather_than_stalling_the_hub() {
        let hub = IpcHub::new();
        let receiver = hub.subscribe();
        drop(receiver);
        // Must not panic and must not keep the closed subscriber.
        hub.emit(&IpcEvent::phase(phase::STARTING));
        assert_eq!(hub.subscriber_count(), 0);
    }

    #[test]
    fn a_full_subscriber_queue_drops_that_subscriber() {
        let hub = IpcHub::new();
        let _receiver = hub.subscribe();
        // Never read from the receiver; overflow the queue.
        for _ in 0..SUBSCRIBER_QUEUE + 1 {
            hub.emit(&IpcEvent::phase(phase::CASTING));
        }
        assert_eq!(hub.subscriber_count(), 0);
    }

    #[test]
    fn a_live_socket_blocks_a_second_bind() {
        let (_dir, server, path) = temp_server();
        let result = IpcServer::bind(path.clone(), IpcHub::new());
        assert!(matches!(result, Err(IpcError::AlreadyRunning(_))));
        drop(server);
    }

    #[test]
    fn a_stale_socket_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = socket_path_for(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Create a socket file, then drop the listener: the file remains but
        // nothing accepts on it, which is exactly the post-crash state.
        {
            let stale = UnixListener::bind(&path).unwrap();
            drop(stale);
        }
        assert!(path.exists());
        let server = IpcServer::bind(path.clone(), IpcHub::new()).unwrap();
        assert_eq!(server.path(), path);
    }

    #[test]
    fn shutdown_removes_the_socket_file_and_disconnects_subscribers() {
        let (_dir, mut server, path) = temp_server();
        let receiver = server.hub().subscribe();
        server.shutdown();
        assert!(!path.exists());
        assert!(receiver.recv().is_err(), "subscribers must be closed");
        // Idempotent.
        server.shutdown();
    }

    #[test]
    fn dropping_the_server_removes_the_socket_file() {
        let (_dir, server, path) = temp_server();
        let path_clone = path.clone();
        drop(server);
        assert!(!path_clone.exists());
    }
}
