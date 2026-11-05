//! Descriptor passing on connected `SOCK_SEQPACKET` sockets.
//!
//! Neither std nor tokio has a seqpacket socket type, so these are functions
//! over anything that is [`AsFd`]. Each message keeps its boundary and its
//! descriptors, so unlike a stream there is no framing to do, and a message
//! with descriptors may have an empty payload. A payload too long for the
//! buffer is an `InvalidData` error, as on a datagram socket.
//!
//! macOS has no `AF_UNIX` `SOCK_SEQPACKET`. [`pair`] is the only function here
//! that creates a socket; the rest work on one you already have.

use std::io;
use std::os::unix::io::{AsFd, OwnedFd};

use crate::ext::{
    recv_fds_into_impl, recv_fds_into_vectored_impl, recv_owned_impl, send_fds_flags_impl,
    Datagram, Exact, UpTo, DEFAULT_DATAGRAM_BUF,
};
use crate::ReceivedFds;

/// Create a connected pair with close-on-exec set. Use `UnixDatagram::from` on
/// an end when you need `set_nonblocking`.
#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "netbsd",
    target_os = "openbsd",
))]
pub fn pair() -> io::Result<(OwnedFd, OwnedFd)> {
    use std::os::fd::FromRawFd;
    let mut fds = [0; 2];
    let kind = libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC;
    // SAFETY: fds has room for the two descriptors socketpair writes.
    if unsafe { libc::socketpair(libc::AF_UNIX, kind, 0, fds.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: socketpair just returned these and nothing else owns them.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

/// Send one message of `data` plus borrowed descriptors, marking the end of the
/// record with `MSG_EOR`.
pub fn send_fds(sock: &impl AsFd, data: &[u8], fds: &[impl AsFd]) -> io::Result<usize> {
    send_fds_vectored(sock, &[io::IoSlice::new(data)], fds)
}

/// Send one message from several buffers plus borrowed descriptors.
pub fn send_fds_vectored(
    sock: &impl AsFd,
    iov: &[io::IoSlice<'_>],
    fds: &[impl AsFd],
) -> io::Result<usize> {
    send_fds_flags_impl(sock.as_fd(), None, iov, fds, libc::MSG_EOR)
}

/// Receive one message into several buffers and up to `N` descriptors. Returns
/// `(bytes_read, fds)`. It also works on any connected datagram socket, including
/// a tokio one inside `async_io`.
pub fn recv_fds_vectored<const N: usize>(
    sock: &impl AsFd,
    iov: &mut [io::IoSliceMut<'_>],
) -> io::Result<(usize, Vec<OwnedFd>)> {
    recv_fds_into_vectored_impl(sock.as_fd(), iov, Datagram, UpTo(N))
}

/// Receive one message of up to 64 KiB and up to `N` descriptors, closing any
/// surplus.
/// A closed peer returns an empty result, the same as an empty message from a
/// live peer.
pub fn recv_fds<const N: usize>(sock: &impl AsFd) -> io::Result<ReceivedFds> {
    recv_owned_impl(sock.as_fd(), DEFAULT_DATAGRAM_BUF, Datagram, UpTo(N))
}

/// Like [`recv_fds`], but into a caller-supplied buffer. Returns `(bytes_read, fds)`.
pub fn recv_fds_into<const N: usize>(
    sock: &impl AsFd,
    data_buf: &mut [u8],
) -> io::Result<(usize, Vec<OwnedFd>)> {
    recv_fds_into_impl(sock.as_fd(), data_buf, Datagram, UpTo(N))
}

/// Like [`recv_fds`], but `InvalidData` unless exactly `N` descriptors arrived.
/// That includes a closed peer: unlike a stream, a zero-length read here does not
/// have to mean end of stream, so this is not `UnexpectedEof`.
pub fn recv_fds_exact<const N: usize>(sock: &impl AsFd) -> io::Result<ReceivedFds> {
    recv_owned_impl(sock.as_fd(), DEFAULT_DATAGRAM_BUF, Datagram, Exact(N))
}

/// Like [`recv_fds_into`], but `InvalidData` unless exactly `N` descriptors arrived.
pub fn recv_fds_exact_into<const N: usize>(
    sock: &impl AsFd,
    data_buf: &mut [u8],
) -> io::Result<(usize, Vec<OwnedFd>)> {
    recv_fds_into_impl(sock.as_fd(), data_buf, Datagram, Exact(N))
}

/// Async forms over a nonblocking socket registered with tokio's
/// [`AsyncFd`](::tokio::io::unix::AsyncFd). Enabled by the `tokio` feature.
#[cfg(feature = "tokio")]
pub mod tokio {
    use super::*;
    use ::tokio::io::unix::AsyncFd;
    use ::tokio::io::Interest;

    /// Async [`send_fds`](super::send_fds).
    pub async fn send_fds<T: AsFd + std::os::fd::AsRawFd>(
        sock: &AsyncFd<T>,
        data: &[u8],
        fds: &[impl AsFd],
    ) -> io::Result<usize> {
        let iov = [io::IoSlice::new(data)];
        sock.async_io(Interest::WRITABLE, |s| {
            send_fds_flags_impl(s.as_fd(), None, &iov, fds, libc::MSG_EOR)
        })
        .await
    }

    /// Async [`recv_fds`](super::recv_fds).
    pub async fn recv_fds<const N: usize, T: AsFd + std::os::fd::AsRawFd>(
        sock: &AsyncFd<T>,
    ) -> io::Result<ReceivedFds> {
        sock.async_io(Interest::READABLE, |s| {
            recv_owned_impl(s.as_fd(), DEFAULT_DATAGRAM_BUF, Datagram, UpTo(N))
        })
        .await
    }

    /// Async [`recv_fds_into`](super::recv_fds_into).
    pub async fn recv_fds_into<const N: usize, T: AsFd + std::os::fd::AsRawFd>(
        sock: &AsyncFd<T>,
        data_buf: &mut [u8],
    ) -> io::Result<(usize, Vec<OwnedFd>)> {
        sock.async_io(Interest::READABLE, |s| {
            recv_fds_into_impl(s.as_fd(), &mut *data_buf, Datagram, UpTo(N))
        })
        .await
    }

    /// Async [`recv_fds_exact_into`](super::recv_fds_exact_into).
    pub async fn recv_fds_exact_into<const N: usize, T: AsFd + std::os::fd::AsRawFd>(
        sock: &AsyncFd<T>,
        data_buf: &mut [u8],
    ) -> io::Result<(usize, Vec<OwnedFd>)> {
        sock.async_io(Interest::READABLE, |s| {
            recv_fds_into_impl(s.as_fd(), &mut *data_buf, Datagram, Exact(N))
        })
        .await
    }
}
