//! Safe, ergonomic Unix socket ancillary data (SCM_RIGHTS fd passing).
//!
//! This crate provides a safe Rust API for sending and receiving file
//! descriptors over Unix domain sockets via `SCM_RIGHTS`.
//!
//! # Design
//!
//! - **No `RawFd` in the public API** — `OwnedFd` and `BorrowedFd` only.
//! - **Automatic cleanup** — received fds are `OwnedFd`, closed on drop.
//! - **Ownership on receive** — descriptors are owned before the receive
//!   function returns. Low-level buffers close unread descriptors on clear,
//!   reuse or drop; message iterators close unconsumed descriptors on drop.
//! - **CLOEXEC errors are surfaced** — on platforms without
//!   `MSG_CMSG_CLOEXEC` (notably macOS), a failed `fcntl` closes the whole batch.
//!
//! # Truncation policy
//!
//! Convenience receives reject ancillary truncation and datagram payload
//! truncation with `InvalidData`, closing every descriptor delivered in the
//! control buffer. Linux/Android reserve room for 253 descriptors plus sender
//! credentials. BSD uses a 253-descriptor receive budget; this is not a claim
//! about every BSD kernel's maximum. macOS uses a bounded `RLIMIT_NOFILE`
//! estimate. Additional control messages or platform limits can still cause
//! truncation; callers must handle the error.
//!
//! Low-level [`cmsg_recvmsg`] exposes truncation flags and owns the delivered
//! descriptors even when truncated. Size [`SocketAncillary`] for the messages
//! your protocol accepts, then drain [`SocketAncillary::messages`] once.
//! Platform handling of descriptors omitted from the control buffer is a
//! kernel responsibility; this crate cannot close descriptors it cannot observe.
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
//!
//! # Stream semantics
//!
//! Unix streams do not preserve send-call boundaries: a receive call may
//! return bytes or descriptors from multiple sends, or only part of one send.
//! Use a framed protocol (or a datagram / seqpacket socket) when
//! descriptor-to-message association matters. Sending one or more descriptors
//! over a stream requires at least one payload byte.
//!
//! # Choosing a send / receive method
//!
//! - [`UnixStreamExt::send_fds`] is one `sendmsg` — a single atomic call,
//!   not a message transaction. A partial accept leaves descriptors
//!   delivered and bytes pending.
//! - [`UnixStreamExt::send_fds_all`] sends descriptors exactly once, then
//!   completes the remaining ordinary bytes with signal-safe sends.
//! - [`UnixStreamExt::recv_fds`] is permissive: up to `N` descriptors, and
//!   surplus descriptors are closed automatically.
//! - [`UnixStreamExt::recv_fds_exact`] validates the descriptor count,
//!   returning `InvalidData` unless the peer sent exactly `N`.
//! - Datagram methods reject payload truncation with `InvalidData`; use
//!   [`cmsg_recvmsg`] to inspect a truncated datagram via
//!   [`RecvResult::data_truncated`].
//! - Sends are `SIGPIPE`-safe (`MSG_NOSIGNAL` / `SO_NOSIGPIPE`) and retry
//!   internally on `EINTR`.

#![deny(unsafe_op_in_unsafe_fn)]
#![doc = include_str!("../MIGRATION.md")]

#[cfg(not(unix))]
compile_error!("unix-ancillary only supports Unix platforms");

mod ancillary;
#[cfg(feature = "tokio")]
mod asyncio;
mod cmsg;
#[cfg(any(target_os = "linux", target_os = "android"))]
mod credentials;
mod ext;
mod platform;

pub use ancillary::{AncillaryData, AncillaryError, Messages, ScmRights, SocketAncillary};
#[cfg(any(target_os = "linux", target_os = "android"))]
pub use credentials::{passcred, set_passcred, ScmCredentials};
pub use ext::{ReceivedFds, UnixDatagramExt, UnixStreamExt};

#[cfg(feature = "tokio")]
pub use asyncio::{AsyncUnixDatagramExt, AsyncUnixStreamExt};

#[doc(hidden)]
pub use ancillary::__fuzz_parse;

use std::io;
use std::os::unix::io::BorrowedFd;

/// Result returned by [`cmsg_recvmsg`].
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct RecvResult {
    /// Bytes written into the iov buffers.
    pub bytes_read: usize,
    /// `true` if `MSG_CTRUNC` was set on the underlying `recvmsg`.
    pub truncated: bool,
    /// `true` if `MSG_TRUNC` was set: the datagram payload did not fit the
    /// iov buffers and was truncated. Stream sockets never set this.
    pub data_truncated: bool,
}

/// Send data with ancillary control messages over a Unix socket.
///
/// Low-level API. Prefer [`UnixStreamExt::send_fds`] for convenience.
pub fn cmsg_sendmsg(
    fd: BorrowedFd<'_>,
    iov: &[io::IoSlice<'_>],
    ancillary: &SocketAncillary<'_>,
) -> io::Result<usize> {
    if ancillary.receive_mode {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "clear the receive buffer before building a send",
        ));
    }
    cmsg::sendmsg_vectored(fd, iov, ancillary.buffer, ancillary.length)
}

/// Receive data with ancillary control messages from a Unix socket.
///
/// Low-level API. Prefer [`UnixStreamExt::recv_fds`] for convenience.
/// Previously unread descriptors are closed before attempting this receive,
/// including when it returns an error. On success, use
/// [`SocketAncillary::messages`] to drain the newly owned descriptors.
///
/// On non-`MSG_CMSG_CLOEXEC` platforms, all received fds have `FD_CLOEXEC`
/// set before this function returns. If that fails for any fd, every
/// received fd is closed and the error is propagated.
pub fn cmsg_recvmsg(
    fd: BorrowedFd<'_>,
    iov: &mut [io::IoSliceMut<'_>],
    ancillary: &mut SocketAncillary<'_>,
) -> io::Result<RecvResult> {
    ancillary.clear();
    ancillary.receive_mode = true;
    let result = cmsg::recvmsg_vectored(fd, iov, ancillary.buffer)?;
    ancillary.received = result.messages;
    ancillary.length = result.ancillary_len;
    ancillary.truncated = result.ancillary_truncated;
    Ok(RecvResult {
        bytes_read: result.bytes_read,
        truncated: result.ancillary_truncated,
        data_truncated: result.data_truncated,
    })
}
