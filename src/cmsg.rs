use std::io;
use std::mem;
use std::os::unix::io::{AsRawFd, BorrowedFd};

use crate::platform;

pub(crate) struct RecvMsgResult {
    pub bytes_read: usize,
    pub ancillary_len: usize,
    pub ancillary_truncated: bool,
    pub data_truncated: bool,
    pub messages: Vec<crate::ancillary::ReceivedMessage>,
}

/// Send ordinary bytes without ancillary data, using the same signal-safe
/// flags and EINTR behavior as descriptor sends.
pub(crate) fn send_bytes(fd: BorrowedFd<'_>, data: &[u8]) -> io::Result<usize> {
    let iov = [io::IoSlice::new(data)];
    sendmsg_vectored(fd, &iov, &[], 0)
}

/// Send data and ancillary control messages over a Unix socket.
pub(crate) fn sendmsg_vectored(
    fd: BorrowedFd<'_>,
    iov: &[io::IoSlice<'_>],
    ancillary_buf: &[u8],
    ancillary_len: usize,
) -> io::Result<usize> {
    // `mem::zeroed::<msghdr>()` is sound on every libc target: every field is
    // either explicitly initialised below or unused for `sendmsg` (msg_name*,
    // msg_flags), and any padding bytes are inert.
    let mut msg: libc::msghdr = unsafe { mem::zeroed() };
    msg.msg_iov = iov.as_ptr() as *mut libc::iovec;
    msg.msg_iovlen = iov.len() as _;

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
        let ret = unsafe { libc::sendmsg(fd.as_raw_fd(), &msg, platform::SEND_FLAGS) };
        if ret >= 0 {
            return Ok(ret as usize);
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

/// Receive data and ancillary control messages from a Unix socket.
///
/// On platforms without `MSG_CMSG_CLOEXEC`, sets `FD_CLOEXEC` on every
/// received fd before returning. If that fails for any fd, all received fds
/// are closed and the error is returned — callers never see partial state.
pub(crate) fn recvmsg_vectored(
    fd: BorrowedFd<'_>,
    iov: &mut [io::IoSliceMut<'_>],
    ancillary_buf: &mut [u8],
) -> io::Result<RecvMsgResult> {
    // SAFETY: same justification as sendmsg_vectored.
    let mut msg: libc::msghdr = unsafe { mem::zeroed() };
    msg.msg_iov = iov.as_mut_ptr() as *mut libc::iovec;
    msg.msg_iovlen = iov.len() as _;
    msg.msg_control = ancillary_buf.as_mut_ptr() as *mut libc::c_void;
    msg.msg_controllen = ancillary_buf.len() as _;

    // SAFETY: msghdr fully populated; ancillary_buf borrowed mutably for the
    // syscall duration; fd is a valid descriptor.
    let ret = loop {
        let ret = unsafe { libc::recvmsg(fd.as_raw_fd(), &mut msg, platform::RECV_FLAGS) };
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

    #[allow(clippy::unnecessary_cast)]
    let ancillary_len = msg.msg_controllen as usize;
    let ancillary_truncated = (msg.msg_flags & libc::MSG_CTRUNC) != 0;
    let data_truncated = (msg.msg_flags & libc::MSG_TRUNC) != 0;

    // SAFETY: this is fresh, kernel-produced recvmsg output, consumed once.
    // Establish ownership before fallible CLOEXEC processing so an error drops
    // every delivered descriptor, including those not yet processed.
    let messages = unsafe { crate::ancillary::take_received(&ancillary_buf[..ancillary_len]) };
    platform::cloexec_received(&messages)?;

    Ok(RecvMsgResult {
        bytes_read: ret as usize,
        ancillary_len,
        ancillary_truncated,
        data_truncated,
        messages,
    })
}
