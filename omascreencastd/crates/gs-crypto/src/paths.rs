//! On-disk locations for persistent state (D9).
//!
//! Everything lives under `$XDG_DATA_HOME/omascreencast/gamestream/`, which
//! resolves to `~/.local/share/omascreencast/gamestream/` when `XDG_DATA_HOME`
//! is unset, matching the XDG base-directory specification. Nothing is written
//! inside the plugin folder: a plugin update replaces that tree.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Resolved paths for the daemon's persistent state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityPaths {
    /// Directory holding all persistent state.
    pub dir: PathBuf,
    /// `cert.pem`: the server certificate, world-readable.
    pub cert: PathBuf,
    /// `key.pem`: the matching private key, owner-only.
    pub key: PathBuf,
    /// `clients.json`: the paired-client trust store (M1-T8).
    pub clients: PathBuf,
    /// `daemon.json`: the host's stable identity and settings (M1-T6).
    pub host: PathBuf,
}

impl IdentityPaths {
    /// Resolve paths under a specific data directory.
    #[must_use]
    pub fn under(dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        Self {
            cert: dir.join("cert.pem"),
            key: dir.join("key.pem"),
            clients: dir.join("clients.json"),
            host: dir.join("daemon.json"),
            dir,
        }
    }
}

/// Resolve the base data directory from the process environment.
///
/// # Errors
///
/// Returns [`PathError::NoHome`] when `XDG_DATA_HOME` is unset **and** `HOME`
/// is absent, which means there is nowhere legitimate to persist state.
pub fn data_dir() -> Result<PathBuf, PathError> {
    resolve_data_dir(std::env::var_os("XDG_DATA_HOME"), std::env::var_os("HOME"))
}

/// Pure form of [`data_dir`]: resolve from explicit environment values.
///
/// Splitting this out keeps the environment out of the tests, since mutating
/// process-global variables is both unsafe in edition 2024 and racy under the
/// parallel test harness.
///
/// # Errors
///
/// Returns [`PathError::NoHome`] when neither value is usable.
pub fn resolve_data_dir(
    xdg_data_home: Option<OsString>,
    home: Option<OsString>,
) -> Result<PathBuf, PathError> {
    let base = match xdg_data_home {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => {
            let home = home
                .filter(|value| !value.is_empty())
                .ok_or(PathError::NoHome)?;
            PathBuf::from(home).join(".local").join("share")
        }
    };
    Ok(base.join("omascreencast").join("gamestream"))
}

/// Resolve the standard identity paths from the environment.
///
/// # Errors
///
/// Propagates [`data_dir`]'s failure.
pub fn identity_paths() -> Result<IdentityPaths, PathError> {
    Ok(IdentityPaths::under(data_dir()?))
}

/// Resolve identity paths relative to an explicit root, for tests and for a
/// daemon started with an overridden data directory.
#[must_use]
pub fn paths_under(root: &Path) -> IdentityPaths {
    IdentityPaths::under(root)
}

/// Why the data directory could not be resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    /// Neither `XDG_DATA_HOME` nor `HOME` is set.
    #[error("cannot locate a data directory: set XDG_DATA_HOME or HOME")]
    NoHome,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_laid_out_under_the_gamestream_directory() {
        let paths = paths_under(Path::new("/data/omascreencast/gamestream"));
        assert_eq!(
            paths.cert,
            PathBuf::from("/data/omascreencast/gamestream/cert.pem")
        );
        assert_eq!(
            paths.key,
            PathBuf::from("/data/omascreencast/gamestream/key.pem")
        );
        assert_eq!(
            paths.clients,
            PathBuf::from("/data/omascreencast/gamestream/clients.json")
        );
        assert_eq!(
            paths.host,
            PathBuf::from("/data/omascreencast/gamestream/daemon.json")
        );
    }

    #[test]
    fn xdg_data_home_wins_over_home() {
        let resolved = resolve_data_dir(
            Some(OsString::from("/xdg")),
            Some(OsString::from("/home/someone")),
        )
        .unwrap();
        assert_eq!(resolved, PathBuf::from("/xdg/omascreencast/gamestream"));
    }

    #[test]
    fn unset_or_empty_xdg_falls_back_to_home_local_share() {
        for xdg in [None, Some(OsString::new())] {
            let resolved = resolve_data_dir(xdg, Some(OsString::from("/home/someone"))).unwrap();
            assert_eq!(
                resolved,
                PathBuf::from("/home/someone/.local/share/omascreencast/gamestream")
            );
        }
    }

    #[test]
    fn no_usable_base_is_an_error() {
        assert_eq!(resolve_data_dir(None, None), Err(PathError::NoHome));
        assert_eq!(
            resolve_data_dir(Some(OsString::new()), None),
            Err(PathError::NoHome)
        );
        assert_eq!(
            resolve_data_dir(None, Some(OsString::new())),
            Err(PathError::NoHome)
        );
    }
}
