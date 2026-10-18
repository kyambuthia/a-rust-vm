//! Private, atomic writes for local state.
//!
//! Transcripts, permission rules, and trust records can hold code and
//! secrets, so they are written owner-only: directories `0700`, files `0600`.
//! A file is written to a temporary sibling and renamed into place, so a
//! crash never leaves a half-written file and readers never see one.

use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEMPORARY_ID: AtomicU64 = AtomicU64::new(0);

/// Create `directory` and any missing parents owner-only (`0700`).
///
/// Existing directories keep their mode: a state directory may be a shared
/// one such as `/tmp`, and the files inside are private either way.
pub fn create_private_dir_all(directory: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(directory)
}

/// Atomically replace `path` with `bytes`, owner-only. The parent directory
/// is created owner-only when missing.
pub fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    create_private_dir_all(parent)?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?;
    let temporary = parent.join(format!(
        ".{}.{}.{}.tmp",
        name.to_string_lossy(),
        std::process::id(),
        NEXT_TEMPORARY_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = options.open(&temporary).and_then(|mut file| {
        file.write_all(bytes)?;
        file.sync_all()
    });
    let result = result.and_then(|()| fs::rename(&temporary, path));
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn writes_owner_only_files_and_directories() {
        let root = std::env::temp_dir().join(format!("a-rvm-private-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let file = root.join("state").join("rules.json");
        write_private(&file, b"[]").unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"[]");
        assert_eq!(mode(&file), 0o600);
        assert_eq!(mode(&root.join("state")), 0o700);
        assert_eq!(mode(&root), 0o700);

        // An existing world-readable file becomes private; an existing
        // directory keeps its mode.
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        fs::set_permissions(root.join("state"), fs::Permissions::from_mode(0o755)).unwrap();
        write_private(&file, b"[1]").unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"[1]");
        assert_eq!(mode(&file), 0o600);
        assert_eq!(mode(&root.join("state")), 0o755);

        let leftovers = fs::read_dir(root.join("state"))
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp")
            })
            .count();
        assert_eq!(leftovers, 0);
        let _ = fs::remove_dir_all(&root);
    }
}
