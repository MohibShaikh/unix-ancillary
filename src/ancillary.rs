use std::marker::PhantomData;
use std::os::unix::io::{BorrowedFd, FromRawFd, OwnedFd, RawFd};
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
    /// A set of file descriptors received via `SCM_RIGHTS`.
    ScmRights(ScmRights<'a>),
}

/// Iterator over file descriptors received via `SCM_RIGHTS`.
///
/// Each returned `OwnedFd` takes ownership of the received file descriptor
/// and will close it on drop.
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
        // SAFETY: The kernel just gave us this fd via recvmsg SCM_RIGHTS.
        // We wrap it in OwnedFd immediately so it will be closed on drop.
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
        let mut msg: libc::msghdr = unsafe { mem::zeroed() };
        msg.msg_control = buffer.as_ptr() as *mut libc::c_void;
        msg.msg_controllen = length as _;

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
        if self.current.is_null() {
            return None;
        }

        unsafe {
            let cmsg = &*self.current;
            self.current = libc::CMSG_NXTHDR(&self.msg, self.current);

            if cmsg.cmsg_level == libc::SOL_SOCKET && cmsg.cmsg_type == libc::SCM_RIGHTS {
                let data_ptr = libc::CMSG_DATA(cmsg as *const _ as *mut _);
                #[allow(clippy::unnecessary_cast)]
                let data_len =
                    cmsg.cmsg_len as usize - (data_ptr as usize - cmsg as *const _ as usize);
                let data = std::slice::from_raw_parts(data_ptr, data_len);
                Some(AncillaryData::ScmRights(ScmRights::new(data)))
            } else {
                // Skip unknown cmsg types
                self.next()
            }
        }
    }
}

/// Buffer for building and parsing Unix socket ancillary data (control messages).
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

    /// Returns the minimum buffer size needed to send `num_fds` file descriptors.
    pub fn buffer_size_for_rights(num_fds: usize) -> usize {
        unsafe { libc::CMSG_SPACE((num_fds * mem::size_of::<RawFd>()) as libc::c_uint) as usize }
    }

    /// Add file descriptors to be sent via `SCM_RIGHTS`.
    ///
    /// Uses `BorrowedFd` to ensure the caller retains ownership of the FDs.
    pub fn add_fds(&mut self, fds: &[BorrowedFd<'_>]) -> Result<(), AncillaryError> {
        // Convert BorrowedFd slice to raw fd slice for the kernel
        let raw_fds: Vec<RawFd> = fds.iter().map(|fd| {
            use std::os::unix::io::AsRawFd;
            fd.as_raw_fd()
        }).collect();

        let fd_bytes_len = raw_fds.len() * mem::size_of::<RawFd>();
        let space = unsafe { libc::CMSG_SPACE(fd_bytes_len as libc::c_uint) as usize };

        if self.length + space > self.buffer.len() {
            return Err(AncillaryError);
        }

        unsafe {
            let mut msg: libc::msghdr = mem::zeroed();
            msg.msg_control = self.buffer.as_mut_ptr() as *mut libc::c_void;
            msg.msg_controllen = (self.length + space) as _;

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
                msg.msg_controllen = (self.length + space) as _;
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

            let data_ptr = libc::CMSG_DATA(cmsg);
            std::ptr::copy_nonoverlapping(
                raw_fds.as_ptr() as *const u8,
                data_ptr,
                fd_bytes_len,
            );
        }

        self.length += space;
        Ok(())
    }

    /// Iterate over received ancillary data messages.
    pub fn messages(&self) -> Messages<'_> {
        Messages::new(&self.buffer[..self.length], self.length)
    }

    /// Returns `true` if the ancillary data was truncated during receive.
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

/// On drop, if the ancillary data was truncated, attempt to close any FDs
/// that the kernel may have added to our process but didn't deliver in the buffer.
///
/// This is primarily needed on macOS where the kernel adds FDs to the process
/// FD table even when the ancillary buffer is too small to hold them.
impl Drop for SocketAncillary<'_> {
    fn drop(&mut self) {
        // On macOS, if MSG_CTRUNC is set, the kernel may have leaked FDs into our
        // process. Consume all visible FDs so they get closed via OwnedFd::drop.
        #[cfg(target_os = "macos")]
        if self.truncated {
            for msg in self.messages() {
                match msg {
                    AncillaryData::ScmRights(rights) => {
                        for _fd in rights {
                            // OwnedFd dropped here, closing the fd
                        }
                    }
                }
            }
        }
    }
}

/// Platform-specific: set CLOEXEC on an fd via fcntl.
/// Used on platforms without MSG_CMSG_CLOEXEC (macOS).
#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "netbsd",
    target_os = "openbsd",
)))]
pub(crate) fn set_cloexec(fd: RawFd) -> io::Result<()> {
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
