use std::marker::PhantomData;
use std::os::unix::io::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::{fmt, mem};

/// Error returned when the ancillary buffer is too small.
#[derive(Debug, Clone)]
pub struct AncillaryError;

impl fmt::Display for AncillaryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ancillary buffer too small")
    }
}

impl std::error::Error for AncillaryError {}

/// Received ancillary data from a Unix socket.
pub enum AncillaryData<'a> {
    /// File descriptors received via `SCM_RIGHTS`.
    ScmRights(ScmRights<'a>),
}

/// Iterator over file descriptors received via `SCM_RIGHTS`.
///
/// Each yielded `OwnedFd` takes ownership of one received descriptor and
/// closes it on drop.
///
/// # Important
///
/// Iterate this exactly once. Iterating the same `Messages`/`ScmRights` view
/// twice would manufacture two `OwnedFd`s for the same raw fd, leading to a
/// double-close.
pub struct ScmRights<'a> {
    data: &'a [u8],
    offset: usize,
}

impl<'a> ScmRights<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        ScmRights { data, offset: 0 }
    }
}

impl Iterator for ScmRights<'_> {
    type Item = OwnedFd;

    fn next(&mut self) -> Option<Self::Item> {
        let fd_size = mem::size_of::<RawFd>();
        if self.offset + fd_size > self.data.len() {
            return None;
        }
        let mut fd_bytes = [0u8; mem::size_of::<RawFd>()];
        fd_bytes.copy_from_slice(&self.data[self.offset..self.offset + fd_size]);
        self.offset += fd_size;
        let raw = RawFd::from_ne_bytes(fd_bytes);
        // SAFETY: the kernel just delivered this fd to us via recvmsg
        // SCM_RIGHTS; we wrap it in OwnedFd immediately and the caller owns
        // it from this point. Caller MUST iterate exactly once — see type
        // docs.
        Some(unsafe { OwnedFd::from_raw_fd(raw) })
    }
}

/// Iterator over control messages in an ancillary buffer.
pub struct Messages<'a> {
    current: *const libc::cmsghdr,
    msg: libc::msghdr,
    _marker: PhantomData<&'a [u8]>,
}

impl<'a> Messages<'a> {
    fn new(buffer: &'a [u8], length: usize) -> Self {
        // SAFETY: zeroed msghdr followed by explicit field init.
        let mut msg: libc::msghdr = unsafe { mem::zeroed() };
        msg.msg_control = buffer.as_ptr() as *mut libc::c_void;
        msg.msg_controllen = length as _;

        // SAFETY: msg.msg_control points at `buffer` for `length` bytes;
        // CMSG_FIRSTHDR walks that region per the cmsg(3) contract.
        let current = unsafe { libc::CMSG_FIRSTHDR(&msg) };

        Messages {
            current,
            msg,
            _marker: PhantomData,
        }
    }
}

impl<'a> Iterator for Messages<'a> {
    type Item = AncillaryData<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        // Loop instead of recursing on unknown cmsg types: an adversarial
        // peer could otherwise force unbounded recursion.
        loop {
            if self.current.is_null() {
                return None;
            }

            // SAFETY: `current` is a valid cmsg pointer in the borrowed
            // buffer; CMSG_DATA / CMSG_NXTHDR are defined for it.
            #[allow(clippy::unnecessary_cast)]
            // cmsg_len is size_t on Linux but socklen_t (u32) elsewhere
            let (level, ty, data_ptr, data_len) = unsafe {
                let cmsg = &*self.current;
                let data_ptr = libc::CMSG_DATA(self.current as *mut _);
                let header_len = (data_ptr as usize).saturating_sub(self.current as usize);
                let total = cmsg.cmsg_len as usize;
                // Defensive: if a malformed cmsg claims a length shorter
                // than its own header, treat the data area as empty rather
                // than wrap-around to a giant slice.
                let data_len = total.saturating_sub(header_len);
                let level = cmsg.cmsg_level;
                let ty = cmsg.cmsg_type;
                self.current = libc::CMSG_NXTHDR(&self.msg, self.current);
                (level, ty, data_ptr, data_len)
            };

            if level == libc::SOL_SOCKET && ty == libc::SCM_RIGHTS {
                // SAFETY: data_ptr/data_len are bound by the cmsg header
                // values which the kernel produced. The lifetime ties back
                // to the buffer borrowed by Messages<'a>.
                let data: &'a [u8] = unsafe { std::slice::from_raw_parts(data_ptr, data_len) };
                return Some(AncillaryData::ScmRights(ScmRights::new(data)));
            }
            // Unknown cmsg type — skip and continue walking.
        }
    }
}

/// Buffer for building and parsing Unix socket ancillary data (control
/// messages).
///
/// Used with `sendmsg`/`recvmsg` to pass file descriptors via `SCM_RIGHTS`.
pub struct SocketAncillary<'a> {
    pub(crate) buffer: &'a mut [u8],
    pub(crate) length: usize,
    pub(crate) truncated: bool,
}

impl<'a> SocketAncillary<'a> {
    /// Create a new `SocketAncillary` backed by the given buffer.
    pub fn new(buffer: &'a mut [u8]) -> Self {
        SocketAncillary {
            buffer,
            length: 0,
            truncated: false,
        }
    }

    /// Minimum buffer size needed to send `num_fds` file descriptors.
    pub fn buffer_size_for_rights(num_fds: usize) -> usize {
        // SAFETY: CMSG_SPACE is a pure inline calculation.
        unsafe { libc::CMSG_SPACE((num_fds * mem::size_of::<RawFd>()) as libc::c_uint) as usize }
    }

    /// Append file descriptors as an `SCM_RIGHTS` cmsg.
    ///
    /// `BorrowedFd` ensures the caller retains ownership of the fds.
    pub fn add_fds(&mut self, fds: &[BorrowedFd<'_>]) -> Result<(), AncillaryError> {
        let fd_bytes_len = fds.len() * mem::size_of::<RawFd>();
        // SAFETY: pure inline calculation.
        let space = unsafe { libc::CMSG_SPACE(fd_bytes_len as libc::c_uint) as usize };

        let new_len = self.length.checked_add(space).ok_or(AncillaryError)?;
        if new_len > self.buffer.len() {
            return Err(AncillaryError);
        }

        // SAFETY: we walk the buffer with cmsg(3) macros and write a single
        // cmsghdr + fd payload at the correct offset. The buffer is
        // exclusively borrowed and large enough for `new_len` bytes.
        unsafe {
            let mut msg: libc::msghdr = mem::zeroed();
            msg.msg_control = self.buffer.as_mut_ptr() as *mut libc::c_void;
            msg.msg_controllen = new_len as _;

            let cmsg = if self.length == 0 {
                libc::CMSG_FIRSTHDR(&msg)
            } else {
                let mut walk_msg: libc::msghdr = mem::zeroed();
                walk_msg.msg_control = self.buffer.as_mut_ptr() as *mut libc::c_void;
                walk_msg.msg_controllen = self.length as _;

                let mut cur = libc::CMSG_FIRSTHDR(&walk_msg);
                while !cur.is_null() {
                    let next = libc::CMSG_NXTHDR(&walk_msg, cur);
                    if next.is_null() {
                        break;
                    }
                    cur = next;
                }
                if cur.is_null() {
                    libc::CMSG_FIRSTHDR(&msg)
                } else {
                    libc::CMSG_NXTHDR(&msg, cur)
                }
            };

            if cmsg.is_null() {
                return Err(AncillaryError);
            }

            (*cmsg).cmsg_level = libc::SOL_SOCKET;
            (*cmsg).cmsg_type = libc::SCM_RIGHTS;
            (*cmsg).cmsg_len = libc::CMSG_LEN(fd_bytes_len as libc::c_uint) as _;

            // Write fds straight into the cmsg data area. `write_unaligned`
            // because `CMSG_DATA` is not guaranteed to be `RawFd`-aligned.
            let data_ptr = libc::CMSG_DATA(cmsg) as *mut RawFd;
            for (i, fd) in fds.iter().enumerate() {
                std::ptr::write_unaligned(data_ptr.add(i), fd.as_raw_fd());
            }
        }

        self.length = new_len;
        Ok(())
    }

    /// Iterate received ancillary data messages.
    ///
    /// Iterate exactly once; see [`ScmRights`].
    pub fn messages(&self) -> Messages<'_> {
        Messages::new(&self.buffer[..self.length], self.length)
    }

    /// Returns `true` if the ancillary data was truncated during receive.
    ///
    /// On platforms with `MSG_CMSG_CLOEXEC` (Linux/*BSD), truncation means
    /// extra fds were discarded by the kernel and never entered our process.
    /// On macOS, the kernel may have deposited fds beyond the buffer that
    /// this crate cannot reach — **always size the buffer for the maximum
    /// expected fd count on macOS**.
    #[must_use]
    pub fn is_truncated(&self) -> bool {
        self.truncated
    }

    /// Clear the ancillary buffer for reuse.
    pub fn clear(&mut self) {
        self.length = 0;
        self.truncated = false;
    }
}
