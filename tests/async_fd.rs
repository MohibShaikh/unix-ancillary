//! Async (tokio) fd-passing tests. Compiled only with `--features tokio`.
#![cfg(feature = "tokio")]

use std::io::{Read, Seek, Write};

use tokio::net::{UnixDatagram, UnixStream};
use unix_ancillary::{AsyncUnixDatagramExt, AsyncUnixStreamExt};

#[tokio::test]
async fn async_stream_send_recv_fd() {
    let (tx, rx) = UnixStream::pair().unwrap();

    let mut file = tempfile::tempfile().unwrap();
    file.write_all(b"async payload").unwrap();
    file.flush().unwrap();

    tx.send_fds(b"async", &[&file]).await.unwrap();

    let recv = rx.recv_fds::<1>().await.unwrap();
    assert_eq!(&recv.data[..], b"async");
    assert_eq!(recv.fds.len(), 1);

    // The received fd points at the same file — read it back through it.
    let mut received: std::fs::File = recv.fds.into_iter().next().unwrap().into();
    received.seek(std::io::SeekFrom::Start(0)).unwrap();
    let mut contents = String::new();
    received.read_to_string(&mut contents).unwrap();
    assert_eq!(contents, "async payload");
}

#[tokio::test]
async fn async_stream_surplus_fds_capped() {
    let (tx, rx) = UnixStream::pair().unwrap();
    let f1 = tempfile::tempfile().unwrap();
    let f2 = tempfile::tempfile().unwrap();
    let f3 = tempfile::tempfile().unwrap();

    tx.send_fds(b"three", &[&f1, &f2, &f3]).await.unwrap();

    // Ask for one; surplus two are wrapped in OwnedFd and dropped/closed.
    let recv = rx.recv_fds::<1>().await.unwrap();
    assert_eq!(recv.fds.len(), 1);
    assert_eq!(&recv.data[..], b"three");
}

#[tokio::test]
async fn async_datagram_send_recv_fd() {
    let (a, b) = UnixDatagram::pair().unwrap();

    let file = tempfile::tempfile().unwrap();
    a.send_fds(b"dgram", &[&file]).await.unwrap();

    let recv = b.recv_fds::<1>().await.unwrap();
    assert_eq!(&recv.data[..], b"dgram");
    assert_eq!(recv.fds.len(), 1);
}
