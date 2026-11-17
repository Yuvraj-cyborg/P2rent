//! Positional file I/O that does not depend on a shared cursor, so chunks can
//! be read and written from many threads at once.

use model::artifact::{EntryKind, EntryMetadata, PERMISSION_BITS, Timestamp};
use std::fs::File;
use std::io;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(unix)]
pub(crate) fn read_exact_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, buf, offset)
}

#[cfg(unix)]
pub(crate) fn write_all_at(file: &File, buf: &[u8], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(file, buf, offset)
}

#[cfg(windows)]
pub(crate) fn read_exact_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    in_parts(buf.len(), io::ErrorKind::UnexpectedEof, |done| {
        file.seek_read(&mut buf[done..], offset + done as u64)
    })
}

#[cfg(windows)]
pub(crate) fn write_all_at(file: &File, buf: &[u8], offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    in_parts(buf.len(), io::ErrorKind::WriteZero, |done| {
        file.seek_write(&buf[done..], offset + done as u64)
    })
}

/// Repeats a positional read or write until `len` bytes have been handled.
/// Windows may transfer fewer bytes than asked for in one call. `step` is
/// given the number of bytes done so far and returns how many more it moved;
/// moving none is reported as `stuck`.
#[cfg(windows)]
fn in_parts(
    len: usize,
    stuck: io::ErrorKind,
    mut step: impl FnMut(usize) -> io::Result<usize>,
) -> io::Result<()> {
    let mut done = 0;
    while done < len {
        match step(done) {
            Ok(0) => return Err(stuck.into()),
            Ok(n) => done += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// The permission bits recorded for a file or directory.
#[cfg(unix)]
pub(crate) fn mode_of(metadata: &std::fs::Metadata) -> u16 {
    use std::os::unix::fs::PermissionsExt;
    u16::try_from(metadata.permissions().mode() & u32::from(PERMISSION_BITS)).unwrap_or(0)
}

/// Windows has one permission to record: whether the file may be written.
#[cfg(not(unix))]
pub(crate) fn mode_of(metadata: &std::fs::Metadata) -> u16 {
    match (metadata.is_dir(), metadata.permissions().readonly()) {
        (true, _) => 0o755,
        (false, true) => 0o444,
        (false, false) => 0o644,
    }
}

/// Gives a file the recorded permission bits.
#[cfg(unix)]
pub(crate) fn set_mode(file: &File, mode: u16) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(std::fs::Permissions::from_mode(u32::from(
        mode & PERMISSION_BITS,
    )))
}

/// Makes a file read-only if nobody may write it, which is the one
/// permission Windows has.
#[cfg(not(unix))]
pub(crate) fn set_mode(file: &File, mode: u16) -> io::Result<()> {
    let mut permissions = file.metadata()?.permissions();
    permissions.set_readonly(mode & 0o222 == 0);
    file.set_permissions(permissions)
}

/// Gives the file at `path` the recorded permission bits.
#[cfg(unix)]
pub(crate) fn set_mode_at(path: &std::path::Path, mode: u16) -> io::Result<()> {
    set_mode(&File::open(path)?, mode)
}

/// Makes the file at `path` read-only if nobody may write it. It is done
/// by the file's name: Windows lets a file be opened for writing only if
/// it may be written, which is the very thing to be changed.
#[cfg(not(unix))]
pub(crate) fn set_mode_at(path: &std::path::Path, mode: u16) -> io::Result<()> {
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_readonly(mode & 0o222 == 0);
    std::fs::set_permissions(path, permissions)
}

/// When a file or directory was last modified, if the system says.
pub(crate) fn modified_of(metadata: &std::fs::Metadata) -> Option<Timestamp> {
    let time = metadata.modified().ok()?;
    Some(match time.duration_since(UNIX_EPOCH) {
        Ok(after) => Timestamp {
            seconds: i64::try_from(after.as_secs()).ok()?,
            nanos: after.subsec_nanos(),
        },
        // Before 1970: count back, keeping the nanoseconds positive.
        Err(before) => {
            let before = before.duration();
            let whole = i64::try_from(before.as_secs()).ok()?;
            match before.subsec_nanos() {
                0 => Timestamp {
                    seconds: -whole,
                    nanos: 0,
                },
                nanos => Timestamp {
                    seconds: -whole - 1,
                    nanos: 1_000_000_000 - nanos,
                },
            }
        }
    })
}

/// The recorded time as the system's own kind of time, or `None` if the
/// system cannot represent it.
pub(crate) fn system_time(time: Timestamp) -> Option<SystemTime> {
    let nanos = Duration::from_nanos(u64::from(time.nanos));
    match u64::try_from(time.seconds) {
        Ok(seconds) => UNIX_EPOCH
            .checked_add(Duration::from_secs(seconds))?
            .checked_add(nanos),
        Err(_) => UNIX_EPOCH
            .checked_sub(Duration::from_secs(time.seconds.unsigned_abs()))?
            .checked_add(nanos),
    }
}

/// Gives a directory its recorded time and permissions.
/// What is recorded of an entry, as this system keeps it: everything.
#[cfg(unix)]
pub(crate) fn as_kept(_kind: &EntryKind, recorded: EntryMetadata) -> EntryMetadata {
    recorded
}

/// What is recorded of an entry, as this system keeps it: of a file's
/// permissions whether it may be written, and of a directory nothing.
#[cfg(not(unix))]
pub(crate) fn as_kept(kind: &EntryKind, recorded: EntryMetadata) -> EntryMetadata {
    match kind {
        EntryKind::File { .. } => EntryMetadata {
            mode: if recorded.mode & 0o222 == 0 {
                0o444
            } else {
                0o644
            },
            ..recorded
        },
        EntryKind::Directory => EntryMetadata {
            mode: 0o755,
            modified: None,
        },
        EntryKind::Symlink { .. } => recorded,
    }
}

#[cfg(unix)]
pub(crate) fn restore_directory(path: &std::path::Path, metadata: EntryMetadata) -> io::Result<()> {
    let dir = File::open(path)?;
    if let Some(time) = metadata.modified.and_then(system_time) {
        dir.set_modified(time)?;
    }
    set_mode(&dir, metadata.mode)
}

/// Windows offers no way to do either through the standard library.
#[cfg(not(unix))]
pub(crate) fn restore_directory(
    _path: &std::path::Path,
    _metadata: EntryMetadata,
) -> io::Result<()> {
    Ok(())
}

/// Creates a symbolic link at `path` that points at `target`.
#[cfg(unix)]
pub(crate) fn symlink(target: &str, path: &std::path::Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, path)
}

/// Flushes a directory, so that a rename inside it survives a power cut.
#[cfg(unix)]
pub(crate) fn sync_dir(dir: &std::path::Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

/// Windows cannot flush a directory through a file handle. NTFS records a
/// rename in its journal, which serves the same purpose.
#[cfg(not(unix))]
pub(crate) fn sync_dir(_dir: &std::path::Path) -> io::Result<()> {
    Ok(())
}
