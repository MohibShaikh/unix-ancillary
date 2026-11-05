use std::os::unix::io::{AsRawFd, BorrowedFd, OwnedFd};
use std::{io, mem};

use crate::addr::RawAddr;
use crate::platform;

pub(crate) struct RecvMsgResult {
    pub bytes_read: usize,
    pub ancillary_len: usize,
    pub ancillary_truncated: bool,
    pub data_truncated: bool,
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub credentials: Option<crate::ScmCredentials>,
}

/// Send ordinary bytes without ancillary data, using the same signal-safe
/// flags and EINTR behavior as descriptor sends.
pub(crate) fn send_bytes(fd: BorrowedFd<'_>, data: &[u8]) -> io::Result<usize> {
    sendmsg_flags(fd, None, &[io::IoSlice::new(data)], &[], 0, 0)
}

/// Send data and ancillary control messages over a Unix socket.
pub(crate) fn sendmsg_flags(
    fd: BorrowedFd<'_>,
    addr: Option<&RawAddr>,
    iov: &[io::IoSlice<'_>],
    ancillary_buf: &[u8],
    ancillary_len: usize,
    extra_flags: libc::c_int,
) -> io::Result<usize> {
    // `mem::zeroed::<msghdr>()` is sound on every libc target: every field is
    // either explicitly initialised below or unused for `sendmsg` (msg_flags),
    // and any padding bytes are inert.
    let mut msg: libc::msghdr = unsafe { mem::zeroed() };
    msg.msg_iov = iov.as_ptr() as *mut libc::iovec;
    msg.msg_iovlen = iov.len() as _;
    if let Some(addr) = addr {
        msg.msg_name = (&addr.sun as *const libc::sockaddr_un).cast_mut().cast();
        msg.msg_namelen = addr.len;
    }

    if ancillary_len > 0 {
        msg.msg_control = ancillary_buf.as_ptr() as *mut libc::c_void;
        msg.msg_controllen = ancillary_len as _;
    }

    // SAFETY: msghdr fully populated; iov and ancillary_buf borrowed for the
    // syscall duration; fd is a valid descriptor.
    platform::prepare_send(fd.as_raw_fd())?;
    loop {
        // SAFETY: same justification as above; SEND_FLAGS is defined by
        // platform.rs for every target.
        let ret =
            unsafe { libc::sendmsg(fd.as_raw_fd(), &msg, platform::SEND_FLAGS | extra_flags) };
        if ret >= 0 {
            return Ok(ret as usize);
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

/// Receive into caller storage, leaving descriptors unowned in `ancillary_buf`.
/// The caller must take ownership before doing anything fallible.
pub(crate) fn recvmsg_raw(
    fd: BorrowedFd<'_>,
    iov: &mut [io::IoSliceMut<'_>],
    ancillary_buf: &mut [u8],
    extra_flags: libc::c_int,
    mut addr: Option<&mut RawAddr>,
) -> io::Result<RecvMsgResult> {
    // SAFETY: same justification as sendmsg_flags.
    let mut msg: libc::msghdr = unsafe { mem::zeroed() };
    msg.msg_iov = iov.as_mut_ptr() as *mut libc::iovec;
    msg.msg_iovlen = iov.len() as _;
    msg.msg_control = ancillary_buf.as_mut_ptr() as *mut libc::c_void;
    msg.msg_controllen = ancillary_buf.len() as _;
    if let Some(addr) = addr.as_deref_mut() {
        msg.msg_name = (&mut addr.sun as *mut libc::sockaddr_un).cast();
        msg.msg_namelen = addr.len;
    }

    // SAFETY: msghdr fully populated; ancillary_buf borrowed mutably for the
    // syscall duration; fd is a valid descriptor.
    let ret = loop {
        let ret =
            unsafe { libc::recvmsg(fd.as_raw_fd(), &mut msg, platform::RECV_FLAGS | extra_flags) };
        if ret >= 0 {
            break ret;
        }
        let err = io::Error::last_os_error();
        // Retry interrupted syscalls, but surface `WouldBlock` unchanged so
        // readiness adapters can retry when the socket becomes ready.
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    };

    if let Some(addr) = addr {
        addr.len = msg.msg_namelen;
    }
    #[allow(clippy::unnecessary_cast)]
    Ok(RecvMsgResult {
        bytes_read: ret as usize,
        ancillary_len: msg.msg_controllen as usize,
        ancillary_truncated: (msg.msg_flags & libc::MSG_CTRUNC) != 0,
        data_truncated: (msg.msg_flags & libc::MSG_TRUNC) != 0,
        #[cfg(any(target_os = "linux", target_os = "android"))]
        credentials: crate::ancillary::credentials(&ancillary_buf[..msg.msg_controllen as usize]),
    })
}

/// Receive and own every delivered descriptor as one flat list.
///
/// On platforms without `MSG_CMSG_CLOEXEC`, sets `FD_CLOEXEC` on every
/// received fd before returning. If that fails for any fd, all received fds
/// are closed and the error is returned — callers never see partial state.
pub(crate) fn recv_fds(
    fd: BorrowedFd<'_>,
    iov: &mut [io::IoSliceMut<'_>],
    ancillary_buf: &mut [u8],
) -> io::Result<(RecvMsgResult, Vec<OwnedFd>)> {
    let result = recvmsg_raw(fd, iov, ancillary_buf, 0, None)?;
    // SAFETY: this is fresh, kernel-produced recvmsg output, consumed once.
    // Establish ownership before fallible CLOEXEC processing so an error drops
    // every delivered descriptor, including those not yet processed.
    let control = &ancillary_buf[..result.ancillary_len];
    let mut fds = Vec::new();
    unsafe {
        crate::ancillary::for_each_fd(control, |fd| {
            if fds.is_empty() {
                // Bytes over-count by the headers, which is a few slots at most.
                fds.reserve_exact(control.len() / mem::size_of::<std::os::fd::RawFd>());
            }
            fds.push(fd);
        })
    };
    platform::cloexec_fds(&fds)?;
    Ok((result, fds))
}

/// Like [`recv_fds`], but fill `slots` and close the rest. Returns the result,
/// the number kept and the number closed. On error every slot is empty.
pub(crate) fn recv_fds_slots(
    fd: BorrowedFd<'_>,
    iov: &mut [io::IoSliceMut<'_>],
    ancillary_buf: &mut [u8],
    slots: &mut [Option<OwnedFd>],
    flags: libc::c_int,
    addr: Option<&mut RawAddr>,
) -> io::Result<(RecvMsgResult, usize, usize)> {
    slots.fill_with(|| None);
    let result = recvmsg_raw(fd, iov, ancillary_buf, flags, addr)?;
    let (mut kept, mut surplus) = (0, 0);
    // SAFETY: as in recv_fds. Each descriptor is owned as soon as it is seen.
    unsafe {
        crate::ancillary::for_each_fd(&ancillary_buf[..result.ancillary_len], |fd| {
            match slots.get_mut(kept) {
                Some(slot) => {
                    *slot = Some(fd);
                    kept += 1;
                }
                None => surplus += 1,
            }
        });
    }
    if let Err(err) = platform::cloexec_fds(slots.iter().flatten()) {
        slots.fill_with(|| None);
        return Err(err);
    }
    Ok((result, kept, surplus))
}
