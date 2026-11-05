//! Platform-specific glue. All `cfg` selection lives here so the rest of the
//! crate stays portable.
//!
//! The split is between platforms that support `MSG_CMSG_CLOEXEC` on
//! `recvmsg` (Linux, Android, FreeBSD, DragonFly, NetBSD, OpenBSD) and
//! everything else (notably macOS), where we must `fcntl(F_SETFD, FD_CLOEXEC)`
//! after the syscall.
//!
//! Nothing in the `fcntl` path ([`fallback`]) is Darwin-specific, so it compiles
//! everywhere and the tests at the bottom of this file run it on Linux.

use std::io;
use std::os::unix::io::{OwnedFd, RawFd};

#[allow(dead_code)]
pub(crate) mod fallback {
    use std::io;
    use std::os::fd::{AsRawFd, OwnedFd};

    /// Apply CLOEXEC to already-owned descriptors. The caller drops the whole
    /// batch on failure, closing processed and unprocessed descriptors alike.
    pub(crate) fn cloexec_fds<'a>(fds: impl IntoIterator<Item = &'a OwnedFd>) -> io::Result<()> {
        for fd in fds {
            // SAFETY: F_SETFD on a kernel-supplied fd is always defined. A freshly
            // received descriptor has no other flags to preserve.
            if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }

    /// XNU rejects every `setsockopt` with `EINVAL` once a socket is shut down
    /// in both directions, which is what a closed peer leaves behind. A send
    /// would have failed with `EPIPE`, so report that. Not ignored: on a socket
    /// without `SO_NOSIGPIPE` the send could raise `SIGPIPE`.
    pub(crate) fn nosigpipe_error(err: io::Error) -> io::Error {
        if err.raw_os_error() == Some(libc::EINVAL) {
            io::Error::from_raw_os_error(libc::EPIPE)
        } else {
            err
        }
    }
}

/// `MSG_CMSG_CLOEXEC` sets `FD_CLOEXEC` atomically. `MSG_NOSIGNAL` turns the
/// `SIGPIPE` of a write to a closed peer into `EPIPE`.
#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "netbsd",
    target_os = "openbsd",
))]
mod flags {
    pub(crate) const RECV_FLAGS: libc::c_int = libc::MSG_CMSG_CLOEXEC;
    pub(crate) const SEND_FLAGS: libc::c_int = libc::MSG_NOSIGNAL;
}

/// macOS and others lack both. Close-on-exec comes from `fcntl` after the receive,
/// accepting the brief inherit-across-exec race. Apple suppresses `SIGPIPE` through
/// [`prepare_send`]. Other targets (illumos, Solaris, AIX) have no portable way, so
/// their applications must ignore or handle `SIGPIPE`.
#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "netbsd",
    target_os = "openbsd",
)))]
mod flags {
    pub(crate) const RECV_FLAGS: libc::c_int = 0;
    pub(crate) const SEND_FLAGS: libc::c_int = 0;
}

pub(crate) use flags::{RECV_FLAGS, SEND_FLAGS};

/// Set close-on-exec on received descriptors, if the kernel has not already.
pub(crate) fn cloexec_fds<'a>(fds: impl IntoIterator<Item = &'a OwnedFd>) -> io::Result<()> {
    if RECV_FLAGS == 0 {
        return fallback::cloexec_fds(fds);
    }
    Ok(())
}

/// Suppress `SIGPIPE` through `SO_NOSIGPIPE`. This costs one extra syscall per
/// send because the extension traits hold no per-socket state.
#[cfg(target_vendor = "apple")]
pub(crate) fn prepare_send(fd: RawFd) -> io::Result<()> {
    let on: libc::c_int = 1;
    // SAFETY: fd is a live socket; setsockopt is always defined and we propagate
    // any errno the kernel returns.
    let ret = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_NOSIGPIPE,
            (&on as *const libc::c_int).cast(),
            std::mem::size_of_val(&on) as libc::socklen_t,
        )
    };
    if ret < 0 {
        return Err(fallback::nosigpipe_error(io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(not(target_vendor = "apple"))]
pub(crate) fn prepare_send(_fd: RawFd) -> io::Result<()> {
    Ok(())
}

/// Descriptors to make room for when a receive wants `wanted`.
///
/// Linux and Android carry at most `SCM_MAX_FD`, 253, per message. XNU refuses a
/// control message that needs more than `MCLBYTES` once its descriptors expand to
/// pointers (`sockargs`, bsd/kern/uipc_syscalls.c), which is 254. Other kernels'
/// limits are unverified, so honor the request as far as stack storage goes. More
/// than that surfaces as a truncation error, not as a leak.
pub(crate) fn recv_budget(wanted: usize) -> usize {
    if cfg!(any(target_os = "linux", target_os = "android")) {
        253
    } else if cfg!(target_vendor = "apple") {
        254
    } else {
        wanted.clamp(253, 1000)
    }
}

#[cfg(test)]
mod tests {
    use super::fallback;
    use std::os::unix::io::AsRawFd;

    #[test]
    fn nosigpipe_einval_becomes_broken_pipe() {
        use std::io::{Error, ErrorKind};
        let mapped = fallback::nosigpipe_error(Error::from_raw_os_error(libc::EINVAL));
        assert_eq!(mapped.kind(), ErrorKind::BrokenPipe);
        let other = fallback::nosigpipe_error(Error::from_raw_os_error(libc::ENOTSOCK));
        assert_eq!(other.raw_os_error(), Some(libc::ENOTSOCK));
    }

    /// Exercise the exact macOS post-recv path on Linux: receive an fd with
    /// `MSG_CMSG_CLOEXEC` suppressed (so it arrives WITHOUT close-on-exec),
    /// then run the fcntl fallback and confirm `FD_CLOEXEC` is now set.
    #[test]
    fn cloexec_fds_sets_flag_via_fcntl() {
        use crate::ancillary::SocketAncillary;
        use crate::cmsg;
        use std::io::{IoSlice, IoSliceMut};
        use std::os::unix::io::AsFd;
        use std::os::unix::net::UnixStream;

        let (tx, rx) = UnixStream::pair().unwrap();
        let file = tempfile::tempfile().unwrap();

        let mut sbuf = vec![0u8; SocketAncillary::buffer_size_for_rights(1)];
        let mut anc = SocketAncillary::new(&mut sbuf);
        anc.add_fds(&[file.as_fd()]).unwrap();
        let iov = [IoSlice::new(b"x")];
        cmsg::sendmsg_flags(tx.as_fd(), None, &iov, anc.buffer, anc.length, 0).unwrap();

        // Raw recvmsg with flags = 0 (NOT MSG_CMSG_CLOEXEC) so the delivered
        // fd is inheritable — the state a macOS kernel always hands back.
        let mut data = [0u8; 8];
        let mut anc_buf = vec![0u8; SocketAncillary::buffer_size_for_rights(1)];
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        let mut riov = [IoSliceMut::new(&mut data)];
        msg.msg_iov = riov.as_mut_ptr() as *mut libc::iovec;
        msg.msg_iovlen = riov.len() as _;
        msg.msg_control = anc_buf.as_mut_ptr() as *mut libc::c_void;
        msg.msg_controllen = anc_buf.len() as _;
        let ret = unsafe { libc::recvmsg(rx.as_fd().as_raw_fd(), &mut msg, 0) };
        assert!(ret >= 0);
        let anc_len = msg.msg_controllen as usize;

        // SAFETY: this test alone owns the fresh recvmsg output.
        let received = unsafe { crate::ancillary::take_received(&anc_buf[..anc_len]) };
        let fds: Vec<_> = received.iter().flat_map(|message| message.fds()).collect();
        assert_eq!(fds.len(), 1);
        let fd = fds[0].as_raw_fd();

        // Before the fallback runs, the fd is inheritable.
        let before = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        assert_eq!(before & libc::FD_CLOEXEC, 0, "arrived with CLOEXEC unset");

        fallback::cloexec_fds(received.iter().flat_map(|message| message.fds())).unwrap();

        let after = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        assert_eq!(
            after & libc::FD_CLOEXEC,
            libc::FD_CLOEXEC,
            "fallback must set FD_CLOEXEC"
        );

        // The received batch owns the duplicate and closes it on drop.
    }
}
