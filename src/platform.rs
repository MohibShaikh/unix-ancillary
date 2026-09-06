//! Platform-specific glue. All `cfg` selection lives here so the rest of the
//! crate stays portable.
//!
//! The split is between platforms that support `MSG_CMSG_CLOEXEC` on
//! `recvmsg` (Linux, Android, FreeBSD, DragonFly, NetBSD, OpenBSD) and
//! everything else (notably macOS), where we must `fcntl(F_SETFD, FD_CLOEXEC)`
//! after the syscall.
//!
//! The macOS fallback ([`fallback`]) is written against plain `fcntl`/
//! `getrlimit`, which behave identically on Linux — nothing in it is
//! Darwin-specific, it is merely *selected out* on kernels that offer
//! `MSG_CMSG_CLOEXEC`. It is compiled on every platform so its logic can be
//! unit-tested anywhere (see the tests at the bottom of this file), even
//! though only non-`MSG_CMSG_CLOEXEC` targets wire it into the recv path.

/// The macOS / no-`MSG_CMSG_CLOEXEC` fallback, compiled on all platforms so it
/// stays testable on Linux CI. Only `mod inner` on those targets calls it; on
/// `MSG_CMSG_CLOEXEC` platforms it is reached solely from the unit tests, so
/// suppress dead-code warnings there.
#[cfg_attr(
    any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "dragonfly",
        target_os = "netbsd",
        target_os = "openbsd",
    ),
    allow(dead_code)
)]
pub(crate) mod fallback {
    use std::io;
    use std::mem;
    use std::os::unix::io::RawFd;

    fn set_cloexec(fd: RawFd) -> io::Result<()> {
        // SAFETY: F_GETFD/F_SETFD on a kernel-supplied fd is always defined;
        // we propagate any errno the kernel returns.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags < 0 {
                return Err(io::Error::last_os_error());
            }
            let ret = libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
            if ret < 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }

    /// Apply CLOEXEC to already-owned descriptors. The caller drops the whole
    /// batch on failure, closing processed and unprocessed descriptors alike.
    pub(crate) fn cloexec_received(
        messages: &[crate::ancillary::ReceivedMessage],
    ) -> io::Result<()> {
        use std::os::fd::AsRawFd;
        for fd in messages.iter().flat_map(|message| message.fds()) {
            set_cloexec(fd.as_raw_fd())?;
        }
        Ok(())
    }

    /// Hard ceiling on the dynamic cap to bound buffer size against bogus or
    /// `RLIM_INFINITY` values. 1M fds × 4 bytes ≈ 4 MiB cmsg buffer — far
    /// used as an allocation budget, not a proof against kernel truncation.
    pub(crate) const HARD_CEILING: usize = 1 << 20;

    /// Floor — never go below the Linux-style 253 cap so behaviour stays
    /// consistent across platforms when `getrlimit` returns nonsense.
    pub(crate) const HARD_FLOOR: usize = 253;

    /// Clamp a raw `RLIMIT_NOFILE` current value into `[HARD_FLOOR,
    /// HARD_CEILING]`, mapping `RLIM_INFINITY` and any unrepresentable value
    /// to the ceiling. Pure — the buffer-sizing policy, split out so it is
    /// testable without a live `getrlimit`.
    pub(crate) fn clamp_nofile(cur: libc::rlim_t) -> usize {
        let n: usize = if cur == libc::RLIM_INFINITY {
            HARD_CEILING
        } else {
            usize::try_from(cur).unwrap_or(HARD_CEILING)
        };
        n.clamp(HARD_FLOOR, HARD_CEILING)
    }

    /// Receive allocation budget derived from RLIMIT_NOFILE and clamped to
    /// bound per-call memory use. Reported truncation remains an error.
    pub(crate) fn max_recv_fds() -> usize {
        // SAFETY: getrlimit with a writable rlimit out-pointer is always
        // defined; we treat any failure as "fall back to a safe default".
        let mut rlim: libc::rlimit = unsafe { mem::zeroed() };
        let rc = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut rlim) };
        if rc < 0 {
            return HARD_CEILING;
        }
        clamp_nofile(rlim.rlim_cur)
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

    /// Linux/Android SCM_MAX_FD and the default receive budget on BSD.
    /// BSD kernel limits may differ; truncation must still be handled.
    #[inline]
    pub(crate) fn max_recv_fds() -> usize {
        253
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

    pub(crate) use super::fallback::{cloexec_received, max_recv_fds};

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
        /// no per-socket state; `FdChannel` will own its socket and can hoist
        /// this to construction.
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
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        }
    }

    /// Unknown Unix targets (illumos, Solaris, AIX) have no portable
    /// `sendmsg` signal suppression in libc. Sends are not `SIGPIPE`-safe;
    /// set `SIGPIPE` to `SIG_IGN` (or handle it) in your process, or the
    /// kernel may terminate it on a write to a closed peer.
    #[cfg(not(target_vendor = "apple"))]
    mod send_flags {
        use std::io;
        use std::os::unix::io::RawFd;

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
    use super::fallback;
    use std::os::unix::io::AsRawFd;

    #[test]
    fn clamp_nofile_maps_special_and_out_of_range_values() {
        assert_eq!(fallback::clamp_nofile(0), fallback::HARD_FLOOR);
        assert_eq!(fallback::clamp_nofile(10), fallback::HARD_FLOOR);
        assert_eq!(fallback::clamp_nofile(1024), 1024);
        assert_eq!(
            fallback::clamp_nofile(libc::RLIM_INFINITY),
            fallback::HARD_CEILING,
        );
        assert_eq!(
            fallback::clamp_nofile(u64::MAX as libc::rlim_t),
            fallback::HARD_CEILING,
        );
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
