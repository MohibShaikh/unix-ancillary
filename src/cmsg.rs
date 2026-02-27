use std::io;
use std::os::unix::io::BorrowedFd;

pub(crate) struct RecvMsgResult {
    pub bytes_read: usize,
    pub ancillary_len: usize,
    pub truncated: bool,
}

/// Returns platform-appropriate flags for recvmsg.
#[inline]
fn recv_flags() -> libc::c_int {
    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "dragonfly",
        target_os = "netbsd",
        target_os = "openbsd",
    ))]
    {
        libc::MSG_CMSG_CLOEXEC
    }

    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "dragonfly",
        target_os = "netbsd",
        target_os = "openbsd",
    )))]
    {
        0
    }
}

/// Send data and ancillary control messages over a Unix socket.
pub(crate) fn sendmsg_vectored(
    fd: BorrowedFd<'_>,
    iov: &[io::IoSlice<'_>],
    ancillary_buf: &[u8],
    ancillary_len: usize,
) -> io::Result<usize> {
    use std::os::unix::io::AsRawFd;

    unsafe {
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = iov.as_ptr() as *mut libc::iovec;
        msg.msg_iovlen = iov.len() as _;

        if ancillary_len > 0 {
            msg.msg_control = ancillary_buf.as_ptr() as *mut libc::c_void;
            msg.msg_controllen = ancillary_len as _;
        }

        let ret = libc::sendmsg(fd.as_raw_fd(), &msg, 0);
        if ret < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(ret as usize)
        }
    }
}

/// Receive data and ancillary control messages from a Unix socket.
pub(crate) fn recvmsg_vectored(
    fd: BorrowedFd<'_>,
    iov: &mut [io::IoSliceMut<'_>],
    ancillary_buf: &mut [u8],
) -> io::Result<RecvMsgResult> {
    use std::os::unix::io::AsRawFd;

    unsafe {
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = iov.as_mut_ptr() as *mut libc::iovec;
        msg.msg_iovlen = iov.len() as _;
        msg.msg_control = ancillary_buf.as_mut_ptr() as *mut libc::c_void;
        msg.msg_controllen = ancillary_buf.len() as _;

        let flags = recv_flags();

        let ret = libc::recvmsg(fd.as_raw_fd(), &mut msg, flags);
        if ret < 0 {
            Err(io::Error::last_os_error())
        } else {
            #[allow(clippy::unnecessary_cast)]
            let ancillary_len = msg.msg_controllen as usize;

            // On platforms without MSG_CMSG_CLOEXEC, set CLOEXEC on received fds
            #[cfg(not(any(
                target_os = "linux",
                target_os = "android",
                target_os = "freebsd",
                target_os = "dragonfly",
                target_os = "netbsd",
                target_os = "openbsd",
            )))]
            {
                use crate::ancillary::{set_cloexec, AncillaryData, SocketAncillary};
                // Walk the ancillary data and set CLOEXEC on all received fds
                // We need to do this before wrapping in OwnedFd
                let temp = SocketAncillary {
                    buffer: &mut ancillary_buf[..ancillary_len],
                    length: ancillary_len,
                    truncated: false,
                };
                for msg in temp.messages() {
                    match msg {
                        AncillaryData::ScmRights(rights) => {
                            for fd in rights {
                                use std::os::unix::io::AsRawFd;
                                // Ignore errors — best effort
                                let _ = set_cloexec(fd.as_raw_fd());
                                // Leak the OwnedFd — we don't want to close it here,
                                // the caller will re-parse and take ownership
                                std::mem::forget(fd);
                            }
                        }
                    }
                }
            }

            Ok(RecvMsgResult {
                bytes_read: ret as usize,
                ancillary_len,
                truncated: (msg.msg_flags & libc::MSG_CTRUNC) != 0,
            })
        }
    }
}
