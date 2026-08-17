//! Async fd passing on tokio sockets. Enabled by the `tokio` feature.
//!
//! Mirrors the blocking [`UnixStreamExt`](crate::UnixStreamExt) /
//! [`UnixDatagramExt`](crate::UnixDatagramExt) API on tokio's nonblocking
//! sockets. Each call drives the same leak-proof `sendmsg`/`recvmsg` core the
//! blocking path uses, scheduled via tokio readiness ([`async_io`]): the
//! closure runs the syscall and returns `WouldBlock` until the socket is
//! ready, which `async_io` retries. Because the retried closure returns
//! before wrapping any fd in `OwnedFd`, a `WouldBlock` retry never leaks.
//!
//! [`async_io`]: tokio::net::UnixStream::async_io
#![allow(async_fn_in_trait)]

use std::io;
use std::os::unix::io::{AsFd, BorrowedFd, OwnedFd};

use tokio::io::Interest;
use tokio::net::{UnixDatagram, UnixStream};

use crate::cmsg;
use crate::ext::{
    recv_fds_into_impl, send_fds_impl, validate_stream_send, CountMode, ReceivedFds, SocketKind,
    DEFAULT_DATAGRAM_BUF, DEFAULT_STREAM_BUF,
};

/// Async fd passing on tokio's [`UnixStream`].
///
/// The async analogue of [`UnixStreamExt`](crate::UnixStreamExt); identical
/// semantics, including surplus-fd closing and CLOEXEC handling.
pub trait AsyncUnixStreamExt {
    /// Send `data` plus borrowed file descriptors. Caller retains ownership.
    ///
    /// Unix streams do not preserve send-call boundaries. A receive call may
    /// return bytes or descriptors from multiple sends, or only part of one
    /// send. Use a framed protocol when descriptor-to-message association
    /// matters.
    ///
    /// Sending one or more descriptors requires at least one payload byte.
    async fn send_fds(&self, data: &[u8], fds: &[impl AsFd]) -> io::Result<usize>;

    /// Send `data` plus borrowed file descriptors, completing the payload
    /// even if the initial send accepts only part of it.
    ///
    /// The descriptors are attached to the first successful send and never
    /// retransmitted. The remaining payload bytes are written with ordinary
    /// signal-safe sends. Returns `WriteZero` if the socket accepts no
    /// progress.
    ///
    /// Cancellation after the initial send may transfer the descriptors and
    /// a payload prefix; the remaining bytes are then left unsent.
    async fn send_fds_all(&self, data: &[u8], fds: &[impl AsFd]) -> io::Result<()>;

    /// Receive data and up to `N` file descriptors. Surplus fds beyond `N`
    /// are closed automatically.
    async fn recv_fds<const N: usize>(&self) -> io::Result<ReceivedFds>;

    /// Like [`recv_fds`](Self::recv_fds) but writes data into a
    /// caller-supplied buffer. Returns `(bytes_read, fds)`.
    async fn recv_fds_into<const N: usize>(
        &self,
        data_buf: &mut [u8],
    ) -> io::Result<(usize, Vec<OwnedFd>)>;

    /// Receive data and exactly `N` file descriptors.
    ///
    /// Like [`recv_fds`](Self::recv_fds), but errors with `InvalidData`
    /// when the peer sends fewer or more than `N` descriptors. Surplus
    /// descriptors are still closed before the error is returned.
    async fn recv_fds_exact<const N: usize>(&self) -> io::Result<ReceivedFds>;

    /// Like [`recv_fds_exact`](Self::recv_fds_exact) but writes data into a
    /// caller-supplied buffer. Returns `(bytes_read, fds)`.
    async fn recv_fds_exact_into<const N: usize>(
        &self,
        data_buf: &mut [u8],
    ) -> io::Result<(usize, Vec<OwnedFd>)>;
}

impl AsyncUnixStreamExt for UnixStream {
    async fn send_fds(&self, data: &[u8], fds: &[impl AsFd]) -> io::Result<usize> {
        validate_stream_send(data, fds.len())?;
        let borrowed: Vec<BorrowedFd<'_>> = fds.iter().map(|f| f.as_fd()).collect();
        self.async_io(Interest::WRITABLE, || {
            send_fds_impl(self.as_fd(), data, &borrowed)
        })
        .await
    }

    async fn send_fds_all(&self, data: &[u8], fds: &[impl AsFd]) -> io::Result<()> {
        validate_stream_send(data, fds.len())?;
        let borrowed: Vec<BorrowedFd<'_>> = fds.iter().map(|f| f.as_fd()).collect();
        let mut sent = if borrowed.is_empty() {
            0
        } else {
            let first = self
                .async_io(Interest::WRITABLE, || {
                    send_fds_impl(self.as_fd(), data, &borrowed)
                })
                .await?;
            if first == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "failed to send fd payload byte",
                ));
            }
            first
        };
        while sent < data.len() {
            let n = self
                .async_io(Interest::WRITABLE, || {
                    cmsg::send_bytes(self.as_fd(), &data[sent..])
                })
                .await?;
            if n == 0 {
                return Err(io::Error::new(io::ErrorKind::WriteZero, "short write"));
            }
            sent += n;
        }
        Ok(())
    }

    async fn recv_fds<const N: usize>(&self) -> io::Result<ReceivedFds> {
        let mut data_buf = vec![0u8; DEFAULT_STREAM_BUF];
        let (n, fds) = self
            .async_io(Interest::READABLE, || {
                recv_fds_into_impl(
                    self.as_fd(),
                    &mut data_buf,
                    SocketKind::Stream,
                    CountMode::UpTo(N),
                )
            })
            .await?;
        data_buf.truncate(n);
        Ok(ReceivedFds {
            data: data_buf,
            fds,
        })
    }

    async fn recv_fds_into<const N: usize>(
        &self,
        data_buf: &mut [u8],
    ) -> io::Result<(usize, Vec<OwnedFd>)> {
        self.async_io(Interest::READABLE, || {
            recv_fds_into_impl(
                self.as_fd(),
                &mut *data_buf,
                SocketKind::Stream,
                CountMode::UpTo(N),
            )
        })
        .await
    }

    async fn recv_fds_exact<const N: usize>(&self) -> io::Result<ReceivedFds> {
        let mut data_buf = vec![0u8; DEFAULT_STREAM_BUF];
        let (n, fds) = self
            .async_io(Interest::READABLE, || {
                recv_fds_into_impl(
                    self.as_fd(),
                    &mut data_buf,
                    SocketKind::Stream,
                    CountMode::Exact(N),
                )
            })
            .await?;
        data_buf.truncate(n);
        Ok(ReceivedFds {
            data: data_buf,
            fds,
        })
    }

    async fn recv_fds_exact_into<const N: usize>(
        &self,
        data_buf: &mut [u8],
    ) -> io::Result<(usize, Vec<OwnedFd>)> {
        self.async_io(Interest::READABLE, || {
            recv_fds_into_impl(
                self.as_fd(),
                &mut *data_buf,
                SocketKind::Stream,
                CountMode::Exact(N),
            )
        })
        .await
    }
}

/// Async fd passing on tokio's [`UnixDatagram`].
///
/// The async analogue of [`UnixDatagramExt`](crate::UnixDatagramExt). The
/// socket must be connected. All convenience receive methods reject payload
/// truncation with `InvalidData`; a caller who intends to inspect a truncated
/// datagram must use [`crate::cmsg_recvmsg`] and read
/// [`crate::RecvResult::data_truncated`].
pub trait AsyncUnixDatagramExt {
    /// Send `data` plus borrowed fds. The socket must be connected.
    async fn send_fds(&self, data: &[u8], fds: &[impl AsFd]) -> io::Result<usize>;

    /// Receive data and up to `N` fds. Surplus fds beyond `N` are closed.
    async fn recv_fds<const N: usize>(&self) -> io::Result<ReceivedFds>;

    /// Like [`recv_fds`](Self::recv_fds) but writes data into a
    /// caller-supplied buffer. Returns `(bytes_read, fds)`.
    async fn recv_fds_into<const N: usize>(
        &self,
        data_buf: &mut [u8],
    ) -> io::Result<(usize, Vec<OwnedFd>)>;

    /// Receive data and exactly `N` file descriptors.
    ///
    /// Like [`recv_fds`](Self::recv_fds), but errors with `InvalidData`
    /// when the peer sends fewer or more than `N` descriptors. Surplus
    /// descriptors are still closed before the error is returned.
    async fn recv_fds_exact<const N: usize>(&self) -> io::Result<ReceivedFds>;

    /// Like [`recv_fds_exact`](Self::recv_fds_exact) but writes data into a
    /// caller-supplied buffer. Returns `(bytes_read, fds)`.
    async fn recv_fds_exact_into<const N: usize>(
        &self,
        data_buf: &mut [u8],
    ) -> io::Result<(usize, Vec<OwnedFd>)>;
}

impl AsyncUnixDatagramExt for UnixDatagram {
    async fn send_fds(&self, data: &[u8], fds: &[impl AsFd]) -> io::Result<usize> {
        let borrowed: Vec<BorrowedFd<'_>> = fds.iter().map(|f| f.as_fd()).collect();
        self.async_io(Interest::WRITABLE, || {
            send_fds_impl(self.as_fd(), data, &borrowed)
        })
        .await
    }

    async fn recv_fds<const N: usize>(&self) -> io::Result<ReceivedFds> {
        let mut data_buf = vec![0u8; DEFAULT_DATAGRAM_BUF];
        let (n, fds) = self
            .async_io(Interest::READABLE, || {
                recv_fds_into_impl(
                    self.as_fd(),
                    &mut data_buf,
                    SocketKind::Datagram,
                    CountMode::UpTo(N),
                )
            })
            .await?;
        data_buf.truncate(n);
        Ok(ReceivedFds {
            data: data_buf,
            fds,
        })
    }

    async fn recv_fds_into<const N: usize>(
        &self,
        data_buf: &mut [u8],
    ) -> io::Result<(usize, Vec<OwnedFd>)> {
        self.async_io(Interest::READABLE, || {
            recv_fds_into_impl(
                self.as_fd(),
                &mut *data_buf,
                SocketKind::Datagram,
                CountMode::UpTo(N),
            )
        })
        .await
    }

    async fn recv_fds_exact<const N: usize>(&self) -> io::Result<ReceivedFds> {
        let mut data_buf = vec![0u8; DEFAULT_DATAGRAM_BUF];
        let (n, fds) = self
            .async_io(Interest::READABLE, || {
                recv_fds_into_impl(
                    self.as_fd(),
                    &mut data_buf,
                    SocketKind::Datagram,
                    CountMode::Exact(N),
                )
            })
            .await?;
        data_buf.truncate(n);
        Ok(ReceivedFds {
            data: data_buf,
            fds,
        })
    }

    async fn recv_fds_exact_into<const N: usize>(
        &self,
        data_buf: &mut [u8],
    ) -> io::Result<(usize, Vec<OwnedFd>)> {
        self.async_io(Interest::READABLE, || {
            recv_fds_into_impl(
                self.as_fd(),
                &mut *data_buf,
                SocketKind::Datagram,
                CountMode::Exact(N),
            )
        })
        .await
    }
}
