//! Descriptor passing to and from an address, for sockets that are not connected.

use std::ffi::OsStr;
use std::io;
use std::mem;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::AsFd;
use std::os::unix::net::SocketAddr;

#[cfg(target_os = "android")]
use std::os::android::net::SocketAddrExt;
#[cfg(target_os = "linux")]
use std::os::linux::net::SocketAddrExt;

use crate::ext::{recv_slots, send_fds_flags_impl, RecvSlots};

/// A Unix socket address in the form `msghdr` takes.
pub(crate) struct RawAddr {
    pub(crate) sun: libc::sockaddr_un,
    pub(crate) len: libc::socklen_t,
}

/// Where `sun_path` starts: two bytes of family, or of length and family, on every
/// target libc has a `sockaddr_un` for.
const PATH_OFFSET: usize = 2;

impl RawAddr {
    /// Room for the kernel to write a sender's address.
    pub(crate) fn unset() -> Self {
        RawAddr {
            // SAFETY: all zero is a valid sockaddr_un, which is integers and bytes.
            sun: unsafe { mem::zeroed() },
            len: mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
        }
    }

    pub(crate) fn from_std(addr: &SocketAddr) -> io::Result<Self> {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        let name = addr.as_abstract_name();
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        let name: Option<&[u8]> = None;
        // A path ends in a NUL and an abstract name starts with one, both already
        // there from zeroing. std sized both to fit when it built `addr`.
        let (bytes, skip, nul) = match (addr.as_pathname(), name) {
            (Some(path), _) => (path.as_os_str().as_bytes(), 0, 1),
            (None, Some(name)) => (name, 1, 0),
            (None, None) => {
                let msg = "cannot send to an unnamed socket address";
                return Err(io::Error::new(io::ErrorKind::InvalidInput, msg));
            }
        };
        let mut raw = Self::unset();
        raw.sun.sun_family = libc::AF_UNIX as libc::sa_family_t;
        for (slot, byte) in raw.sun.sun_path[skip..].iter_mut().zip(bytes) {
            *slot = *byte as libc::c_char;
        }
        let used = PATH_OFFSET + skip + bytes.len() + nul;
        raw.len = used.min(mem::size_of::<libc::sockaddr_un>()) as libc::socklen_t;
        Ok(raw)
    }

    /// The address the kernel wrote. `None` for an unnamed sender.
    pub(crate) fn to_std(&self) -> Option<SocketAddr> {
        let used = (self.len as usize).checked_sub(PATH_OFFSET)?;
        let used = used.min(self.sun.sun_path.len());
        // SAFETY: c_char and u8 have the same layout, and `used` is in bounds.
        let path =
            unsafe { std::slice::from_raw_parts(self.sun.sun_path.as_ptr().cast::<u8>(), used) };
        #[cfg(any(target_os = "linux", target_os = "android"))]
        if let [0, name @ ..] = path {
            return SocketAddr::from_abstract_name(name).ok();
        }
        let end = path.iter().position(|&b| b == 0).unwrap_or(path.len());
        (end > 0)
            .then(|| SocketAddr::from_pathname(OsStr::from_bytes(&path[..end])).ok())
            .flatten()
    }
}

/// Send one datagram of `data` plus borrowed descriptors to `addr`.
///
/// For a socket that is not connected, such as one notifying a service manager.
/// Build the address with `SocketAddr::from_pathname` or, on Linux and Android,
/// `SocketAddrExt::from_abstract_name`. An unnamed address is `InvalidInput`.
/// On tokio, run it inside `async_io`, as shown on [`crate::recv_fds_slots`].
pub fn send_fds_to(
    sock: &impl AsFd,
    addr: &SocketAddr,
    data: &[u8],
    fds: &[impl AsFd],
) -> io::Result<usize> {
    let raw = RawAddr::from_std(addr)?;
    let iov = [io::IoSlice::new(data)];
    send_fds_flags_impl(sock.as_fd(), Some(&raw), &iov, fds, 0)
}

/// Receive one datagram into caller storage, along with the sender's address.
///
/// Behaves like [`crate::recv_fds_slots`]. The address is `None` when the sender
/// has no name, which is the usual case unless it called `bind`. It is also `None`
/// for a path that fills `sun_path` with no terminator, which std cannot represent.
pub fn recv_fds_from(
    sock: &impl AsFd,
    data_buf: &mut [u8],
    slots: &mut [Option<std::os::fd::OwnedFd>],
) -> io::Result<(RecvSlots, Option<SocketAddr>)> {
    let mut raw = RawAddr::unset();
    let mut iov = [io::IoSliceMut::new(data_buf)];
    let got = recv_slots(sock.as_fd(), &mut iov, slots, 0, Some(&mut raw))?;
    Ok((got, raw.to_std()))
}
