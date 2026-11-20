//! XNU's per-message descriptor limit, which sizes the macOS receive budget.
//! Runs only on Apple kernels, where a wrong assumption would show up as
//! truncation of a message the budget claims to cover.
#![cfg(target_vendor = "apple")]

use std::os::unix::net::UnixStream;
use unix_ancillary::UnixStreamExt;

fn raise_fd_limit() {
    let mut limit: libc::rlimit = unsafe { std::mem::zeroed() };
    // SAFETY: valid out-pointer and in-pointer for RLIMIT_NOFILE.
    unsafe {
        assert_eq!(libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit), 0);
        limit.rlim_cur = limit.rlim_max.min(4096);
        assert_eq!(libc::setrlimit(libc::RLIMIT_NOFILE, &limit), 0);
    }
}

#[test]
fn largest_message_fits_and_queued_messages_do_not_merge() {
    raise_fd_limit();
    let (tx, rx) = UnixStream::pair().unwrap();
    let file = tempfile::tempfile().unwrap();
    let fds = vec![&file; 254];

    tx.send_fds(b"a", &fds).unwrap();
    tx.send_fds(b"b", &fds).unwrap();

    let mut data = [0u8; 8];
    let (n, first) = rx.recv_fds_into::<254>(&mut data).unwrap();
    assert_eq!((n, data[0], first.len()), (1, b'a', 254));
    drop(first);
    let (n, second) = rx.recv_fds_into::<254>(&mut data).unwrap();
    assert_eq!((n, data[0], second.len()), (1, b'b', 254));
}

#[test]
fn kernel_refuses_one_descriptor_past_the_budget() {
    raise_fd_limit();
    let (tx, _rx) = UnixStream::pair().unwrap();
    let file = tempfile::tempfile().unwrap();
    assert!(tx.send_fds(b"a", &vec![&file; 255]).is_err());
}
