//! `MSG_PEEK` on kernels that install a fresh duplicate per peek.
#![cfg(any(target_os = "linux", target_os = "android"))]

use std::io::IoSliceMut;
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::net::UnixStream;
use unix_ancillary::{
    cmsg_recvmsg, cmsg_recvmsg_peek, AncillaryData, SocketAncillary, UnixStreamExt,
};

fn drain(ancillary: &mut SocketAncillary<'_>) -> Vec<std::os::fd::OwnedFd> {
    let mut fds = Vec::new();
    for message in ancillary.messages() {
        if let AncillaryData::ScmRights(rights) = message {
            fds.extend(rights);
        }
    }
    fds
}

#[test]
fn peek_leaves_the_message_and_returns_owned_duplicates() {
    let (tx, rx) = UnixStream::pair().unwrap();
    let file = tempfile::tempfile().unwrap();
    tx.send_fds(b"hello", &[&file]).unwrap();

    let mut storage = vec![0u8; SocketAncillary::buffer_size_for_rights(1)];
    let mut ancillary = SocketAncillary::new(&mut storage);
    let mut data = [0u8; 5];

    let peeked = cmsg_recvmsg_peek(
        rx.as_fd(),
        &mut [IoSliceMut::new(&mut data)],
        &mut ancillary,
    )
    .unwrap();
    assert_eq!((peeked.bytes_read, &data), (5, b"hello"));
    let first = drain(&mut ancillary);
    assert_eq!(first.len(), 1);
    // SAFETY: F_GETFD on a live descriptor.
    let flags = unsafe { libc::fcntl(first[0].as_raw_fd(), libc::F_GETFD) };
    assert_eq!(flags & libc::FD_CLOEXEC, libc::FD_CLOEXEC);

    data = [0; 5];
    let real = cmsg_recvmsg(
        rx.as_fd(),
        &mut [IoSliceMut::new(&mut data)],
        &mut ancillary,
    )
    .unwrap();
    assert_eq!((real.bytes_read, &data), (5, b"hello"));
    let second = drain(&mut ancillary);
    assert_eq!(second.len(), 1);
    assert_ne!(first[0].as_raw_fd(), second[0].as_raw_fd());
}
