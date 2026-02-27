//! Safe, ergonomic Unix socket ancillary data (SCM_RIGHTS fd passing).
//!
//! This crate provides a safe Rust API for sending and receiving file descriptors
//! over Unix domain sockets using the `SCM_RIGHTS` ancillary data mechanism.
//!
//! # Key Design Principles
//!
//! - **No `RawFd` in public API** — uses `OwnedFd` and `BorrowedFd` throughout
//! - **Automatic cleanup** — received FDs are `OwnedFd`, closed on drop
//! - **macOS fd-leak protection** — handles kernel truncation edge cases
//!
//! # Quick Start
//!
//! ```no_run
//! use std::os::unix::net::UnixStream;
//! use unix_ancillary::UnixStreamExt;
//!
//! let (tx, rx) = UnixStream::pair().unwrap();
//!
//! let file = std::fs::File::open("/dev/null").unwrap();
//! tx.send_fds(b"hello", &[&file]).unwrap();
//!
//! let (n, data, fds) = rx.recv_fds::<1>().unwrap();
//! assert_eq!(&data[..n], b"hello");
//! assert_eq!(fds.len(), 1);
//! ```

#[cfg(not(unix))]
compile_error!("unix-ancillary only supports Unix platforms");

mod ancillary;
mod cmsg;
mod ext;

pub use ancillary::{AncillaryData, AncillaryError, Messages, ScmRights, SocketAncillary};
pub use ext::{UnixDatagramExt, UnixStreamExt};

use std::io;
use std::os::unix::io::BorrowedFd;

/// Send data with ancillary control messages over a Unix socket.
///
/// This is the low-level API. Prefer `UnixStreamExt::send_fds` for convenience.
pub fn cmsg_sendmsg(
    fd: BorrowedFd<'_>,
    iov: &[io::IoSlice<'_>],
    ancillary: &SocketAncillary<'_>,
) -> io::Result<usize> {
    cmsg::sendmsg_vectored(fd, iov, ancillary.buffer, ancillary.length)
}

/// Receive data with ancillary control messages from a Unix socket.
///
/// This is the low-level API. Prefer `UnixStreamExt::recv_fds` for convenience.
pub fn cmsg_recvmsg(
    fd: BorrowedFd<'_>,
    iov: &mut [io::IoSliceMut<'_>],
    ancillary: &mut SocketAncillary<'_>,
) -> io::Result<usize> {
    let result = cmsg::recvmsg_vectored(fd, iov, ancillary.buffer)?;
    ancillary.length = result.ancillary_len;
    ancillary.truncated = result.truncated;
    Ok(result.bytes_read)
}
