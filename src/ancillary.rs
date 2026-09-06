use std::marker::PhantomData;
use std::os::unix::io::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::{fmt, mem};

/// The ancillary buffer is too small, or must be cleared before building a send.
#[derive(Debug, Clone)]
pub struct AncillaryError;

impl fmt::Display for AncillaryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ancillary buffer too small or not cleared after receive")
    }
}
impl std::error::Error for AncillaryError {}

/// Received ancillary data from a Unix socket.
#[non_exhaustive]
pub enum AncillaryData<'a> {
    /// Owned descriptors. Unconsumed descriptors close when the iterator drops.
    ScmRights(ScmRights<'a>),
    /// Kernel-validated sender credentials; requires [`crate::set_passcred`].
    #[cfg(any(target_os = "linux", target_os = "android"))]
    Credentials(crate::ScmCredentials),
}

/// Draining iterator over received descriptors. Each descriptor is yielded once.
/// Dropping this iterator closes every descriptor not yet yielded.
pub struct ScmRights<'a> {
    inner: std::vec::IntoIter<OwnedFd>,
    _marker: PhantomData<&'a ()>,
}

impl Iterator for ScmRights<'_> {
    type Item = OwnedFd;
    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next()
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

pub(crate) enum ReceivedMessage {
    Rights(Vec<OwnedFd>),
    #[cfg(any(target_os = "linux", target_os = "android"))]
    Credentials(crate::ScmCredentials),
}

impl ReceivedMessage {
    pub(crate) fn fds(&self) -> &[OwnedFd] {
        match self {
            Self::Rights(fds) => fds,
            #[cfg(any(target_os = "linux", target_os = "android"))]
            Self::Credentials(_) => &[],
        }
    }
}

/// Drains received control messages. Dropping it closes unconsumed descriptors.
pub struct Messages<'a> {
    inner: std::vec::Drain<'a, ReceivedMessage>,
}

impl<'a> Iterator for Messages<'a> {
    type Item = AncillaryData<'a>;
    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|message| match message {
            ReceivedMessage::Rights(fds) => AncillaryData::ScmRights(ScmRights {
                inner: fds.into_iter(),
                _marker: PhantomData,
            }),
            #[cfg(any(target_os = "linux", target_os = "android"))]
            ReceivedMessage::Credentials(creds) => AncillaryData::Credentials(creds),
        })
    }
}

fn header_len() -> usize {
    // SAFETY: pure calculation with a zero payload, no pointers or overflow.
    unsafe { libc::CMSG_LEN(0) as usize }
}

fn cmsg_space(payload_len: usize) -> Option<usize> {
    // Derive the target's ABI alignment from libc, but do length arithmetic
    // in checked usize operations instead of narrowing a caller length to u32.
    let alignment = unsafe { (libc::CMSG_SPACE(1) - libc::CMSG_SPACE(0)) as usize };
    let padded = payload_len.checked_add(alignment - 1)? / alignment * alignment;
    header_len().checked_add(padded)
}

struct RawMessage<'a> {
    level: libc::c_int,
    kind: libc::c_int,
    data: &'a [u8],
}

impl RawMessage<'_> {
    fn rights(&self) -> impl Iterator<Item = RawFd> + '_ {
        self.data
            .chunks_exact(mem::size_of::<RawFd>())
            .map(|bytes| RawFd::from_ne_bytes(bytes.try_into().expect("exact fd-sized chunk")))
            .filter(|&fd| fd >= 0)
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn credentials(&self) -> Option<crate::ScmCredentials> {
        if self.level != libc::SOL_SOCKET
            || self.kind != libc::SCM_CREDENTIALS
            || self.data.len() < crate::ScmCredentials::SIZE
        {
            return None;
        }
        // SAFETY: ucred consists of integer fields, and the whole value fits.
        // read_unaligned imposes no alignment requirement on the byte slice.
        let cred = unsafe { self.data.as_ptr().cast::<libc::ucred>().read_unaligned() };
        Some(crate::ScmCredentials::from_ucred(cred))
    }
}

/// Byte parser only: never creates ownership from untrusted input.
struct RawMessages<'a> {
    remaining: &'a [u8],
}

impl<'a> Iterator for RawMessages<'a> {
    type Item = RawMessage<'a>;
    fn next(&mut self) -> Option<Self::Item> {
        let bytes = mem::take(&mut self.remaining);
        if bytes.len() < mem::size_of::<libc::cmsghdr>() {
            return None;
        }
        // SAFETY: a whole integer-only header fits. No aligned reference is
        // formed, including for caller-provided offset byte slices.
        let header = unsafe { bytes.as_ptr().cast::<libc::cmsghdr>().read_unaligned() };
        #[allow(clippy::unnecessary_cast)]
        let total = header.cmsg_len as usize;
        if total < header_len() || total > bytes.len() {
            return None;
        }
        let data = &bytes[header_len()..total];
        if let Some(next) = cmsg_space(data.len()).filter(|&next| next <= bytes.len()) {
            self.remaining = &bytes[next..];
        }
        Some(RawMessage {
            level: header.cmsg_level,
            kind: header.cmsg_type,
            data,
        })
    }
}

/// Own descriptors from one fresh successful recvmsg, before any fallible
/// postprocessing. Never call with send buffers or previously consumed bytes.
///
/// # Safety
/// Every SCM_RIGHTS integer must be an open descriptor newly owned by the caller.
pub(crate) unsafe fn take_received(buffer: &[u8]) -> Vec<ReceivedMessage> {
    let mut messages = Vec::new();
    for message in (RawMessages { remaining: buffer }) {
        if message.level == libc::SOL_SOCKET && message.kind == libc::SCM_RIGHTS {
            let fds = message
                .rights()
                .map(|raw| {
                    // SAFETY: guaranteed by this function's fresh-recv contract.
                    unsafe { OwnedFd::from_raw_fd(raw) }
                })
                .collect();
            messages.push(ReceivedMessage::Rights(fds));
        }
        #[cfg(any(target_os = "linux", target_os = "android"))]
        if let Some(creds) = message.credentials() {
            messages.push(ReceivedMessage::Credentials(creds));
        }
    }
    messages
}

/// Caller-provided storage for sending or receiving ancillary messages.
///
/// Send descriptors remain borrowed until this buffer is dropped. Received
/// descriptors are owned before [`crate::cmsg_recvmsg`] returns. Use
/// [`messages`](Self::messages) to drain them; unread descriptors close on
/// clear, the next receive (including a failed receive), or drop.
///
/// After receiving, call [`clear`](Self::clear) before building a new send.
/// Construct a separate send buffer when forwarding received descriptors.
pub struct SocketAncillary<'a> {
    pub(crate) buffer: &'a mut [u8],
    pub(crate) length: usize,
    pub(crate) truncated: bool,
    pub(crate) received: Vec<ReceivedMessage>,
    pub(crate) receive_mode: bool,
    borrowed: Vec<BorrowedFd<'a>>,
}

impl<'a> SocketAncillary<'a> {
    /// Create empty storage. Any byte-slice alignment is accepted.
    pub fn new(buffer: &'a mut [u8]) -> Self {
        Self {
            buffer,
            length: 0,
            truncated: false,
            received: Vec::new(),
            receive_mode: false,
            borrowed: Vec::new(),
        }
    }

    /// Buffer size for one SCM_RIGHTS message containing `num_fds` descriptors.
    /// Panics if the size cannot be represented by usize.
    pub fn buffer_size_for_rights(num_fds: usize) -> usize {
        num_fds
            .checked_mul(mem::size_of::<RawFd>())
            .and_then(cmsg_space)
            .expect("ancillary buffer size overflow")
    }

    /// Additional space for one SCM_CREDENTIALS message.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub fn buffer_size_for_credentials() -> usize {
        cmsg_space(crate::ScmCredentials::SIZE).expect("credentials fit in usize")
    }

    /// Append credentials. The kernel validates the claim on send.
    /// [`crate::ScmCredentials::for_this_process`] supplies permitted values.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub fn add_credentials(&mut self, creds: crate::ScmCredentials) -> Result<(), AncillaryError> {
        let cred = creds.to_ucred();
        // ucred is three consecutive integer fields without padding on Linux/Android.
        let bytes = unsafe {
            std::slice::from_raw_parts(
                (&cred as *const libc::ucred).cast::<u8>(),
                crate::ScmCredentials::SIZE,
            )
        };
        self.add_cmsg(libc::SOL_SOCKET, libc::SCM_CREDENTIALS, bytes)
    }

    fn add_cmsg(
        &mut self,
        level: libc::c_int,
        kind: libc::c_int,
        payload: &[u8],
    ) -> Result<(), AncillaryError> {
        if self.receive_mode {
            return Err(AncillaryError);
        }
        let space = cmsg_space(payload.len()).ok_or(AncillaryError)?;
        let end = self.length.checked_add(space).ok_or(AncillaryError)?;
        if end > self.buffer.len() {
            return Err(AncillaryError);
        }
        let length = header_len()
            .checked_add(payload.len())
            .ok_or(AncillaryError)?;
        #[allow(clippy::useless_conversion)]
        let cmsg_len = length.try_into().map_err(|_| AncillaryError)?;
        let bytes = &mut self.buffer[self.length..end];
        bytes.fill(0);
        let header = bytes.as_mut_ptr().cast::<libc::cmsghdr>();
        // SAFETY: space reserves the whole header and payload. Form only raw
        // field pointers and write unaligned. Writing fields individually keeps
        // header padding initialized instead of copying Rust struct padding.
        unsafe {
            std::ptr::addr_of_mut!((*header).cmsg_level).write_unaligned(level);
            std::ptr::addr_of_mut!((*header).cmsg_type).write_unaligned(kind);
            std::ptr::addr_of_mut!((*header).cmsg_len).write_unaligned(cmsg_len);
        }
        bytes[header_len()..length].copy_from_slice(payload);
        self.length = end;
        Ok(())
    }

    /// Append borrowed descriptors, retaining their lifetimes in this buffer.
    /// The caller keeps ownership. Clear a received buffer before adding data.
    ///
    /// ```compile_fail,E0505
    /// use std::os::fd::AsFd;
    /// use unix_ancillary::SocketAncillary;
    /// let file = std::fs::File::open("/dev/null").unwrap();
    /// let mut buf = [0u8; 128];
    /// let mut ancillary = SocketAncillary::new(&mut buf);
    /// ancillary.add_fds(&[file.as_fd()]).unwrap();
    /// drop(file);
    /// ancillary.clear();
    /// ```
    pub fn add_fds(&mut self, fds: &[BorrowedFd<'a>]) -> Result<(), AncillaryError> {
        let raw: Vec<RawFd> = fds.iter().map(|f| f.as_raw_fd()).collect();
        // SAFETY: RawFd is an integer without padding.
        let bytes = unsafe {
            std::slice::from_raw_parts(raw.as_ptr().cast::<u8>(), mem::size_of_val(&raw[..]))
        };
        self.add_cmsg(libc::SOL_SOCKET, libc::SCM_RIGHTS, bytes)?;
        self.borrowed.extend_from_slice(fds);
        Ok(())
    }

    /// Drain received messages exactly once. Send buffers yield no messages.
    /// Dropping the iterator closes all unconsumed received descriptors.
    pub fn messages(&mut self) -> Messages<'_> {
        Messages {
            inner: self.received.drain(..),
        }
    }

    /// Whether recvmsg reported MSG_CTRUNC. Delivered descriptors are still
    /// owned and cleaned up, but the complete ancillary message was not received.
    #[must_use]
    pub fn is_truncated(&self) -> bool {
        self.truncated
    }

    /// Close unread received descriptors and reset storage for a new send.
    /// Sender borrows remain conservatively tied to this object's lifetime;
    /// drop and reconstruct it to release that lifetime constraint.
    pub fn clear(&mut self) {
        self.received.clear();
        self.borrowed.clear();
        self.length = 0;
        self.truncated = false;
        self.receive_mode = false;
    }
}

/// Exercise the byte parser without manufacturing descriptors from fuzz input.
/// Hidden, unstable harness entry point; returns the number of parsed messages.
#[doc(hidden)]
pub fn __fuzz_parse(buf: &[u8]) -> usize {
    let mut count = 0;
    for message in (RawMessages { remaining: buf }) {
        if message.level == libc::SOL_SOCKET && message.kind == libc::SCM_RIGHTS {
            std::hint::black_box(message.rights().count());
        }
        #[cfg(any(target_os = "linux", target_os = "android"))]
        std::hint::black_box(message.credentials());
        count += 1;
    }
    count
}
