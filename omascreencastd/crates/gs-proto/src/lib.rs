//! GameStream wire fundamentals.
//!
//! This crate holds only things that are literally on the wire: numeric
//! constants the client filters or branches on, endian-correct helpers, a small
//! declarative macro for fixed-layout structs, and the JSON/XML data-transfer
//! objects exchanged over HTTPS.
//!
//! It deliberately knows nothing about sockets, TLS, sessions, or Hyprland, so
//! it can be exercised against captured traces with no I/O.
//!
//! # Clean-room note
//!
//! Every constant here is either fixed by public Moonlight documentation, taken
//! from a trace captured by this project, or chosen by this project (for
//! example the D14 codec allow-list). Nothing is transcribed from Sunshine or
//! `moonlight-common-c`; see `THIRD_PARTY_NOTICES.md`.

#![deny(missing_docs)]

pub mod constants;
pub mod dto;
pub mod endian;
pub mod ipc;
pub mod packed;
pub mod wire;
pub mod xml;

pub use constants::*;
pub use endian::Endian;
pub use ipc::IpcEvent;
pub use packed::{PackedError, PackedField};
