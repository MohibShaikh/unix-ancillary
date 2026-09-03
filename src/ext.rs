use std::io;
use std::os::unix::io::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::net::{UnixDatagram, UnixStream};

use crate::ancillary::{AncillaryData, SocketAncillary};
use crate::cmsg;
use crate::platform;

pub(crate) const DEFAULT_STREAM_BUF: usize = 4096;
pub(crate) const DEFAULT_DATAGRAM_BUF: usize = 65536;

/// Whether the receive operation is a stream or a datagram. Datagrams reject
/// payload truncation; streams cannot truncate and ignore the flag.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SocketKind {
    Stream,
    Datagram,
}

/// Descriptor-count policy for a receive. Up-to keeps the first `N` and
/// closes the surplus; exact errors unless the peer sent exactly `N`.
#[derive(Clone, Copy)]
pub(crate) enum CountMode {
    UpTo(usize),
    Exact(usize),
}

impl CountMode {
    fn count(self) -> usize {
        match self {
            CountMode::UpTo(n) | CountMode::Exact(n) => n,
        }
    }
}

/// Result of a successful `recv_fds` call.
///
/// Truncation is a hard error in the high-level API: if you observe a
/// successful `Ok(ReceivedFds)`, every fd the kernel deposited has been
/// accounted for. Surplus fds beyond the caller's requested `N` are closed
/// automatically before this struct is returned.
///
/// Payload truncation is not reported here: stream sockets never truncate,
/// and the datagram methods fail with `InvalidData` rather than returning a
/// short payload. Use [`crate::cmsg_recvmsg`] and read
/// [`crate::RecvResult::data_truncated`] to inspect a truncated datagram.
#[derive(Debug)]
#[non_exhaustive]
pub struct ReceivedFds {
    /// Application data bytes received.
    pub data: Vec<u8>,
    /// File descriptors received, capped at `N`. Each `OwnedFd` is closed on
    /// drop.
    pub fds: Vec<OwnedFd>,
}

pub(crate) fn validate_stream_send(data: &[u8], fd_count: usize) -> io::Result<()> {
    if data.is_empty() && fd_count != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "SCM_RIGHTS over a Unix stream requires at least one payload byte",
        ));
    }
    Ok(())
}

pub(crate) fn send_fds_impl(
    fd: BorrowedFd<'_>,
    data: &[u8],
    fds: &[BorrowedFd<'_>],
) -> io::Result<usize> {
    send_fds_vectored_impl(fd, &[io::IoSlice::new(data)], fds)
}

pub(crate) fn send_fds_vectored_impl(
    fd: BorrowedFd<'_>,
    iov: &[io::IoSlice<'_>],
    fds: &[BorrowedFd<'_>],
) -> io::Result<usize> {
    let mut buf = vec![0u8; SocketAncillary::buffer_size_for_rights(fds.len())];
    let mut ancillary = SocketAncillary::new(&mut buf);
    ancillary
        .add_fds(fds)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

    cmsg::sendmsg_vectored(fd, iov, ancillary.buffer, ancillary.length)
}

/// Total payload bytes across the iovecs, for the stream empty-payload rule.
pub(crate) fn iov_len(iov: &[io::IoSlice<'_>]) -> usize {
    iov.iter().map(|s| s.len()).sum()
}

/// Receive into `data_buf`, capping fds returned to the caller at `N`.
///
/// The internal cmsg buffer is sized to a platform-specific upper bound that
/// the kernel cannot exceed for a single `SCM_RIGHTS` message:
///
/// - Linux / *BSD: a fixed `SCM_MAX_FD = 253` per-message cap.
/// - macOS: the receiver's current `RLIMIT_NOFILE`, since the kernel must
///   allocate an fd table entry per delivered fd and cannot deliver more
///   than the receiver can hold.
///
/// Truncation is therefore kernel-impossible. Surplus fds beyond `N` are
/// wrapped in `OwnedFd` and dropped before returning, closing them. If the
/// kernel still reports `MSG_CTRUNC` (defensive — should be unreachable),
/// every fd we extracted is closed and an error is returned.
pub(crate) fn recv_fds_into_impl(
    fd: BorrowedFd<'_>,
    data_buf: &mut [u8],
    kind: SocketKind,
    mode: CountMode,
) -> io::Result<(usize, Vec<OwnedFd>)> {
    let mut iov = [io::IoSliceMut::new(data_buf)];
    recv_fds_into_vectored_impl(fd, &mut iov, kind, mode)
}

pub(crate) fn recv_fds_into_vectored_impl(
    fd: BorrowedFd<'_>,
    iov: &mut [io::IoSliceMut<'_>],
    kind: SocketKind,
    mode: CountMode,
) -> io::Result<(usize, Vec<OwnedFd>)> {
    let cap = mode.count().max(platform::max_recv_fds());
    let mut anc_buf = vec![0u8; SocketAncillary::buffer_size_for_rights(cap)];

    let result = cmsg::recvmsg_vectored(fd, iov, &mut anc_buf)?;

    let ancillary = SocketAncillary {
        buffer: &mut anc_buf,
        length: result.ancillary_len,
        truncated: result.ancillary_truncated,
    };

    // Wrap every fd the kernel handed us. Surplus past the mode are dropped
    // at function exit, closing them and preventing leaks.
    let mut all_fds: Vec<OwnedFd> = Vec::new();
    for msg in ancillary.messages() {
        match msg {
            AncillaryData::ScmRights(rights) => all_fds.extend(rights),
        }
    }

    if result.ancillary_truncated {
        // Defensive path: should be unreachable because `cap` exceeds
        // every kernel's per-message fd limit. If we land here, fds may
        // have been deposited beyond our buffer (macOS) and we cannot
        // reach them. Drop everything we DID receive and surface the
        // error so the caller knows the connection state is suspect.
        drop(all_fds);
        return Err(io::Error::other(
            "ancillary truncated despite oversized buffer; possible fd leak — abort the connection",
        ));
    }

    if kind == SocketKind::Datagram && result.data_truncated {
        // The datagram payload did not fit `data_buf`. Bytes and fds past
        // this point are lost; closing the fds we did receive keeps the
        // process state clean. `cmsg_recvmsg` is the only path that reports
        // the flag instead of failing.
        drop(all_fds);
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "datagram payload truncated before it fit the receive buffer",
        ));
    }

    match mode {
        CountMode::UpTo(n) => {
            // Keep first n; remaining `OwnedFd`s drop here, closing surplus.
            let kept: Vec<OwnedFd> = all_fds.into_iter().take(n).collect();
            Ok((result.bytes_read, kept))
        }
        CountMode::Exact(n) => {
            if all_fds.len() != n {
                let observed = all_fds.len();
                // Close every received descriptor before reporting the count
                // mismatch; the caller must not observe a partial set.
                drop(all_fds);
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("expected exactly {n} file descriptors, received {observed}"),
                ));
            }
            Ok((result.bytes_read, all_fds))
        }
    }
}

/// Extension trait for `UnixStream` adding fd-passing convenience methods.
pub trait UnixStreamExt {
    /// Send `data` plus borrowed file descriptors over the stream.
    ///
    /// Caller retains ownership of the fds.
    ///
    /// Unix streams do not preserve send-call boundaries. A receive call may
    /// return bytes or descriptors from multiple sends, or only part of one
    /// send. Use a framed protocol when descriptor-to-message association
    /// matters.
    ///
    /// Sending one or more descriptors requires at least one payload byte.
    fn send_fds(&self, data: &[u8], fds: &[impl AsFd]) -> io::Result<usize>;

    /// Send `data` plus borrowed file descriptors, completing the payload
    /// even if the initial send accepts only part of it.
    ///
    /// The descriptors are attached to the first successful send and never
    /// retransmitted. The remaining payload bytes are written with ordinary
    /// signal-safe sends. Returns `WriteZero` if the socket accepts no
    /// progress.
    fn send_fds_all(&self, data: &[u8], fds: &[impl AsFd]) -> io::Result<()>;

    /// Send several payload buffers plus borrowed descriptors in one syscall.
    ///
    /// The descriptors ride the same `sendmsg` as the whole iovec, so a header
    /// and a body go out without being copied into one buffer first. The
    /// stream rule still applies: at least one payload byte across all of
    /// `iov` when descriptors are present.
    ///
    /// There is no `send_fds_vectored_all`. Like [`send_fds`](Self::send_fds)
    /// this is one `sendmsg`, so on a stream it can accept only part of the
    /// payload while still delivering every descriptor. If you need the
    /// completion loop, call this once and finish the remaining bytes
    /// yourself, or use [`send_fds_all`](Self::send_fds_all) with a single
    /// contiguous buffer.
    fn send_fds_vectored(
        &self,
        iov: &[io::IoSlice<'_>],
        fds: &[impl AsFd],
    ) -> io::Result<usize>;

    /// Receive into several caller-supplied buffers, capping fds at `N`.
    ///
    /// Returns `(bytes_read, fds)`, filling `iov` in order.
    fn recv_fds_vectored<const N: usize>(
        &self,
        iov: &mut [io::IoSliceMut<'_>],
    ) -> io::Result<(usize, Vec<OwnedFd>)>;

    /// Receive data and up to `N` file descriptors.
    ///
    /// Allocates an internal 4 KiB data buffer plus an oversized cmsg buffer
    /// that prevents kernel-level fd leaks. Surplus fds beyond `N` are
    /// closed automatically.
    fn recv_fds<const N: usize>(&self) -> io::Result<ReceivedFds>;

    /// Like [`recv_fds`](Self::recv_fds) but writes data into a
    /// caller-supplied buffer. Returns `(bytes_read, fds)`.
    fn recv_fds_into<const N: usize>(
        &self,
        data_buf: &mut [u8],
    ) -> io::Result<(usize, Vec<OwnedFd>)>;

    /// Receive data and exactly `N` file descriptors.
    ///
    /// Like [`recv_fds`](Self::recv_fds), but errors with `InvalidData`
    /// when the peer sends fewer or more than `N` descriptors. Surplus
    /// descriptors are still closed before the error is returned.
    fn recv_fds_exact<const N: usize>(&self) -> io::Result<ReceivedFds>;

    /// Like [`recv_fds_exact`](Self::recv_fds_exact) but writes data into a
    /// caller-supplied buffer. Returns `(bytes_read, fds)`.
    fn recv_fds_exact_into<const N: usize>(
        &self,
        data_buf: &mut [u8],
    ) -> io::Result<(usize, Vec<OwnedFd>)>;
}

impl UnixStreamExt for UnixStream {
    fn send_fds(&self, data: &[u8], fds: &[impl AsFd]) -> io::Result<usize> {
        validate_stream_send(data, fds.len())?;
        let borrowed: Vec<BorrowedFd<'_>> = fds.iter().map(|f| f.as_fd()).collect();
        send_fds_impl(self.as_fd(), data, &borrowed)
    }

    fn send_fds_vectored(
        &self,
        iov: &[io::IoSlice<'_>],
        fds: &[impl AsFd],
    ) -> io::Result<usize> {
        if !fds.is_empty() && iov_len(iov) == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "SCM_RIGHTS over a Unix stream requires at least one payload byte",
            ));
        }
        let borrowed: Vec<BorrowedFd<'_>> = fds.iter().map(|f| f.as_fd()).collect();
        send_fds_vectored_impl(self.as_fd(), iov, &borrowed)
    }

    fn recv_fds_vectored<const N: usize>(
        &self,
        iov: &mut [io::IoSliceMut<'_>],
    ) -> io::Result<(usize, Vec<OwnedFd>)> {
        recv_fds_into_vectored_impl(self.as_fd(), iov, SocketKind::Stream, CountMode::UpTo(N))
    }

    fn send_fds_all(&self, data: &[u8], fds: &[impl AsFd]) -> io::Result<()> {
        validate_stream_send(data, fds.len())?;
        let borrowed: Vec<BorrowedFd<'_>> = fds.iter().map(|f| f.as_fd()).collect();
        let mut sent = if borrowed.is_empty() {
            0
        } else {
            let first = send_fds_impl(self.as_fd(), data, &borrowed)?;
            if first == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "failed to send fd payload byte",
                ));
            }
            first
        };
        while sent < data.len() {
            let n = cmsg::send_bytes(self.as_fd(), &data[sent..])?;
            if n == 0 {
                return Err(io::Error::new(io::ErrorKind::WriteZero, "short write"));
            }
            sent += n;
        }
        Ok(())
    }

    fn recv_fds<const N: usize>(&self) -> io::Result<ReceivedFds> {
        let mut data_buf = vec![0u8; DEFAULT_STREAM_BUF];
        let (n, fds) = recv_fds_into_impl(
            self.as_fd(),
            &mut data_buf,
            SocketKind::Stream,
            CountMode::UpTo(N),
        )?;
        data_buf.truncate(n);
        Ok(ReceivedFds {
            data: data_buf,
            fds,
        })
    }

    fn recv_fds_into<const N: usize>(
        &self,
        data_buf: &mut [u8],
    ) -> io::Result<(usize, Vec<OwnedFd>)> {
        recv_fds_into_impl(
            self.as_fd(),
            data_buf,
            SocketKind::Stream,
            CountMode::UpTo(N),
        )
    }

    fn recv_fds_exact<const N: usize>(&self) -> io::Result<ReceivedFds> {
        let mut data_buf = vec![0u8; DEFAULT_STREAM_BUF];
        let (n, fds) = recv_fds_into_impl(
            self.as_fd(),
            &mut data_buf,
            SocketKind::Stream,
            CountMode::Exact(N),
        )?;
        data_buf.truncate(n);
        Ok(ReceivedFds {
            data: data_buf,
            fds,
        })
    }

    fn recv_fds_exact_into<const N: usize>(
        &self,
        data_buf: &mut [u8],
    ) -> io::Result<(usize, Vec<OwnedFd>)> {
        recv_fds_into_impl(
            self.as_fd(),
            data_buf,
            SocketKind::Stream,
            CountMode::Exact(N),
        )
    }
}

/// Extension trait for `UnixDatagram` adding fd-passing convenience methods.
///
/// The socket must be connected. All convenience receive methods reject
/// payload truncation with `InvalidData`; a caller who intends to inspect a
/// truncated datagram must use [`crate::cmsg_recvmsg`] and read
/// [`crate::RecvResult::data_truncated`].
pub trait UnixDatagramExt {
    /// Send `data` plus borrowed fds. The socket must be connected.
    fn send_fds(&self, data: &[u8], fds: &[impl AsFd]) -> io::Result<usize>;

    /// Send several payload buffers plus borrowed descriptors in one datagram.
    fn send_fds_vectored(
        &self,
        iov: &[io::IoSlice<'_>],
        fds: &[impl AsFd],
    ) -> io::Result<usize>;

    /// Receive one datagram into several caller-supplied buffers, capping fds
    /// at `N`. Returns `(bytes_read, fds)`.
    fn recv_fds_vectored<const N: usize>(
        &self,
        iov: &mut [io::IoSliceMut<'_>],
    ) -> io::Result<(usize, Vec<OwnedFd>)>;

    /// Receive data and up to `N` fds.
    ///
    /// Allocates an internal 64 KiB data buffer plus an oversized cmsg
    /// buffer that prevents kernel-level fd leaks. Surplus fds beyond `N`
    /// are closed automatically.
    fn recv_fds<const N: usize>(&self) -> io::Result<ReceivedFds>;

    /// Like [`recv_fds`](Self::recv_fds) but writes data into a
    /// caller-supplied buffer. Returns `(bytes_read, fds)`.
    fn recv_fds_into<const N: usize>(
        &self,
        data_buf: &mut [u8],
    ) -> io::Result<(usize, Vec<OwnedFd>)>;

    /// Receive data and exactly `N` file descriptors.
    ///
    /// Like [`recv_fds`](Self::recv_fds), but errors with `InvalidData`
    /// when the peer sends fewer or more than `N` descriptors. Surplus
    /// descriptors are still closed before the error is returned.
    fn recv_fds_exact<const N: usize>(&self) -> io::Result<ReceivedFds>;

    /// Like [`recv_fds_exact`](Self::recv_fds_exact) but writes data into a
    /// caller-supplied buffer. Returns `(bytes_read, fds)`.
    fn recv_fds_exact_into<const N: usize>(
        &self,
        data_buf: &mut [u8],
    ) -> io::Result<(usize, Vec<OwnedFd>)>;
}

impl UnixDatagramExt for UnixDatagram {
    fn send_fds_vectored(
        &self,
        iov: &[io::IoSlice<'_>],
        fds: &[impl AsFd],
    ) -> io::Result<usize> {
        let borrowed: Vec<BorrowedFd<'_>> = fds.iter().map(|f| f.as_fd()).collect();
        send_fds_vectored_impl(self.as_fd(), iov, &borrowed)
    }

    fn recv_fds_vectored<const N: usize>(
        &self,
        iov: &mut [io::IoSliceMut<'_>],
    ) -> io::Result<(usize, Vec<OwnedFd>)> {
        recv_fds_into_vectored_impl(self.as_fd(), iov, SocketKind::Datagram, CountMode::UpTo(N))
    }

    fn send_fds(&self, data: &[u8], fds: &[impl AsFd]) -> io::Result<usize> {
        let borrowed: Vec<BorrowedFd<'_>> = fds.iter().map(|f| f.as_fd()).collect();
        send_fds_impl(self.as_fd(), data, &borrowed)
    }

    fn recv_fds<const N: usize>(&self) -> io::Result<ReceivedFds> {
        let mut data_buf = vec![0u8; DEFAULT_DATAGRAM_BUF];
        let (n, fds) = recv_fds_into_impl(
            self.as_fd(),
            &mut data_buf,
            SocketKind::Datagram,
            CountMode::UpTo(N),
        )?;
        data_buf.truncate(n);
        Ok(ReceivedFds {
            data: data_buf,
            fds,
        })
    }

    fn recv_fds_into<const N: usize>(
        &self,
        data_buf: &mut [u8],
    ) -> io::Result<(usize, Vec<OwnedFd>)> {
        recv_fds_into_impl(
            self.as_fd(),
            data_buf,
            SocketKind::Datagram,
            CountMode::UpTo(N),
        )
    }

    fn recv_fds_exact<const N: usize>(&self) -> io::Result<ReceivedFds> {
        let mut data_buf = vec![0u8; DEFAULT_DATAGRAM_BUF];
        let (n, fds) = recv_fds_into_impl(
            self.as_fd(),
            &mut data_buf,
            SocketKind::Datagram,
            CountMode::Exact(N),
        )?;
        data_buf.truncate(n);
        Ok(ReceivedFds {
            data: data_buf,
            fds,
        })
    }

    fn recv_fds_exact_into<const N: usize>(
        &self,
        data_buf: &mut [u8],
    ) -> io::Result<(usize, Vec<OwnedFd>)> {
        recv_fds_into_impl(
            self.as_fd(),
            data_buf,
            SocketKind::Datagram,
            CountMode::Exact(N),
        )
    }
}
