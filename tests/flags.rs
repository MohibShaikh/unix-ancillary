//! Per-call socket flags on the low-level API.

use std::io::{ErrorKind, IoSlice, IoSliceMut};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use unix_ancillary::{cmsg_recvmsg_with_flags, cmsg_sendmsg_with_flags, SocketAncillary};

#[test]
fn dontwait_returns_wouldblock_on_a_blocking_socket() {
    let (_tx, rx) = UnixStream::pair().unwrap();
    let mut storage = [0u8; 64];
    let mut ancillary = SocketAncillary::new(&mut storage);
    let mut data = [0u8; 1];
    let err = cmsg_recvmsg_with_flags(
        rx.as_fd(),
        &mut [IoSliceMut::new(&mut data)],
        &mut ancillary,
        libc::MSG_DONTWAIT,
    )
    .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::WouldBlock);
}

#[test]
fn descriptors_round_trip_with_flags_set() {
    let (tx, rx) = UnixStream::pair().unwrap();
    let file = tempfile::tempfile().unwrap();
    let mut send = vec![0u8; SocketAncillary::buffer_size_for_rights(1)];
    let mut out = SocketAncillary::new(&mut send);
    out.add_fds(&[file.as_fd()]).unwrap();
    let sent = cmsg_sendmsg_with_flags(tx.as_fd(), &[IoSlice::new(b"x")], &out, libc::MSG_DONTWAIT)
        .unwrap();
    assert_eq!(sent, 1);

    let mut storage = vec![0u8; SocketAncillary::buffer_size_for_rights(1)];
    let mut ancillary = SocketAncillary::new(&mut storage);
    let mut data = [0u8; 1];
    let got = cmsg_recvmsg_with_flags(
        rx.as_fd(),
        &mut [IoSliceMut::new(&mut data)],
        &mut ancillary,
        libc::MSG_WAITALL,
    )
    .unwrap();
    assert_eq!(got.bytes_read, 1);
    assert_eq!(ancillary.messages().count(), 1);
}

#[test]
fn flags_that_could_break_ownership_are_refused() {
    let (tx, rx) = UnixStream::pair().unwrap();
    let mut storage = [0u8; 64];
    let mut ancillary = SocketAncillary::new(&mut storage);
    let mut data = [0u8; 1];
    for flag in [libc::MSG_PEEK, libc::MSG_TRUNC] {
        let err = cmsg_recvmsg_with_flags(
            rx.as_fd(),
            &mut [IoSliceMut::new(&mut data)],
            &mut ancillary,
            flag,
        )
        .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
    }
    drop(ancillary);
    let mut storage = [0u8; 64];
    let out = SocketAncillary::new(&mut storage);
    let err = cmsg_sendmsg_with_flags(tx.as_fd(), &[IoSlice::new(b"x")], &out, libc::MSG_OOB)
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidInput);
}
