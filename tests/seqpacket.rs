//! `AF_UNIX` `SOCK_SEQPACKET` descriptor passing. Linux only here: macOS has no
//! such socket, and CI runs no BSD kernel.
#![cfg(target_os = "linux")]

use std::io::{Read, Seek, Write};
use std::os::fd::{FromRawFd, OwnedFd};
use unix_ancillary::seqpacket;

fn pair(extra_flags: libc::c_int) -> (OwnedFd, OwnedFd) {
    let mut fds = [0; 2];
    // SAFETY: fds has room for the two descriptors socketpair writes.
    let rc = unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC | extra_flags,
            0,
            fds.as_mut_ptr(),
        )
    };
    assert_eq!(rc, 0);
    // SAFETY: socketpair just returned these and nothing else owns them.
    unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
}

#[test]
fn messages_keep_their_boundaries_and_descriptors() {
    let (a, b) = pair(0);
    let mut first = tempfile::tempfile().unwrap();
    first.write_all(b"first").unwrap();
    let second = tempfile::tempfile().unwrap();

    seqpacket::send_fds(&a, b"one", &[&first]).unwrap();
    seqpacket::send_fds(&a, b"two", &[&second]).unwrap();

    let mut buf = [0u8; 16];
    let (n, mut fds) = seqpacket::recv_fds_into::<1>(&b, &mut buf).unwrap();
    assert_eq!(&buf[..n], b"one");
    let mut got = std::fs::File::from(fds.remove(0));
    got.rewind().unwrap();
    let mut text = String::new();
    got.read_to_string(&mut text).unwrap();
    assert_eq!(text, "first");

    let (n, fds) = seqpacket::recv_fds_exact_into::<1>(&b, &mut buf).unwrap();
    assert_eq!(&buf[..n], b"two");
    assert_eq!(fds.len(), 1);
}

#[test]
fn empty_payload_carries_descriptors() {
    let (a, b) = pair(0);
    let file = tempfile::tempfile().unwrap();
    assert_eq!(seqpacket::send_fds(&a, b"", &[&file]).unwrap(), 0);
    let received = seqpacket::recv_fds::<1>(&b).unwrap();
    assert!(received.data.is_empty());
    assert_eq!(received.fds.len(), 1);
}

#[test]
fn surplus_is_counted_and_oversized_payload_is_an_error() {
    let (a, b) = pair(0);
    let file = tempfile::tempfile().unwrap();
    seqpacket::send_fds(&a, b"x", &[&file, &file, &file]).unwrap();
    let received = seqpacket::recv_fds::<1>(&b).unwrap();
    assert_eq!((received.fds.len(), received.surplus_closed), (1, 2));

    seqpacket::send_fds(&a, b"too long", &[&file]).unwrap();
    let mut small = [0u8; 2];
    let err = seqpacket::recv_fds_into::<1>(&b, &mut small).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}

#[test]
fn closed_peer_is_an_empty_result() {
    let (a, b) = pair(0);
    drop(a);
    let received = seqpacket::recv_fds::<1>(&b).unwrap();
    assert!(received.data.is_empty() && received.fds.is_empty());
}

#[cfg(feature = "tokio")]
#[tokio::test]
async fn async_forms_round_trip_after_waiting() {
    use tokio::io::unix::AsyncFd;

    let (a, b) = pair(libc::SOCK_NONBLOCK);
    let a = AsyncFd::new(a).unwrap();
    let b = AsyncFd::new(b).unwrap();
    let file = tempfile::tempfile().unwrap();

    let mut buf = [0u8; 16];
    let (received, sent) = tokio::join!(
        seqpacket::tokio::recv_fds_into::<1, _>(&b, &mut buf),
        async { seqpacket::tokio::send_fds(&a, b"hi", &[&file]).await },
    );
    assert_eq!(sent.unwrap(), 2);
    let (n, fds) = received.unwrap();
    assert_eq!((n, fds.len()), (2, 1));
    drop(fds);

    seqpacket::tokio::send_fds(&a, b"hi", &[&file])
        .await
        .unwrap();
    let (n, fds) = seqpacket::tokio::recv_fds_exact_into::<1, _>(&b, &mut buf)
        .await
        .unwrap();
    assert_eq!((n, fds.len()), (2, 1));
}
