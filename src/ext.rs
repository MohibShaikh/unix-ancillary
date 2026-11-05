use std::cell::Cell;
use std::io;
use std::io::ErrorKind::{InvalidData, InvalidInput, WouldBlock, WriteZero};
use std::os::unix::io::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::net::{UnixDatagram, UnixStream};

use crate::addr::RawAddr;
use crate::ancillary::{with_control, SocketAncillary};
use crate::cmsg;
use crate::platform;
pub(crate) use CountMode::{Exact, UpTo};
pub(crate) use SocketKind::{Datagram, Stream};

pub(crate) const DEFAULT_STREAM_BUF: usize = 4096;
pub(crate) const DEFAULT_DATAGRAM_BUF: usize = 65536;

/// Whether a receive is on a stream or a datagram. A zero-length read on a stream
/// is end of stream; on a datagram it can be an empty message.
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
    /// Descriptors the peer sent beyond `N`, already closed.
    pub surplus_closed: usize,
    /// The sender's credentials, when the socket has [`crate::set_passcred`] on.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub credentials: Option<crate::ScmCredentials>,
}

/// Reject descriptors with no payload byte to carry them on a stream.
pub(crate) fn validate_stream_iov(iov: &[io::IoSlice<'_>], fd_count: usize) -> io::Result<()> {
    if fd_count != 0 && iov.iter().all(|buf| buf.is_empty()) {
        let msg = "SCM_RIGHTS over a Unix stream requires at least one payload byte";
        return Err(io::Error::new(InvalidInput, msg));
    }
    Ok(())
}

pub(crate) fn send_fds_impl<F: AsFd>(
    fd: BorrowedFd<'_>,
    data: &[u8],
    fds: &[F],
) -> io::Result<usize> {
    send_fds_flags_impl(fd, None, &[io::IoSlice::new(data)], fds, 0)
}

pub(crate) fn send_fds_flags_impl<F: AsFd>(
    fd: BorrowedFd<'_>,
    addr: Option<&RawAddr>,
    iov: &[io::IoSlice<'_>],
    fds: &[F],
    flags: libc::c_int,
) -> io::Result<usize> {
    // No descriptors: send no control message, not an empty SCM_RIGHTS header.
    if fds.is_empty() {
        return cmsg::sendmsg_flags(fd, addr, iov, &[], 0, flags);
    }
    with_control(SocketAncillary::buffer_size_for_rights(fds.len()), |buf| {
        let mut ancillary = SocketAncillary::new(buf);
        ancillary.add_rights(fds.iter().map(|f| f.as_fd().as_raw_fd()))?;
        cmsg::sendmsg_flags(fd, addr, iov, ancillary.buffer, ancillary.length, flags)
    })
    .map_err(|err| {
        // A bare `EINVAL` here is all the kernel says about this limit.
        if err.raw_os_error() == Some(libc::EINVAL) && fds.len() > 253 {
            let msg = format!("{} descriptors is too many for one message", fds.len());
            return io::Error::new(InvalidInput, msg);
        }
        err
    })
}

/// Receive into `data_buf`, capping fds returned to the caller at `N`.
///
/// The ancillary budget covers Linux's 253-fd limit plus automatic credentials.
/// macOS reserves XNU's 254-fd message limit, and other targets what was asked.
/// Any reported truncation is an error, closing all delivered descriptors.
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
    let (result, fds, _) = recv_core(fd, iov, kind, mode)?;
    Ok((result.bytes_read, fds))
}

thread_local! {
    /// Receive buffer for [`recv_owned_impl`]. Reusing it means a small message
    /// neither pays to zero the default size nor keeps that size alive.
    static SCRATCH: Cell<Vec<u8>> = const { Cell::new(Vec::new()) };
}

/// Receive up to `size` bytes and return them in a [`ReceivedFds`], in a `Vec`
/// no larger than the message.
pub(crate) fn recv_owned_impl(
    fd: BorrowedFd<'_>,
    size: usize,
    kind: SocketKind,
    mode: CountMode,
) -> io::Result<ReceivedFds> {
    // `try_with` fails only during thread teardown, where a fresh buffer is fine.
    let mut scratch = SCRATCH.try_with(Cell::take).unwrap_or_default();
    if scratch.len() < size {
        scratch.resize(size, 0);
    }
    let mut iov = [io::IoSliceMut::new(&mut scratch[..size])];
    let received = recv_core(fd, &mut iov, kind, mode);
    // Do not keep the payload past this call. `WouldBlock` wrote nothing, and an
    // async receive retries on it, so skip the 64 KiB there.
    let result = received.map(|(result, fds, surplus_closed)| ReceivedFds {
        data: scratch[..result.bytes_read].to_vec(),
        fds,
        surplus_closed,
        #[cfg(any(target_os = "linux", target_os = "android"))]
        credentials: result.credentials,
    });
    let written = match &result {
        Ok(received) => received.data.len(),
        Err(err) if err.kind() == WouldBlock => 0,
        Err(_) => size,
    };
    scratch[..written].fill(0);
    let _ = SCRATCH.try_with(|cell| cell.set(scratch));
    result
}

/// Returns `(result, fds, surplus_closed)`.
fn recv_core(
    fd: BorrowedFd<'_>,
    iov: &mut [io::IoSliceMut<'_>],
    kind: SocketKind,
    mode: CountMode,
) -> io::Result<(cmsg::RecvMsgResult, Vec<OwnedFd>, usize)> {
    let has_room = iov.iter().any(|buf| !buf.is_empty());
    let (UpTo(wanted) | Exact(wanted)) = mode;
    let size = recv_control_size(wanted);
    let (result, mut fds) = with_control(size, |buf| cmsg::recv_fds(fd, iov, buf))?;
    check_truncation(&result)?;

    if let (Exact(n), Stream, 0, true, true) =
        (mode, kind, result.bytes_read, fds.is_empty(), has_room)
    {
        if n > 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("stream ended while expecting {n} file descriptors"),
            ));
        }
    }

    match mode {
        UpTo(n) => {
            // Keep first n; truncate closes the surplus `OwnedFd`s.
            let surplus = fds.len().saturating_sub(n);
            fds.truncate(n);
            Ok((result, fds, surplus))
        }
        Exact(n) => {
            if fds.len() != n {
                // Returning drops `fds`, closing every received descriptor; the
                // caller must not observe a partial set.
                return Err(io::Error::new(
                    InvalidData,
                    format!(
                        "expected exactly {n} file descriptors, received {}",
                        fds.len()
                    ),
                ));
            }
            Ok((result, fds, 0))
        }
    }
}

/// Control storage for a receive of up to `fds` descriptors. The budget covers
/// the kernel limit even when fewer are wanted, so surplus can be closed.
fn recv_control_size(fds: usize) -> usize {
    let size = SocketAncillary::buffer_size_for_rights(platform::recv_budget(fds));
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let size = size + SocketAncillary::buffer_size_for_credentials();
    size
}

/// Reject a truncated receive. Returning the error drops anything delivered.
fn check_truncation(result: &cmsg::RecvMsgResult) -> io::Result<()> {
    if result.ancillary_truncated {
        return Err(io::Error::new(
            InvalidData,
            "ancillary data truncated by too many descriptors or a full fd table; \
             discard the incomplete message",
        ));
    }
    if result.data_truncated {
        // `cmsg_recvmsg` is the only path that reports the flag instead of failing.
        let msg = "datagram payload truncated before it fit the receive buffer";
        return Err(io::Error::new(InvalidData, msg));
    }
    Ok(())
}

/// Outcome of [`recv_fds_slots`].
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct RecvSlots {
    /// Bytes written into the data buffer.
    pub bytes_read: usize,
    /// Slots filled, starting from the first.
    pub fds: usize,
    /// Descriptors that did not fit, already closed.
    pub surplus_closed: usize,
    /// The sender's credentials, when the socket has [`crate::set_passcred`] on.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub credentials: Option<crate::ScmCredentials>,
}

/// Receive into caller storage without allocating.
///
/// `vec![None; n]` does not compile, because `OwnedFd` is not `Clone`. For a count
/// known at run time, use `(0..n).map(|_| None).collect::<Vec<_>>()` once and reuse it.
///
/// Works on any connected Unix socket that is `AsFd`, with the same CLOEXEC
/// handling and truncation errors as the other receives. Existing contents of
/// `slots` are closed first, and every slot is empty on error.
///
/// On a tokio socket, run it inside `async_io` so readiness is handled for you:
///
/// ```no_run
/// # async fn demo(socket: tokio::net::UnixStream) -> std::io::Result<()> {
/// use tokio::io::Interest;
/// let (mut data, mut slots) = ([0u8; 64], [None, None]);
/// let got = socket
///     .async_io(Interest::READABLE, || {
///         unix_ancillary::recv_fds_slots(&socket, &mut data, &mut slots)
///     })
///     .await?;
/// # let _ = got;
/// # Ok(())
/// # }
/// ```
pub fn recv_fds_slots(
    sock: &impl AsFd,
    data_buf: &mut [u8],
    slots: &mut [Option<OwnedFd>],
) -> io::Result<RecvSlots> {
    let mut iov = [io::IoSliceMut::new(data_buf)];
    recv_slots(sock.as_fd(), &mut iov, slots, 0, None)
}

/// [`recv_fds_slots`] into several buffers, with `MSG_DONTWAIT` or `MSG_WAITALL`.
///
/// Any other flag is `InvalidInput`, with every slot empty.
pub fn recv_fds_slots_vectored(
    sock: &impl AsFd,
    iov: &mut [io::IoSliceMut<'_>],
    slots: &mut [Option<OwnedFd>],
    flags: libc::c_int,
) -> io::Result<RecvSlots> {
    slots.fill_with(|| None);
    crate::allow_flags(flags, crate::RECV_FLAGS_ALLOWED)?;
    recv_slots(sock.as_fd(), iov, slots, flags, None)
}

pub(crate) fn recv_slots(
    fd: BorrowedFd<'_>,
    iov: &mut [io::IoSliceMut<'_>],
    slots: &mut [Option<OwnedFd>],
    flags: libc::c_int,
    addr: Option<&mut RawAddr>,
) -> io::Result<RecvSlots> {
    let size = recv_control_size(slots.len());
    let (result, fds, surplus_closed) = with_control(size, |buf| {
        cmsg::recv_fds_slots(fd, iov, buf, slots, flags, addr)
    })?;
    if let Err(err) = check_truncation(&result) {
        slots.fill_with(|| None);
        return Err(err);
    }
    Ok(RecvSlots {
        bytes_read: result.bytes_read,
        fds,
        surplus_closed,
        #[cfg(any(target_os = "linux", target_os = "android"))]
        credentials: result.credentials,
    })
}

/// A send that carried descriptors but no bytes, which a stream should never do.
/// Going on would finish the payload and report success with the descriptors lost.
pub(crate) fn require_progress(sent: usize) -> io::Result<usize> {
    (sent > 0).then_some(sent).ok_or(WriteZero.into())
}

/// The error payload when [`UnixStreamExt::send_fds_all`] stops with `WouldBlock`
/// after sending part of the data, on a nonblocking socket or one with a send
/// timeout.
///
/// The descriptors have already gone, so retrying the whole call would send them
/// twice. Read it with `err.get_ref()` and `downcast_ref`, then send the rest of
/// the bytes without descriptors.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct PartialSend {
    /// Bytes of the payload already sent.
    pub sent: usize,
}

impl std::fmt::Display for PartialSend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "would block after sending {} bytes", self.sent)
    }
}

impl std::error::Error for PartialSend {}

/// The owned and into-buffer receives, the same on each socket type apart from
/// its kind and default buffer size.
macro_rules! recv_methods {
    ($kind:expr, $size:expr) => {
        fn recv_fds_vectored<const N: usize>(
            &self,
            iov: &mut [io::IoSliceMut<'_>],
        ) -> io::Result<(usize, Vec<OwnedFd>)> {
            recv_fds_into_vectored_impl(self.as_fd(), iov, $kind, UpTo(N))
        }

        fn recv_fds<const N: usize>(&self) -> io::Result<ReceivedFds> {
            recv_owned_impl(self.as_fd(), $size, $kind, UpTo(N))
        }

        fn recv_fds_into<const N: usize>(
            &self,
            data_buf: &mut [u8],
        ) -> io::Result<(usize, Vec<OwnedFd>)> {
            recv_fds_into_impl(self.as_fd(), data_buf, $kind, UpTo(N))
        }

        fn recv_fds_exact<const N: usize>(&self) -> io::Result<ReceivedFds> {
            recv_owned_impl(self.as_fd(), $size, $kind, Exact(N))
        }

        fn recv_fds_exact_into<const N: usize>(
            &self,
            data_buf: &mut [u8],
        ) -> io::Result<(usize, Vec<OwnedFd>)> {
            recv_fds_into_impl(self.as_fd(), data_buf, $kind, Exact(N))
        }
    };
}

/// Extension trait for `UnixStream` adding fd-passing convenience methods.
///
/// To pass no descriptors, spell the empty list's type, because `&[]` alone cannot
/// be inferred: `&[] as &[BorrowedFd]`. A list holds one type, so convert mixed
/// descriptors with `as_fd` first. On `tokio`'s split halves, call the method
/// through `.as_ref()`.
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
    /// progress. See [`PartialSend`] for a `WouldBlock` partway through.
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
    fn send_fds_vectored(&self, iov: &[io::IoSlice<'_>], fds: &[impl AsFd]) -> io::Result<usize>;

    /// Receive into several caller-supplied buffers, capping fds at `N`.
    ///
    /// Returns `(bytes_read, fds)`, filling `iov` in order.
    fn recv_fds_vectored<const N: usize>(
        &self,
        iov: &mut [io::IoSliceMut<'_>],
    ) -> io::Result<(usize, Vec<OwnedFd>)>;

    /// Receive data and up to `N` file descriptors.
    ///
    /// Reads into a 4 KiB per-thread buffer, then copies out what arrived and clears
    /// it. Surplus fds beyond `N` are closed
    /// automatically and counted in [`ReceivedFds::surplus_closed`]. Control
    /// storage reserves room for many descriptors, but a peer can still
    /// overflow it; that is reported as `InvalidData`.
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
    /// descriptors are still closed before the error is returned, and the
    /// bytes from that receive are discarded. With `N > 0`, a closed peer is
    /// `UnexpectedEof` instead, unless the data buffer is empty: a receive with
    /// no room also returns zero bytes, so that case stays `InvalidData`.
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
        validate_stream_iov(&[io::IoSlice::new(data)], fds.len())?;
        send_fds_impl(self.as_fd(), data, fds)
    }

    fn send_fds_vectored(&self, iov: &[io::IoSlice<'_>], fds: &[impl AsFd]) -> io::Result<usize> {
        validate_stream_iov(iov, fds.len())?;
        send_fds_flags_impl(self.as_fd(), None, iov, fds, 0)
    }

    fn send_fds_all(&self, data: &[u8], fds: &[impl AsFd]) -> io::Result<()> {
        validate_stream_iov(&[io::IoSlice::new(data)], fds.len())?;
        let mut sent = if fds.is_empty() {
            0
        } else {
            send_fds_impl(self.as_fd(), data, fds).and_then(require_progress)?
        };
        while sent < data.len() {
            let n = cmsg::send_bytes(self.as_fd(), &data[sent..]).map_err(|err| {
                if err.kind() == WouldBlock && sent > 0 {
                    return io::Error::new(WouldBlock, PartialSend { sent });
                }
                err
            })?;
            if n == 0 {
                return Err(io::Error::new(WriteZero, "short write"));
            }
            sent += n;
        }
        Ok(())
    }

    recv_methods!(Stream, DEFAULT_STREAM_BUF);
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
    fn send_fds_vectored(&self, iov: &[io::IoSlice<'_>], fds: &[impl AsFd]) -> io::Result<usize>;

    /// Receive one datagram into several caller-supplied buffers, capping fds
    /// at `N`. Returns `(bytes_read, fds)`.
    fn recv_fds_vectored<const N: usize>(
        &self,
        iov: &mut [io::IoSliceMut<'_>],
    ) -> io::Result<(usize, Vec<OwnedFd>)>;

    /// Receive data and up to `N` fds.
    ///
    /// Reads into a 64 KiB per-thread buffer, then copies out what arrived and clears
    /// it. Surplus fds beyond `N` are closed
    /// automatically and counted in [`ReceivedFds::surplus_closed`]. Control
    /// storage reserves room for many descriptors, but a peer can still
    /// overflow it; that is reported as `InvalidData`.
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
    /// descriptors are still closed before the error is returned, and the
    /// bytes from that receive are discarded. A closed peer also counts as
    /// receiving zero descriptors, so with `N > 0` end of stream is this same
    /// error. Use [`recv_fds`](Self::recv_fds) to tell the two apart.
    fn recv_fds_exact<const N: usize>(&self) -> io::Result<ReceivedFds>;

    /// Like [`recv_fds_exact`](Self::recv_fds_exact) but writes data into a
    /// caller-supplied buffer. Returns `(bytes_read, fds)`.
    fn recv_fds_exact_into<const N: usize>(
        &self,
        data_buf: &mut [u8],
    ) -> io::Result<(usize, Vec<OwnedFd>)>;
}

impl UnixDatagramExt for UnixDatagram {
    fn send_fds_vectored(&self, iov: &[io::IoSlice<'_>], fds: &[impl AsFd]) -> io::Result<usize> {
        send_fds_flags_impl(self.as_fd(), None, iov, fds, 0)
    }

    fn send_fds(&self, data: &[u8], fds: &[impl AsFd]) -> io::Result<usize> {
        send_fds_impl(self.as_fd(), data, fds)
    }

    recv_methods!(Datagram, DEFAULT_DATAGRAM_BUF);
}
