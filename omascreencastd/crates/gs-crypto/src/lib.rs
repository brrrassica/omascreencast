//! Server identity and (from M1-T4/T8 onwards) pairing cryptography and the
//! trust store.
//!
//! # Identity model
//!
//! GameStream pairing is anchored on the host's self-signed certificate: the
//! client fetches it during `/getservercert`, trusts it once the operator
//! enters the PIN, and afterwards presents **its own** client certificate on
//! every HTTPS request. That means:
//!
//! - the host identity must be **stable across restarts** — regenerating it
//!   silently would invalidate every paired client;
//! - the private key is a secret and is stored `0600`;
//! - the public certificate is not a secret and is served to unpaired clients.
//!
//! Storage location follows D9 and never lives in the plugin folder.
//!
//! # Clean-room note
//!
//! Everything here is built from the published OpenSSL API and the
//! documented GameStream exchange. No Sunshine or `moonlight-common-c` code is
//! transcribed; see `THIRD_PARTY_NOTICES.md`.

#![deny(missing_docs)]

pub mod identity;
pub mod paths;

pub use identity::{Identity, IdentityError};
pub use paths::{IdentityPaths, PathError, data_dir, identity_paths, paths_under};
