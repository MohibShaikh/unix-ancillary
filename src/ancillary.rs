use std::marker::PhantomData;
use std::mem::MaybeUninit;
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

impl ExactSizeIterator for ScmRights<'_> {}

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
    // Reject lengths whose aligned size could overflow CMSG_SPACE's u32.
    let payload_len = u32::try_from(payload_len)
        .ok()
        .filter(|&n| n <= i32::MAX as u32)?;
    // SAFETY: pure calculation, no pointers; the bound above rules out overflow.
    Some(unsafe { libc::CMSG_SPACE(payload_len) } as usize)
}

/// Stack space for control messages. Covers Linux's whole receive budget, so
/// the common paths never touch the heap.
const STACK_CONTROL: usize = 4096;

/// Run `f` with `size` zeroed bytes of cmsghdr-aligned control storage.
pub(crate) fn with_control<R>(size: usize, f: impl FnOnce(&mut [u8]) -> R) -> R {
    #[repr(align(8))]
    struct Stack(MaybeUninit<[u8; STACK_CONTROL]>);

    if size > STACK_CONTROL {
        return f(&mut vec![0u8; size]);
    }
    let mut stack = Stack(MaybeUninit::uninit());
    let start = stack.0.as_mut_ptr().cast::<u8>();
    // SAFETY: size fits the array, and those bytes are zeroed before the slice
    // is formed. Zeroing only `size` keeps a one-fd send from paying for 4 KiB.
    let buffer = unsafe {
        start.write_bytes(0, size);
        std::slice::from_raw_parts_mut(start, size)
    };
    f(buffer)
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
        // Apple keeps the full cmsg_len on a truncated message while copying
        // only what fit. Those descriptors are installed, so parse what arrived.
        let end = total.min(bytes.len());
        if end < header_len() {
            return None;
        }
        let data = &bytes[header_len()..end];
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

/// `SCM_PIDFD` from include/linux/socket.h; libc 0.2 does not export it.
#[cfg(any(target_os = "linux", target_os = "android"))]
const SCM_PIDFD: libc::c_int = 0x04;

/// SO_PASSPIDFD (Linux 6.5+) makes the kernel install a pidfd with every
/// message. It is not exposed, so close it here.
///
/// # Safety
/// Same fresh-recv contract as [`take_received_into`].
#[cfg(any(target_os = "linux", target_os = "android"))]
unsafe fn close_pidfd(message: &RawMessage<'_>) {
    if message.level == libc::SOL_SOCKET && message.kind == SCM_PIDFD {
        for raw in message.rights() {
            // SAFETY: guaranteed by this function's fresh-recv contract.
            drop(unsafe { OwnedFd::from_raw_fd(raw) });
        }
    }
}

/// Own descriptors from one fresh successful recvmsg, before any fallible
/// postprocessing. Never call with send buffers or previously consumed bytes.
///
/// # Safety
/// Every SCM_RIGHTS and SCM_PIDFD integer must be an open descriptor newly
/// owned by the caller.
pub(crate) unsafe fn take_received_into(buffer: &[u8], messages: &mut Vec<ReceivedMessage>) {
    for message in (RawMessages { remaining: buffer }) {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        // SAFETY: guaranteed by this function's fresh-recv contract.
        unsafe {
            close_pidfd(&message)
        };
        if message.level == libc::SOL_SOCKET && message.kind == libc::SCM_RIGHTS {
            let mut fds = Vec::with_capacity(message.data.len() / mem::size_of::<RawFd>());
            fds.extend(message.rights().map(|raw| {
                // SAFETY: guaranteed by this function's fresh-recv contract.
                unsafe { OwnedFd::from_raw_fd(raw) }
            }));
            messages.push(ReceivedMessage::Rights(fds));
        }
        #[cfg(any(target_os = "linux", target_os = "android"))]
        if let Some(creds) = message.credentials() {
            messages.push(ReceivedMessage::Credentials(creds));
        }
    }
}

#[cfg(test)]
pub(crate) unsafe fn take_received(buffer: &[u8]) -> Vec<ReceivedMessage> {
    let mut messages = Vec::new();
    // SAFETY: the caller upholds the fresh-recv contract.
    unsafe { take_received_into(buffer, &mut messages) };
    messages
}

/// Own every delivered descriptor, handing each to `sink` in order. Allocates
/// nothing. Same purpose and contract as [`take_received_into`].
///
/// # Safety
/// Every SCM_RIGHTS and SCM_PIDFD integer must be an open descriptor newly
/// owned by the caller.
pub(crate) unsafe fn for_each_fd(buffer: &[u8], mut sink: impl FnMut(OwnedFd)) {
    for message in (RawMessages { remaining: buffer }) {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        // SAFETY: guaranteed by this function's fresh-recv contract.
        unsafe {
            close_pidfd(&message)
        };
        if message.level == libc::SOL_SOCKET && message.kind == libc::SCM_RIGHTS {
            for raw in message.rights() {
                // SAFETY: guaranteed by this function's fresh-recv contract.
                sink(unsafe { OwnedFd::from_raw_fd(raw) });
            }
        }
    }
}

/// The number of descriptors [`for_each_fd`] would yield, for sizing a `Vec`.
pub(crate) fn count_rights(buffer: &[u8]) -> usize {
    (RawMessages { remaining: buffer })
        .filter(|message| message.level == libc::SOL_SOCKET && message.kind == libc::SCM_RIGHTS)
        .map(|message| message.rights().count())
        .sum()
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
    borrowed: PhantomData<BorrowedFd<'a>>,
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
            borrowed: PhantomData,
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
        self.reserve_cmsg(libc::SOL_SOCKET, libc::SCM_CREDENTIALS, bytes.len())?
            .copy_from_slice(bytes);
        Ok(())
    }

    /// Write a control header and return its zeroed payload for the caller to fill.
    fn reserve_cmsg(
        &mut self,
        level: libc::c_int,
        kind: libc::c_int,
        payload_len: usize,
    ) -> Result<&mut [u8], AncillaryError> {
        if self.receive_mode {
            return Err(AncillaryError);
        }
        let space = cmsg_space(payload_len).ok_or(AncillaryError)?;
        let end = self.length.checked_add(space).ok_or(AncillaryError)?;
        if end > self.buffer.len() {
            return Err(AncillaryError);
        }
        let length = header_len()
            .checked_add(payload_len)
            .ok_or(AncillaryError)?;
        #[allow(clippy::useless_conversion)]
        let cmsg_len = length.try_into().map_err(|_| AncillaryError)?;
        let start = self.length;
        self.length = end;
        let bytes = &mut self.buffer[start..end];
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
        Ok(&mut bytes[header_len()..length])
    }

    /// Encode descriptor numbers directly into the buffer. An empty list writes
    /// nothing, so it can never produce a bare SCM_RIGHTS header.
    pub(crate) fn add_rights(
        &mut self,
        fds: impl ExactSizeIterator<Item = RawFd>,
    ) -> Result<(), AncillaryError> {
        if self.receive_mode {
            return Err(AncillaryError);
        }
        if fds.len() == 0 {
            return Ok(());
        }
        let len = fds
            .len()
            .checked_mul(mem::size_of::<RawFd>())
            .ok_or(AncillaryError)?;
        let payload = self.reserve_cmsg(libc::SOL_SOCKET, libc::SCM_RIGHTS, len)?;
        for (slot, fd) in payload.chunks_exact_mut(mem::size_of::<RawFd>()).zip(fds) {
            slot.copy_from_slice(&fd.to_ne_bytes());
        }
        Ok(())
    }

    /// Append borrowed descriptors, retaining their lifetimes in this buffer.
    /// The caller keeps ownership. Clear a received buffer before adding data.
    /// On Apple platforms, supply all descriptors in one call: the kernel
    /// rejects sends containing multiple SCM_RIGHTS headers.
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
        self.add_rights(fds.iter().map(|f| f.as_raw_fd()))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::IntoRawFd;

    /// Apple reports the full `cmsg_len` of a truncated message but copies only
    /// what fit. Descriptors in the copied part must still become owned.
    #[test]
    fn oversized_cmsg_len_owns_the_descriptors_that_fit() {
        let raw = tempfile::tempfile().unwrap().into_raw_fd();
        // SAFETY: raw stays open until the OwnedFd below closes it.
        let fd = unsafe { BorrowedFd::borrow_raw(raw) };
        let mut storage = vec![0u8; SocketAncillary::buffer_size_for_rights(3)];
        let mut ancillary = SocketAncillary::new(&mut storage);
        ancillary.add_fds(&[fd, fd, fd]).unwrap();
        let fits = header_len() + mem::size_of::<RawFd>();

        // SAFETY: only the first encoded descriptor is inside the slice, and
        // this test owns it.
        let mut messages = unsafe { take_received(&ancillary.buffer[..fits]) };

        assert_eq!(messages.len(), 1);
        let fds = messages[0].fds();
        assert_eq!(fds.len(), 1);
        assert_eq!(fds[0].as_raw_fd(), raw);
        messages.clear();
        // SAFETY: F_GETFD on any int is defined.
        assert_eq!(unsafe { libc::fcntl(raw, libc::F_GETFD) }, -1);
    }

    #[test]
    fn partial_descriptor_bytes_are_ignored() {
        let mut storage = vec![0u8; SocketAncillary::buffer_size_for_rights(2)];
        let mut ancillary = SocketAncillary::new(&mut storage);
        // SAFETY: the value is never used as a descriptor; the slice below
        // holds less than one encoded fd.
        let fd = unsafe { BorrowedFd::borrow_raw(0) };
        ancillary.add_fds(&[fd, fd]).unwrap();
        let cut = header_len() + mem::size_of::<RawFd>() - 1;
        // SAFETY: no whole descriptor is inside the slice.
        let messages = unsafe { take_received(&ancillary.buffer[..cut]) };
        assert!(messages.iter().all(|message| message.fds().is_empty()));
    }

    #[test]
    fn adding_no_descriptors_writes_no_header() {
        let mut storage = [0u8; 64];
        let mut ancillary = SocketAncillary::new(&mut storage);
        ancillary.add_fds(&[]).unwrap();
        assert_eq!(ancillary.length, 0);
    }

    #[test]
    fn cmsg_space_matches_libc() {
        for payload in [0usize, 1, 4, 8, 12, 253 * 4] {
            // SAFETY: pure calculation.
            let expected = unsafe { libc::CMSG_SPACE(payload as u32) } as usize;
            assert_eq!(cmsg_space(payload), Some(expected));
        }
    }
}
