//! Platform-specific glue. All `cfg` selection lives here so the rest of the
//! crate stays portable.
//!
//! The split is between platforms that support `MSG_CMSG_CLOEXEC` on
//! `recvmsg` (Linux, Android, FreeBSD, DragonFly, NetBSD, OpenBSD) and
//! everything else (notably macOS), where we must `fcntl(F_SETFD, FD_CLOEXEC)`
//! after the syscall.
//!
//! Nothing in the `fcntl` fallback ([`fallback`]) is Darwin-specific, so it is
//! compiled on every platform and unit-tested on Linux (see the tests at the
//! bottom of this file), even though only non-`MSG_CMSG_CLOEXEC` targets wire
//! it into the receive path.

/// The macOS / no-`MSG_CMSG_CLOEXEC` fallback, compiled on all platforms so it
/// stays testable on Linux CI.
#[allow(dead_code)]
pub(crate) mod fallback {
    use std::io;
    use std::os::unix::io::RawFd;

    fn set_cloexec(fd: RawFd) -> io::Result<()> {
        // SAFETY: F_SETFD on a kernel-supplied fd is always defined. A freshly
        // received descriptor has no other fd flags to preserve.
        if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Apply CLOEXEC to already-owned descriptors. The caller drops the whole
    /// batch on failure, closing processed and unprocessed descriptors alike.
    pub(crate) fn cloexec_received(
        messages: &[crate::ancillary::ReceivedMessage],
    ) -> io::Result<()> {
        for message in messages {
            cloexec_fds(message.fds())?;
        }
        Ok(())
    }

    /// Flat-list form of [`cloexec_received`], with the same batch contract.
    pub(crate) fn cloexec_fds(fds: &[std::os::fd::OwnedFd]) -> io::Result<()> {
        use std::os::fd::AsRawFd;
        for fd in fds {
            set_cloexec(fd.as_raw_fd())?;
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

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "netbsd",
    target_os = "openbsd",
))]
mod inner {
    use std::io;

    /// Flags passed to `recvmsg`. On supported platforms we ask the kernel to
    /// set `FD_CLOEXEC` atomically.
    pub(crate) const RECV_FLAGS: libc::c_int = libc::MSG_CMSG_CLOEXEC;

    /// Flags passed to `sendmsg`. `MSG_NOSIGNAL` suppresses the `SIGPIPE`
    /// that a write to a closed peer would otherwise raise, turning it into
    /// an `EPIPE` error instead.
    pub(crate) const SEND_FLAGS: libc::c_int = libc::MSG_NOSIGNAL;

    /// No-op on platforms with `MSG_NOSIGNAL` — the flag handles it.
    #[inline]
    pub(crate) fn prepare_send(_fd: std::os::unix::io::RawFd) -> io::Result<()> {
        Ok(())
    }

    /// No-op on platforms with `MSG_CMSG_CLOEXEC` — kernel handled it.
    #[inline]
    pub(crate) fn cloexec_received(
        _messages: &[crate::ancillary::ReceivedMessage],
    ) -> io::Result<()> {
        Ok(())
    }

    /// No-op on platforms with `MSG_CMSG_CLOEXEC` — kernel handled it.
    #[inline]
    pub(crate) fn cloexec_fds(_fds: &[std::os::fd::OwnedFd]) -> io::Result<()> {
        Ok(())
    }

    /// Descriptors to reserve when a receive requests `wanted`.
    ///
    /// Linux and Android enforce a hard `SCM_MAX_FD` ceiling of 253 per
    /// message, so reserving more can never prevent truncation. BSD targets
    /// keep a 253-descriptor floor and honor larger caller requests; truncation
    /// must still be handled.
    #[inline]
    pub(crate) fn recv_budget(wanted: usize) -> usize {
        if cfg!(any(target_os = "linux", target_os = "android")) {
            253
        } else {
            wanted.max(253)
        }
    }
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "netbsd",
    target_os = "openbsd",
)))]
mod inner {
    /// Flags passed to `recvmsg`. macOS et al. don't support
    /// `MSG_CMSG_CLOEXEC`; we set it via `fcntl` post-recv, accepting the
    /// brief inherit-across-exec race.
    pub(crate) const RECV_FLAGS: libc::c_int = 0;

    pub(crate) use super::fallback::{cloexec_fds, cloexec_received};

    /// Descriptors to reserve when a receive requests `wanted`.
    ///
    /// XNU refuses a control message that needs more than `MCLBYTES` once its
    /// descriptors expand to pointers (`sockargs`, bsd/kern/uipc_syscalls.c),
    /// capping a single message at 254 descriptors. Other targets without
    /// `MSG_CMSG_CLOEXEC` (illumos, Solaris, AIX) reserve `wanted` clamped to
    /// `253..=1000`, keeping control storage on the stack.
    #[inline]
    pub(crate) fn recv_budget(wanted: usize) -> usize {
        if cfg!(target_vendor = "apple") {
            254
        } else {
            wanted.clamp(253, 1000)
        }
    }

    #[cfg(target_vendor = "apple")]
    mod send_flags {
        use std::io;
        use std::mem;
        use std::os::unix::io::RawFd;

        /// Flags passed to `sendmsg` on Apple platforms. macOS lacks
        /// `MSG_NOSIGNAL`, so `SO_NOSIGPIPE` is set once per send instead
        /// (see [`prepare_send`]).
        pub(crate) const SEND_FLAGS: libc::c_int = 0;

        /// Suppress `SIGPIPE` on Apple platforms via `SO_NOSIGPIPE`. This
        /// costs one extra syscall per send because the extension traits hold
        /// no per-socket state.
        pub(crate) fn prepare_send(fd: RawFd) -> io::Result<()> {
            let on: libc::c_int = 1;
            // SAFETY: fd is a live socket; setsockopt is always defined and
            // we propagate any errno the kernel returns.
            let ret = unsafe {
                libc::setsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    libc::SO_NOSIGPIPE,
                    (&on as *const libc::c_int).cast(),
                    mem::size_of_val(&on) as libc::socklen_t,
                )
            };
            if ret < 0 {
                Err(crate::platform::fallback::nosigpipe_error(
                    io::Error::last_os_error(),
                ))
            } else {
                Ok(())
            }
        }
    }

    /// illumos and Solaris provide `MSG_NOSIGNAL` in libc (compile-checked
    /// only; not tested on those kernels). Other Unix targets (such as AIX)
    /// have no portable `sendmsg` signal suppression in libc: set `SIGPIPE`
    /// to `SIG_IGN` or handle it in your process.
    #[cfg(not(target_vendor = "apple"))]
    mod send_flags {
        use std::io;
        use std::os::unix::io::RawFd;

        #[cfg(any(target_os = "illumos", target_os = "solaris"))]
        pub(crate) const SEND_FLAGS: libc::c_int = libc::MSG_NOSIGNAL;

        #[cfg(not(any(target_os = "illumos", target_os = "solaris")))]
        pub(crate) const SEND_FLAGS: libc::c_int = 0;

        pub(crate) fn prepare_send(_fd: RawFd) -> io::Result<()> {
            Ok(())
        }
    }

    pub(crate) use send_flags::{prepare_send, SEND_FLAGS};
}

pub(crate) use inner::*;

#[cfg(test)]
mod tests {
    use super::{fallback, recv_budget};
    use std::os::unix::io::AsRawFd;

    #[test]
    fn recv_budget_respects_platform_limits() {
        assert!(recv_budget(0) >= 253);
        assert!(recv_budget(1) >= 253);
        #[cfg(any(target_os = "linux", target_os = "android"))]
        assert_eq!(recv_budget(usize::MAX), 253);
        #[cfg(target_vendor = "apple")]
        assert_eq!(recv_budget(usize::MAX), 254);
    }

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
    fn cloexec_received_sets_flag_via_fcntl() {
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
        cmsg::sendmsg_vectored(tx.as_fd(), &iov, anc.buffer, anc.length).unwrap();

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

        fallback::cloexec_received(&received).unwrap();

        let after = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        assert_eq!(
            after & libc::FD_CLOEXEC,
            libc::FD_CLOEXEC,
            "fallback must set FD_CLOEXEC"
        );

        // The received batch owns the duplicate and closes it on drop.
    }
}
