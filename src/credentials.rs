//! Peer credentials carried as `SCM_CREDENTIALS` control messages.
//!
//! Linux and Android only. These are ancillary data: the kernel puts them in
//! the same cmsg buffer as `SCM_RIGHTS`, so one `recvmsg` returns descriptors
//! and credentials together.
//!
//! Not to be confused with `SO_PEERCRED`, a `getsockopt` that answers "who
//! connected" once for the whole connection. `SCM_CREDENTIALS` is per message
//! and needs `SO_PASSCRED` on the receiver. Most code that wants to identify a
//! peer wants `SO_PEERCRED`; reach for this when each message needs its own
//! attribution, such as a socket several processes write to.
//!
//! BSD `SCM_CREDS` is deliberately absent. It is sent implicitly rather than
//! attached, and modelling it as the same variant would describe semantics
//! that platform does not have.

use std::io;
use std::mem;
use std::os::fd::{AsFd, AsRawFd};

/// Credentials of the process that sent a message.
///
/// The kernel validates these on send. An unprivileged sender may only claim
/// its own pid and its real, effective or saved uid and gid; anything else
/// needs `CAP_SYS_ADMIN` for the pid or `CAP_SETUID` / `CAP_SETGID` for the
/// ids. So a received value is trustworthy in a way the payload is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScmCredentials {
    pid: libc::pid_t,
    uid: libc::uid_t,
    gid: libc::gid_t,
}

impl ScmCredentials {
    /// The values this process may always send without privileges.
    ///
    /// Use this unless you specifically need to claim something else. The
    /// kernel checks the values on send: an unprivileged process may only
    /// claim its own pid, and a uid and gid that are its real, effective or
    /// saved ones. Anything else needs `CAP_SYS_ADMIN` for the pid, or
    /// `CAP_SETUID` and `CAP_SETGID` for the ids.
    ///
    /// A rejected claim fails the whole `sendmsg` with `EPERM`, surfacing as
    /// [`io::ErrorKind::PermissionDenied`](std::io::ErrorKind::PermissionDenied)
    /// on the send call rather than as an error from [`new`](Self::new). If a
    /// send starts failing with `PermissionDenied` after you switched away
    /// from this constructor, that is why.
    pub fn for_this_process() -> Self {
        // SAFETY: these three calls cannot fail and touch no memory.
        unsafe {
            ScmCredentials {
                pid: libc::getpid(),
                uid: libc::getuid(),
                gid: libc::getgid(),
            }
        }
    }

    /// Claim specific values.
    ///
    /// Nothing is validated here. The kernel checks on send and fails the
    /// whole `sendmsg` with `EPERM`, seen as
    /// [`io::ErrorKind::PermissionDenied`](std::io::ErrorKind::PermissionDenied),
    /// if this process may not make the claim. See
    /// [`for_this_process`](Self::for_this_process) for what is always allowed.
    pub fn new(pid: libc::pid_t, uid: libc::uid_t, gid: libc::gid_t) -> Self {
        ScmCredentials { pid, uid, gid }
    }

    /// Process id of the sender.
    ///
    /// Zero when the sender is in a pid namespace the receiver cannot
    /// translate. Pids are recycled, so use this for logs rather than for
    /// authorization.
    pub fn pid(&self) -> libc::pid_t {
        self.pid
    }

    /// User id of the sender.
    pub fn uid(&self) -> libc::uid_t {
        self.uid
    }

    /// Group id of the sender.
    pub fn gid(&self) -> libc::gid_t {
        self.gid
    }

    pub(crate) fn from_ucred(c: libc::ucred) -> Self {
        ScmCredentials {
            pid: c.pid,
            uid: c.uid,
            gid: c.gid,
        }
    }

    pub(crate) fn to_ucred(self) -> libc::ucred {
        libc::ucred {
            pid: self.pid,
            uid: self.uid,
            gid: self.gid,
        }
    }

    pub(crate) const SIZE: usize = mem::size_of::<libc::ucred>();
}

/// Turn `SO_PASSCRED` on or off for a socket.
///
/// A free function rather than an extension-trait method on purpose. std has an
/// unstable inherent `UnixStream::set_passcred`, and a trait method of the same
/// name collides with it: today that is an `unstable_name_collisions` warning,
/// and if std stabilizes it the inherent method silently wins. Taking `AsFd`
/// also means this works on a tokio socket or a bare `OwnedFd`, which a method
/// on the std types would not.
///
/// Credentials only arrive on a socket that has this set, and it must be set
/// before the message is received. Once on, the kernel attaches the sender's
/// credentials to every message whether or not the sender asked, so this is
/// usually all a receiver needs.
pub fn set_passcred<T: AsFd>(sock: &T, on: bool) -> io::Result<()> {
    let value: libc::c_int = i32::from(on);
    // SAFETY: passing a correctly sized int for a documented boolean option.
    let rc = unsafe {
        libc::setsockopt(
            sock.as_fd().as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PASSCRED,
            (&value as *const libc::c_int).cast(),
            mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Whether `SO_PASSCRED` is on.
pub fn passcred<T: AsFd>(sock: &T) -> io::Result<bool> {
    let mut value: libc::c_int = 0;
    let mut len = mem::size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: buffer and length match the option's documented size.
    let rc = unsafe {
        libc::getsockopt(
            sock.as_fd().as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PASSCRED,
            (&mut value as *mut libc::c_int).cast(),
            &mut len,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(value != 0)
}
