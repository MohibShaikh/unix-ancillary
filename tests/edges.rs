//! Edge cases of the documented contracts, each from a defect found in review.

use std::io::{ErrorKind, IoSlice, IoSliceMut, Read};
use std::os::fd::AsFd;
use std::os::unix::net::{UnixDatagram, UnixStream};
use unix_ancillary::{
    cmsg_recvmsg, cmsg_recvmsg_with_flags, cmsg_sendmsg, SocketAncillary, UnixDatagramExt,
    UnixStreamExt,
};

#[test]
fn huge_descriptor_count_is_not_a_panic() {
    let (tx, rx) = UnixStream::pair().unwrap();
    let file = tempfile::tempfile().unwrap();
    tx.send_fds(b"x", &[&file]).unwrap();
    let mut data = [0u8; 1];
    let (n, fds) = rx.recv_fds_into::<{ usize::MAX }>(&mut data).unwrap();
    assert_eq!((n, fds.len()), (1, 1));
}

#[test]
fn rejected_flags_still_close_unread_descriptors() {
    let (tx, rx) = UnixStream::pair().unwrap();
    let (held, mut peer) = UnixStream::pair().unwrap();
    tx.send_fds(b"x", &[&held]).unwrap();
    drop(held);

    let mut storage = vec![0u8; SocketAncillary::buffer_size_for_rights(1)];
    let mut ancillary = SocketAncillary::new(&mut storage);
    let mut data = [0u8; 1];
    let mut iov = [IoSliceMut::new(&mut data)];
    cmsg_recvmsg(rx.as_fd(), &mut iov, &mut ancillary).unwrap();

    let err =
        cmsg_recvmsg_with_flags(rx.as_fd(), &mut iov, &mut ancillary, libc::MSG_PEEK).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidInput);

    // The only other copy was in `ancillary`, so the peer now sees end of stream.
    peer.set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    let mut byte = [0u8; 1];
    assert_eq!(peer.read(&mut byte).unwrap(), 0);
}

#[test]
fn receive_with_empty_control_storage_returns_the_bytes() {
    let (tx, rx) = UnixStream::pair().unwrap();
    let storage: &mut [u8] = &mut [];
    let mut send = [0u8; 0];
    let out = SocketAncillary::new(&mut send);
    cmsg_sendmsg(tx.as_fd(), &[IoSlice::new(b"x")], &out).unwrap();

    let mut ancillary = SocketAncillary::new(storage);
    let mut data = [0u8; 1];
    let got = cmsg_recvmsg(
        rx.as_fd(),
        &mut [IoSliceMut::new(&mut data)],
        &mut ancillary,
    )
    .unwrap();
    assert_eq!(got.bytes_read, 1);
}

#[cfg(any(target_os = "linux", target_os = "android"))]
#[test]
fn too_many_descriptors_says_so() {
    let (tx, _rx) = UnixStream::pair().unwrap();
    let file = tempfile::tempfile().unwrap();
    let err = tx.send_fds(b"x", &vec![&file; 254]).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidInput);
    assert!(err.to_string().contains("descriptors"), "{err}");
}

#[test]
fn small_datagram_does_not_keep_the_whole_receive_buffer() {
    let (tx, rx) = UnixDatagram::pair().unwrap();
    let file = tempfile::tempfile().unwrap();
    tx.send_fds(b"x", &[&file]).unwrap();
    let received = rx.recv_fds::<1>().unwrap();
    assert_eq!(&received.data[..], b"x");
    assert!(
        received.data.capacity() < 4096,
        "{}",
        received.data.capacity()
    );
}
