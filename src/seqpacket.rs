//! Descriptor passing on connected `SOCK_SEQPACKET` sockets.
//!
//! Neither std nor tokio has a seqpacket socket type, so these are functions
//! over anything that is [`AsFd`]. Each message keeps its boundary and its
//! descriptors, so unlike a stream there is no framing to do, and a message
//! with descriptors may have an empty payload. A payload too long for the
//! buffer is an `InvalidData` error, as on a datagram socket.
//!
//! macOS has no `AF_UNIX` `SOCK_SEQPACKET`. These functions never create a
//! socket, so nothing here stops them working on one you already have.

use std::io;
use std::os::unix::io::{AsFd, OwnedFd};

use crate::ext::{recv_fds_into_impl, recv_owned_impl, send_fds_impl, CountMode, SocketKind};
use crate::ReceivedFds;

/// Longest payload [`recv_fds`] accepts: the 64 KiB datagram default.
const DEFAULT_BUF: usize = crate::ext::DEFAULT_DATAGRAM_BUF;

/// Send one message of `data` plus borrowed descriptors.
pub fn send_fds(sock: &impl AsFd, data: &[u8], fds: &[impl AsFd]) -> io::Result<usize> {
    send_fds_impl(sock.as_fd(), data, fds)
}

/// Receive one message and up to `N` descriptors, closing any surplus.
/// A closed peer returns an empty result.
pub fn recv_fds<const N: usize>(sock: &impl AsFd) -> io::Result<ReceivedFds> {
    let mut data_buf = vec![0u8; DEFAULT_BUF];
    recv_owned_impl(
        sock.as_fd(),
        &mut data_buf,
        SocketKind::Datagram,
        CountMode::UpTo(N),
    )
}

/// Like [`recv_fds`], but into a caller-supplied buffer. Returns `(bytes_read, fds)`.
pub fn recv_fds_into<const N: usize>(
    sock: &impl AsFd,
    data_buf: &mut [u8],
) -> io::Result<(usize, Vec<OwnedFd>)> {
    recv_fds_into_impl(
        sock.as_fd(),
        data_buf,
        SocketKind::Datagram,
        CountMode::UpTo(N),
    )
}

/// Like [`recv_fds_into`], but `InvalidData` unless exactly `N` descriptors arrived.
pub fn recv_fds_exact_into<const N: usize>(
    sock: &impl AsFd,
    data_buf: &mut [u8],
) -> io::Result<(usize, Vec<OwnedFd>)> {
    recv_fds_into_impl(
        sock.as_fd(),
        data_buf,
        SocketKind::Datagram,
        CountMode::Exact(N),
    )
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
        sock.async_io(Interest::WRITABLE, |s| send_fds_impl(s.as_fd(), data, fds))
            .await
    }

    /// Async [`recv_fds_into`](super::recv_fds_into).
    pub async fn recv_fds_into<const N: usize, T: AsFd + std::os::fd::AsRawFd>(
        sock: &AsyncFd<T>,
        data_buf: &mut [u8],
    ) -> io::Result<(usize, Vec<OwnedFd>)> {
        sock.async_io(Interest::READABLE, |s| {
            recv_fds_into_impl(
                s.as_fd(),
                &mut *data_buf,
                SocketKind::Datagram,
                CountMode::UpTo(N),
            )
        })
        .await
    }

    /// Async [`recv_fds_exact_into`](super::recv_fds_exact_into).
    pub async fn recv_fds_exact_into<const N: usize, T: AsFd + std::os::fd::AsRawFd>(
        sock: &AsyncFd<T>,
        data_buf: &mut [u8],
    ) -> io::Result<(usize, Vec<OwnedFd>)> {
        sock.async_io(Interest::READABLE, |s| {
            recv_fds_into_impl(
                s.as_fd(),
                &mut *data_buf,
                SocketKind::Datagram,
                CountMode::Exact(N),
            )
        })
        .await
    }
}
