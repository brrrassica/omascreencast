//! Small filesystem helpers shared by the identity and the trust store.
//!
//! Both persistent files have the same crash-safety requirement: a reader must
//! see either the old complete file or the new complete file, never a partial
//! one. Both also hold material whose permissions matter (`key.pem` is `0600`,
//! `clients.json` is `0600` because it lists trusted devices). Keeping the
//! logic in one place means the two files cannot drift apart in how they are
//! written.

use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::Path;

/// Write `contents` to `path` atomically, with an explicit file mode.
///
/// The bytes go to a sibling temporary file which is fsynced, chmodded, and
/// then renamed over the destination. A crash at any point leaves either the
/// previous file or the new one; it cannot leave a truncated file in place.
///
/// The mode is set explicitly after creation as well as at `create_new` time,
/// so a permissive umask cannot widen a key file.
///
/// # Errors
///
/// Propagates any I/O failure from creating the directory, writing, syncing,
/// chmodding, or renaming.
pub fn write_atomic(path: &Path, contents: &[u8], mode: u32) -> io::Result<()> {
    let file_name = path
        .file_name()
        .ok_or_else(|| io::Error::other("path has no file name"))?;
    let temp = path.with_file_name(format!(
        ".{}.tmp-{}",
        file_name.to_string_lossy(),
        std::process::id()
    ));
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&temp)?;
        file.write_all(contents)?;
        file.sync_all()?;
    }
    fs::set_permissions(&temp, fs::Permissions::from_mode(mode))?;
    fs::rename(&temp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_the_expected_bytes_and_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("value.json");
        write_atomic(&path, b"{\"a\":1}", 0o600).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"{\"a\":1}");
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn replaces_an_existing_file_without_leaving_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("value.json");
        write_atomic(&path, b"old", 0o600).unwrap();
        write_atomic(&path, b"new", 0o600).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "a temporary file was left behind");
    }
}
