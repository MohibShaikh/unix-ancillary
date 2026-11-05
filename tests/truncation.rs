//! Ancillary truncation. Isolated in its own test binary because the leak
//! checks read the process-global fd table; tests serialize through `FD_LOCK`.

use std::io::{IoSlice, IoSliceMut};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::sync::{Mutex, MutexGuard};
use unix_ancillary::{cmsg_recvmsg, cmsg_sendmsg, AncillaryData, SocketAncillary};

static FD_LOCK: Mutex<()> = Mutex::new(());

fn fd_lock() -> MutexGuard<'static, ()> {
    FD_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn count_open_fds() -> usize {
    (0..4096)
        // SAFETY: F_GETFD on any int is defined; returns -1/EBADF if closed.
        .filter(|&fd| unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0)
        .count()
}

/// Send three descriptors into room for one. Whatever fit must be reported as
/// truncated and owned by the caller, on every kernel.
#[test]
fn undersized_buffer_reports_truncation_and_owns_what_arrived() {
    let _guard = fd_lock();
    let (tx, rx) = UnixStream::pair().unwrap();
    let file = tempfile::tempfile().unwrap();

    let mut send = vec![0u8; SocketAncillary::buffer_size_for_rights(3)];
    let mut out = SocketAncillary::new(&mut send);
    out.add_fds(&[file.as_fd(), file.as_fd(), file.as_fd()])
        .unwrap();
    cmsg_sendmsg(tx.as_fd(), &[IoSlice::new(b"x")], &out).unwrap();

    let before = count_open_fds();
    let mut storage = vec![0u8; SocketAncillary::buffer_size_for_rights(1)];
    let mut ancillary = SocketAncillary::new(&mut storage);
    let mut data = [0u8; 1];
    let result = cmsg_recvmsg(
        rx.as_fd(),
        &mut [IoSliceMut::new(&mut data)],
        &mut ancillary,
    )
    .unwrap();
    assert!(result.truncated);
    assert!(ancillary.is_truncated());

    let mut delivered = Vec::new();
    for message in ancillary.messages() {
        if let AncillaryData::ScmRights(fds) = message {
            delivered.extend(fds);
        }
    }
    assert!(
        !delivered.is_empty(),
        "delivered descriptors were not owned"
    );
    drop(delivered);

    // Apple installs descriptors it could not report; only Linux can be exact.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    assert_eq!(count_open_fds(), before, "delivered descriptors leaked");
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    let _ = before;
}

/// `SO_PASSPIDFD` from include/uapi/asm-generic/socket.h.
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
const SO_PASSPIDFD: libc::c_int = 76;

/// Fill the budget, then let the kernel add a pidfd it does not account for.
/// The convenience receive must fail and close everything that arrived.
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[test]
fn convenience_receive_rejects_truncation_without_leaking() {
    use std::os::fd::AsRawFd;
    use unix_ancillary::{set_passcred, UnixStreamExt};

    let _guard = fd_lock();
    let (tx, rx) = UnixStream::pair().unwrap();
    set_passcred(&rx, true).unwrap();
    let on: libc::c_int = 1;
    // SAFETY: correctly sized int for a boolean socket option.
    let rc = unsafe {
        libc::setsockopt(
            rx.as_raw_fd(),
            libc::SOL_SOCKET,
            SO_PASSPIDFD,
            (&on as *const libc::c_int).cast(),
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if rc != 0 {
        eprintln!("skipping: kernel lacks SO_PASSPIDFD");
        return;
    }

    let file = tempfile::tempfile().unwrap();
    let fds = vec![file.as_fd(); 253];
    tx.send_fds(b"x", &fds).unwrap();

    let before = count_open_fds();
    let mut data = [0u8; 1];
    let err = rx.recv_fds_into::<1>(&mut data).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData, "{err:?}");
    assert_eq!(
        count_open_fds(),
        before,
        "truncated receive leaked descriptors"
    );
}
