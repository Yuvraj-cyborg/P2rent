//! Positional file I/O that does not depend on a shared cursor, so chunks can
//! be read and written from many threads at once.

use std::fs::File;
use std::io;

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

#[cfg(unix)]
pub(crate) fn is_executable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
pub(crate) fn is_executable(_metadata: &std::fs::Metadata) -> bool {
    false
}

/// Grants execute permission to everyone who can already read the file.
#[cfg(unix)]
pub(crate) fn make_executable(file: &File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = file.metadata()?.permissions();
    let mode = permissions.mode();
    permissions.set_mode(mode | ((mode & 0o444) >> 2));
    file.set_permissions(permissions)
}

#[cfg(not(unix))]
pub(crate) fn make_executable(_file: &File) -> io::Result<()> {
    Ok(())
}
