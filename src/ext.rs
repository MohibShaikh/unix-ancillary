use std::io;
use std::os::unix::io::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::net::{UnixDatagram, UnixStream};

use crate::ancillary::{AncillaryData, SocketAncillary};
use crate::cmsg;

const DEFAULT_STREAM_BUF: usize = 4096;
const DEFAULT_DATAGRAM_BUF: usize = 65536;

/// Conservative upper bound on fds in a single `SCM_RIGHTS` message.
///
/// Linux hard-codes `SCM_MAX_FD = 253` in the kernel; sendmsg fails with
/// `EINVAL` past it. Other Unix kernels enforce comparable limits via
/// `OPEN_MAX` or per-message caps. Sizing the receive ancillary buffer to
/// this value makes truncation effectively impossible: the sender's kernel
/// rejects oversized messages before they reach the wire. Even if a peer
/// somehow delivered more, we surface it as a hard error rather than
/// silently leaking fds — but in practice this branch is unreachable.
const MAX_RECV_FDS: usize = 253;

/// Result of a successful `recv_fds` call.
///
/// Truncation is a hard error in the high-level API: if you observe a
/// successful `Ok(ReceivedFds)`, every fd the kernel deposited has been
/// accounted for. Surplus fds beyond the caller's requested `N` are closed
/// automatically before this struct is returned.
#[derive(Debug)]
#[non_exhaustive]
pub struct ReceivedFds {
    /// Application data bytes received.
    pub data: Vec<u8>,
    /// File descriptors received, capped at `N`. Each `OwnedFd` is closed on
    /// drop.
    pub fds: Vec<OwnedFd>,
}

fn send_fds_impl(fd: BorrowedFd<'_>, data: &[u8], fds: &[BorrowedFd<'_>]) -> io::Result<usize> {
    let mut buf = vec![0u8; SocketAncillary::buffer_size_for_rights(fds.len())];
    let mut ancillary = SocketAncillary::new(&mut buf);
    ancillary
        .add_fds(fds)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

    let iov = [io::IoSlice::new(data)];
    cmsg::sendmsg_vectored(fd, &iov, ancillary.buffer, ancillary.length)
}

/// Receive into `data_buf`, capping fds returned to the caller at `N`.
///
/// The internal cmsg buffer is always sized for `max(N, MAX_RECV_FDS)` so the
/// kernel cannot truncate under any realistic sender. Surplus fds (beyond
/// `N`) are wrapped in `OwnedFd` and dropped before returning, closing them.
/// If the kernel still reports `MSG_CTRUNC`, every fd we managed to extract
/// is closed and an error is returned — the caller never sees partial state.
fn recv_fds_into_impl<const N: usize>(
    fd: BorrowedFd<'_>,
    data_buf: &mut [u8],
) -> io::Result<(usize, Vec<OwnedFd>)> {
    let cap = N.max(MAX_RECV_FDS);
    let mut anc_buf = vec![0u8; SocketAncillary::buffer_size_for_rights(cap)];

    let mut iov = [io::IoSliceMut::new(data_buf)];
    let result = cmsg::recvmsg_vectored(fd, &mut iov, &mut anc_buf)?;

    let ancillary = SocketAncillary {
        buffer: &mut anc_buf,
        length: result.ancillary_len,
        truncated: result.truncated,
    };

    // Wrap every fd the kernel handed us. Surplus past `N` are dropped at
    // function exit, closing them and preventing leaks.
    let mut all_fds: Vec<OwnedFd> = Vec::new();
    for msg in ancillary.messages() {
        match msg {
            AncillaryData::ScmRights(rights) => all_fds.extend(rights),
        }
    }

    if result.truncated {
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

    // Keep first N; remaining `OwnedFd`s drop here, closing surplus fds.
    let kept: Vec<OwnedFd> = all_fds.into_iter().take(N).collect();

    Ok((result.bytes_read, kept))
}

/// Extension trait for `UnixStream` adding fd-passing convenience methods.
pub trait UnixStreamExt {
    /// Send `data` plus borrowed file descriptors over the stream.
    ///
    /// Caller retains ownership of the fds.
    fn send_fds(&self, data: &[u8], fds: &[impl AsFd]) -> io::Result<usize>;

    /// Receive data and up to `N` file descriptors.
    ///
    /// Allocates an internal data buffer of
    /// [`DEFAULT_STREAM_BUF`](self::DEFAULT_STREAM_BUF) bytes plus an
    /// oversized cmsg buffer that prevents kernel-level fd leaks. Surplus
    /// fds beyond `N` are closed automatically.
    fn recv_fds<const N: usize>(&self) -> io::Result<ReceivedFds>;

    /// Like [`recv_fds`](Self::recv_fds) but writes data into a
    /// caller-supplied buffer. Returns `(bytes_read, fds)`.
    fn recv_fds_into<const N: usize>(
        &self,
        data_buf: &mut [u8],
    ) -> io::Result<(usize, Vec<OwnedFd>)>;
}

impl UnixStreamExt for UnixStream {
    fn send_fds(&self, data: &[u8], fds: &[impl AsFd]) -> io::Result<usize> {
        let borrowed: Vec<BorrowedFd<'_>> = fds.iter().map(|f| f.as_fd()).collect();
        send_fds_impl(self.as_fd(), data, &borrowed)
    }

    fn recv_fds<const N: usize>(&self) -> io::Result<ReceivedFds> {
        let mut data_buf = vec![0u8; DEFAULT_STREAM_BUF];
        let (n, fds) = recv_fds_into_impl::<N>(self.as_fd(), &mut data_buf)?;
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
        recv_fds_into_impl::<N>(self.as_fd(), data_buf)
    }
}

/// Extension trait for `UnixDatagram` adding fd-passing convenience methods.
pub trait UnixDatagramExt {
    /// Send `data` plus borrowed fds. The socket must be connected.
    fn send_fds(&self, data: &[u8], fds: &[impl AsFd]) -> io::Result<usize>;

    /// Receive data and up to `N` fds.
    ///
    /// Allocates an internal data buffer of
    /// [`DEFAULT_DATAGRAM_BUF`](self::DEFAULT_DATAGRAM_BUF) bytes plus an
    /// oversized cmsg buffer that prevents kernel-level fd leaks. Surplus
    /// fds beyond `N` are closed automatically.
    fn recv_fds<const N: usize>(&self) -> io::Result<ReceivedFds>;

    /// Like [`recv_fds`](Self::recv_fds) but writes data into a
    /// caller-supplied buffer. Returns `(bytes_read, fds)`.
    fn recv_fds_into<const N: usize>(
        &self,
        data_buf: &mut [u8],
    ) -> io::Result<(usize, Vec<OwnedFd>)>;
}

impl UnixDatagramExt for UnixDatagram {
    fn send_fds(&self, data: &[u8], fds: &[impl AsFd]) -> io::Result<usize> {
        let borrowed: Vec<BorrowedFd<'_>> = fds.iter().map(|f| f.as_fd()).collect();
        send_fds_impl(self.as_fd(), data, &borrowed)
    }

    fn recv_fds<const N: usize>(&self) -> io::Result<ReceivedFds> {
        let mut data_buf = vec![0u8; DEFAULT_DATAGRAM_BUF];
        let (n, fds) = recv_fds_into_impl::<N>(self.as_fd(), &mut data_buf)?;
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
        recv_fds_into_impl::<N>(self.as_fd(), data_buf)
    }
}
