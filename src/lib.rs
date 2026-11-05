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
//! about every BSD kernel's maximum. macOS reserves XNU's limit of 254
//! descriptors per message. Additional control messages, a full fd table or
//! platform limits can still cause truncation; callers must handle the error.
//!
//! Low-level [`cmsg_recvmsg`] exposes truncation flags and owns the delivered
//! descriptors even when truncated, including on Apple kernels, which report a
//! length longer than what they copied. Size [`SocketAncillary`] for the messages
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
pub mod seqpacket;

pub use ancillary::{AncillaryData, AncillaryError, Messages, ScmRights, SocketAncillary};
#[cfg(any(target_os = "linux", target_os = "android"))]
pub use credentials::{passcred, set_passcred, ScmCredentials};
pub use ext::{recv_fds_slots, ReceivedFds, RecvSlots, UnixDatagramExt, UnixStreamExt};

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
///
/// Unlike the convenience methods, this does not check the stream payload rule.
/// On a stream, an empty payload returns `Ok(0)` and delivers no descriptors.
pub fn cmsg_sendmsg(
    fd: BorrowedFd<'_>,
    iov: &[io::IoSlice<'_>],
    ancillary: &SocketAncillary<'_>,
) -> io::Result<usize> {
    send_from_ancillary(fd, iov, ancillary, 0)
}

/// [`cmsg_sendmsg`] with extra `sendmsg` flags: `MSG_DONTWAIT` and `MSG_EOR`.
///
/// The crate's signal-safety flags always stay on. Any other flag is
/// `InvalidInput`.
pub fn cmsg_sendmsg_with_flags(
    fd: BorrowedFd<'_>,
    iov: &[io::IoSlice<'_>],
    ancillary: &SocketAncillary<'_>,
    flags: libc::c_int,
) -> io::Result<usize> {
    allow_flags(flags, libc::MSG_DONTWAIT | libc::MSG_EOR)?;
    send_from_ancillary(fd, iov, ancillary, flags)
}

fn send_from_ancillary(
    fd: BorrowedFd<'_>,
    iov: &[io::IoSlice<'_>],
    ancillary: &SocketAncillary<'_>,
    flags: libc::c_int,
) -> io::Result<usize> {
    if ancillary.receive_mode {
        let msg = "clear the receive buffer before building a send";
        return Err(io::Error::new(io::ErrorKind::InvalidInput, msg));
    }
    cmsg::sendmsg_flags(fd, iov, ancillary.buffer, ancillary.length, flags)
}

/// [`cmsg_recvmsg`] with extra `recvmsg` flags: `MSG_DONTWAIT` and `MSG_WAITALL`.
///
/// Close-on-exec handling always stays on. Any other flag is `InvalidInput`,
/// including `MSG_PEEK`, which is only safe on some kernels (see
/// `cmsg_recvmsg_peek`).
pub fn cmsg_recvmsg_with_flags(
    fd: BorrowedFd<'_>,
    iov: &mut [io::IoSliceMut<'_>],
    ancillary: &mut SocketAncillary<'_>,
    flags: libc::c_int,
) -> io::Result<RecvResult> {
    allow_flags(flags, libc::MSG_DONTWAIT | libc::MSG_WAITALL)?;
    recv_into_ancillary(fd, iov, ancillary, flags)
}

fn allow_flags(flags: libc::c_int, allowed: libc::c_int) -> io::Result<()> {
    if flags & !allowed != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unsupported socket flag",
        ));
    }
    Ok(())
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
    recv_into_ancillary(fd, iov, ancillary, 0)
}

/// Like [`cmsg_recvmsg`], but leave the message queued (`MSG_PEEK`).
///
/// Linux and Android only: their kernels install a fresh duplicate of each
/// descriptor on every peek, so what this returns is safely owned. The final
/// real receive delivers its own copies, and every copy must be closed. XNU does
/// not install descriptors on peek, and no BSD kernel has been tested.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub fn cmsg_recvmsg_peek(
    fd: BorrowedFd<'_>,
    iov: &mut [io::IoSliceMut<'_>],
    ancillary: &mut SocketAncillary<'_>,
) -> io::Result<RecvResult> {
    recv_into_ancillary(fd, iov, ancillary, libc::MSG_PEEK)
}

fn recv_into_ancillary(
    fd: BorrowedFd<'_>,
    iov: &mut [io::IoSliceMut<'_>],
    ancillary: &mut SocketAncillary<'_>,
    extra_flags: libc::c_int,
) -> io::Result<RecvResult> {
    ancillary.clear();
    ancillary.receive_mode = true;
    let result = cmsg::recvmsg_raw(fd, iov, ancillary.buffer, extra_flags)?;
    // SAFETY: this is fresh, kernel-produced recvmsg output, consumed once.
    // Establish ownership before fallible CLOEXEC processing so an error drops
    // every delivered descriptor, including those not yet processed.
    unsafe {
        ancillary::take_received_into(
            &ancillary.buffer[..result.ancillary_len],
            &mut ancillary.received,
        );
    }
    if let Err(err) = platform::cloexec_received(&ancillary.received) {
        ancillary.received.clear();
        return Err(err);
    }
    ancillary.length = result.ancillary_len;
    ancillary.truncated = result.ancillary_truncated;
    Ok(RecvResult {
        bytes_read: result.bytes_read,
        truncated: result.ancillary_truncated,
        data_truncated: result.data_truncated,
    })
}
