//! Daemon library surface: the IPC server and the pieces that need unit tests.
//!
//! The binary is a thin shell over this crate. Anything with non-obvious
//! behaviour — socket lifecycle, event fan-out, stale-socket handling — lives
//! here so it can be tested without spawning a process.

#![deny(missing_docs)]

pub mod ipc;

pub use ipc::{IpcError, IpcHub, IpcServer, socket_path, socket_path_for};
