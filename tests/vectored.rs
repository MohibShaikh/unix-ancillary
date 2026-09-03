use std::io::{IoSlice, IoSliceMut};
use std::os::unix::net::{UnixDatagram, UnixStream};
use unix_ancillary::{UnixDatagramExt, UnixStreamExt};

// A framed protocol is the motivating case: header and body live in separate
// buffers and must reach the peer in one sendmsg, with the fd attached once.
#[test]
fn stream_vectored_roundtrip_keeps_order_and_fd() {
    let (tx, rx) = UnixStream::pair().unwrap();
    let (payload, _) = UnixStream::pair().unwrap();

    let iov = [IoSlice::new(b"HDR:"), IoSlice::new(b"body")];
    let sent = tx.send_fds_vectored(&iov, &[&payload]).unwrap();
    assert_eq!(sent, 8);

    let mut head = [0u8; 4];
    let mut body = [0u8; 4];
    let mut riov = [IoSliceMut::new(&mut head), IoSliceMut::new(&mut body)];
    let (n, fds) = rx.recv_fds_vectored::<1>(&mut riov).unwrap();

    assert_eq!(n, 8);
    assert_eq!(&head, b"HDR:");
    assert_eq!(&body, b"body");
    assert_eq!(fds.len(), 1);
}

#[test]
fn stream_vectored_rejects_empty_payload_with_fds() {
    let (tx, _rx) = UnixStream::pair().unwrap();
    let (payload, _) = UnixStream::pair().unwrap();

    let iov = [IoSlice::new(b""), IoSlice::new(b"")];
    let err = tx.send_fds_vectored(&iov, &[&payload]).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn stream_vectored_allows_empty_payload_without_fds() {
    let (tx, _rx) = UnixStream::pair().unwrap();
    let iov = [IoSlice::new(b"")];
    let no_fds: [&UnixStream; 0] = [];
    assert_eq!(tx.send_fds_vectored(&iov, &no_fds).unwrap(), 0);
}

#[test]
fn datagram_vectored_roundtrip() {
    let (tx, rx) = UnixDatagram::pair().unwrap();
    let (payload, _) = UnixStream::pair().unwrap();

    let iov = [IoSlice::new(b"ab"), IoSlice::new(b"cd")];
    tx.send_fds_vectored(&iov, &[&payload]).unwrap();

    let mut a = [0u8; 2];
    let mut b = [0u8; 2];
    let mut riov = [IoSliceMut::new(&mut a), IoSliceMut::new(&mut b)];
    let (n, fds) = rx.recv_fds_vectored::<1>(&mut riov).unwrap();

    assert_eq!(n, 4);
    assert_eq!(&a, b"ab");
    assert_eq!(&b, b"cd");
    assert_eq!(fds.len(), 1);
}
