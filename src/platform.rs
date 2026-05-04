//! Platform-specific glue. All `cfg` selection lives here so the rest of the
//! crate stays portable.
//!
//! The split is between platforms that support `MSG_CMSG_CLOEXEC` on
//! `recvmsg` (Linux, Android, FreeBSD, DragonFly, NetBSD, OpenBSD) and
//! everything else (notably macOS), where we must `fcntl(F_SETFD, FD_CLOEXEC)`
//! after the syscall.

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

    /// No-op on platforms with `MSG_CMSG_CLOEXEC` — kernel handled it.
    #[inline]
    pub(crate) fn cloexec_received(_buf: &[u8]) -> io::Result<()> {
        Ok(())
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
    use std::io;
    use std::mem;
    use std::os::unix::io::RawFd;

    /// Flags passed to `recvmsg`. macOS et al. don't support
    /// `MSG_CMSG_CLOEXEC`; we set it via `fcntl` post-recv, accepting the
    /// brief inherit-across-exec race.
    pub(crate) const RECV_FLAGS: libc::c_int = 0;

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

    /// Walk the kernel-formatted ancillary buffer and emit each `SCM_RIGHTS`
    /// fd as a raw value, without taking ownership.
    fn raw_fds_in_buffer(buf: &[u8]) -> Vec<RawFd> {
        let mut out = Vec::new();
        if buf.is_empty() {
            return out;
        }

        // SAFETY: zeroed msghdr followed by explicit field init.
        let mut msg: libc::msghdr = unsafe { mem::zeroed() };
        msg.msg_control = buf.as_ptr() as *mut libc::c_void;
        msg.msg_controllen = buf.len() as _;

        // SAFETY: msg points to `buf` for `buf.len()` bytes; CMSG_FIRSTHDR
        // and CMSG_NXTHDR walk that buffer per the cmsg(3) protocol.
        let mut cur = unsafe { libc::CMSG_FIRSTHDR(&msg) };
        while !cur.is_null() {
            // SAFETY: cur is a valid cmsg pointer inside `buf`.
            #[allow(clippy::unnecessary_cast)]
            // cmsg_len is size_t on Linux but socklen_t (u32) elsewhere
            unsafe {
                let cmsg = &*cur;
                if cmsg.cmsg_level == libc::SOL_SOCKET && cmsg.cmsg_type == libc::SCM_RIGHTS {
                    let data_ptr = libc::CMSG_DATA(cur as *mut _);
                    let header_len = (data_ptr as usize).saturating_sub(cur as usize);
                    let total = cmsg.cmsg_len as usize;
                    let data_len = total.saturating_sub(header_len);
                    let n = data_len / mem::size_of::<RawFd>();
                    let fd_ptr = data_ptr as *const RawFd;
                    for i in 0..n {
                        out.push(std::ptr::read_unaligned(fd_ptr.add(i)));
                    }
                }
                cur = libc::CMSG_NXTHDR(&msg, cur);
            }
        }
        out
    }

    /// Set `FD_CLOEXEC` on every fd present in the buffer. On any failure,
    /// closes every fd found (whether or not we already CLOEXEC'd it) so we
    /// never return partial state to the caller.
    pub(crate) fn cloexec_received(buf: &[u8]) -> io::Result<()> {
        let fds = raw_fds_in_buffer(buf);
        for &raw in &fds {
            if let Err(e) = set_cloexec(raw) {
                // Close everything we found. Earlier fds already have
                // CLOEXEC set but are still owned by us with no path to the
                // caller; later fds may still be inheritable. Either way,
                // closing prevents leaks.
                for &all in &fds {
                    // SAFETY: each fd was just delivered to us by the kernel.
                    unsafe {
                        libc::close(all);
                    }
                }
                return Err(e);
            }
        }
        Ok(())
    }
}

pub(crate) use inner::*;
