//! Safe, ergonomic Unix socket ancillary data (SCM_RIGHTS fd passing).
//!
//! This crate provides a safe Rust API for sending and receiving file
//! descriptors over Unix domain sockets via `SCM_RIGHTS`.
//!
//! # Design
//!
//! - **No `RawFd` in the public API** — `OwnedFd` and `BorrowedFd` only.
//! - **Automatic cleanup** — received fds are `OwnedFd`, closed on drop.
//! - **No fd leaks on truncation** — the high-level API sizes the receive
//!   cmsg buffer past every Unix kernel's per-message fd cap, so the kernel
//!   cannot deliver more fds than we can wrap. Surplus fds beyond the
//!   caller's `N` are wrapped in `OwnedFd` and closed automatically.
//! - **CLOEXEC errors are surfaced** — on platforms without
//!   `MSG_CMSG_CLOEXEC` (notably macOS) we set `FD_CLOEXEC` post-recv; if
//!   that fails, every received fd is closed and the error is returned.
//!
//! # Truncation safety
//!
//! Linux caps a single `SCM_RIGHTS` message at `SCM_MAX_FD = 253` fds; other
//! Unix kernels enforce comparable caps. The high-level
//! [`UnixStreamExt::recv_fds`] / [`UnixDatagramExt::recv_fds`] always allocate
//! a cmsg buffer sized for that cap, regardless of the caller's `N`. Result:
//!
//! - The peer's kernel rejects oversized sends before they hit the wire.
//! - Every fd the kernel delivers becomes an `OwnedFd` we control.
//! - If the kernel still reports `MSG_CTRUNC` (defensive path; should never
//!   fire), every fd we extracted is closed and an error is returned.
//!
//! Low-level callers using [`SocketAncillary`] manage their own buffer and
//! must size it appropriately — the [`is_truncated`](SocketAncillary::is_truncated)
//! flag is exposed for that path.
//!
//! # CLOEXEC race on macOS
//!
//! macOS lacks `MSG_CMSG_CLOEXEC` on `recvmsg`. This crate sets `FD_CLOEXEC`
//! via `fcntl` immediately after the syscall returns, but a concurrent
//! `fork`+`exec` between the two can leak the fd into the child. If your
//! workload forks concurrently with fd-receiving threads, hold a fork lock
//! around the receive.
//!
//! # Quick start
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
//! let recv = rx.recv_fds::<1>().unwrap();
//! assert_eq!(&recv.data[..], b"hello");
//! assert_eq!(recv.fds.len(), 1);
//! ```

#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(not(unix))]
compile_error!("unix-ancillary only supports Unix platforms");

mod ancillary;
mod cmsg;
mod ext;
mod platform;

pub use ancillary::{AncillaryData, AncillaryError, Messages, ScmRights, SocketAncillary};
pub use ext::{ReceivedFds, UnixDatagramExt, UnixStreamExt};

use std::io;
use std::os::unix::io::BorrowedFd;

/// Result returned by [`cmsg_recvmsg`].
#[derive(Debug, Clone, Copy)]
pub struct RecvResult {
    /// Bytes written into the iov buffers.
    pub bytes_read: usize,
    /// `true` if `MSG_CTRUNC` was set on the underlying `recvmsg`.
    pub truncated: bool,
}

/// Send data with ancillary control messages over a Unix socket.
///
/// Low-level API. Prefer [`UnixStreamExt::send_fds`] for convenience.
pub fn cmsg_sendmsg(
    fd: BorrowedFd<'_>,
    iov: &[io::IoSlice<'_>],
    ancillary: &SocketAncillary<'_>,
) -> io::Result<usize> {
    cmsg::sendmsg_vectored(fd, iov, ancillary.buffer, ancillary.length)
}

/// Receive data with ancillary control messages from a Unix socket.
///
/// Low-level API. Prefer [`UnixStreamExt::recv_fds`] for convenience.
///
/// On non-`MSG_CMSG_CLOEXEC` platforms, all received fds have `FD_CLOEXEC`
/// set before this function returns. If that fails for any fd, every
/// received fd is closed and the error is propagated.
pub fn cmsg_recvmsg(
    fd: BorrowedFd<'_>,
    iov: &mut [io::IoSliceMut<'_>],
    ancillary: &mut SocketAncillary<'_>,
) -> io::Result<RecvResult> {
    let result = cmsg::recvmsg_vectored(fd, iov, ancillary.buffer)?;
    ancillary.length = result.ancillary_len;
    ancillary.truncated = result.truncated;
    Ok(RecvResult {
        bytes_read: result.bytes_read,
        truncated: result.truncated,
    })
}
