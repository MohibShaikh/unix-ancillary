//! Isolated in its own test binary: `count_open_fds` reads the process-global
//! fd table, so it must not run concurrently with other fd-mutating tests.
//! Cargo compiles each `tests/*.rs` to a separate process, giving this test a
//! private fd table.

use std::os::unix::net::UnixStream;
use unix_ancillary::UnixStreamExt;

/// Count currently-open fds in this process by probing the low fd range.
/// Portable across Linux/macOS (no /proc dependency).
fn count_open_fds() -> usize {
    (0..1024)
        // SAFETY: F_GETFD on any int is defined; returns -1/EBADF if closed.
        .filter(|&fd| unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0)
        .count()
}

#[test]
fn surplus_fds_dropped_not_leaked() {
    let (tx, rx) = UnixStream::pair().unwrap();

    let f1 = tempfile::tempfile().unwrap();
    let f2 = tempfile::tempfile().unwrap();
    let f3 = tempfile::tempfile().unwrap();

    tx.send_fds(b"trunc", &[&f1, &f2, &f3]).unwrap();

    // Caller asks for one fd; peer sent three. The cmsg buffer is sized to
    // the kernel cap so all three are wrapped in OwnedFd. We keep one and
    // drop the surplus, closing those two fds.
    //
    // tx/rx and f1..f3 all live in this process, so recv allocates three
    // *new* fd numbers. Keeping one and closing two must raise the process
    // open-fd count by exactly one — proving the surplus are closed, not
    // merely omitted from the returned Vec.
    let before = count_open_fds();
    let recv = rx.recv_fds::<1>().unwrap();
    let after = count_open_fds();

    assert_eq!(recv.fds.len(), 1);
    assert_eq!(&recv.data[..], b"trunc");
    assert_eq!(
        after - before,
        1,
        "expected exactly the kept fd to remain open; surplus fds leaked"
    );
}
