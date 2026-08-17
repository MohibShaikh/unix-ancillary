//! Async (tokio) fd-passing tests. Compiled only with `--features tokio`.
#![cfg(feature = "tokio")]

use std::io::{Read, Seek, Write};

use tokio::io::AsyncReadExt;
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

#[tokio::test]
async fn async_stream_rejects_fds_without_payload_byte() {
    let (tx, _rx) = UnixStream::pair().unwrap();
    let file = tempfile::tempfile().unwrap();

    let err = tx.send_fds(b"", &[&file]).await.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
}

#[tokio::test]
async fn async_send_fds_all_transfers_payload_and_descriptor() {
    let (tx, mut rx) = UnixStream::pair().unwrap();
    let file = tempfile::tempfile().unwrap();
    let expected = vec![b'x'; 128 * 1024];
    let payload = expected.clone();

    let sender = tokio::spawn(async move {
        tx.send_fds_all(&payload, &[&file]).await.unwrap();
    });

    let first = rx.recv_fds::<1>().await.unwrap();
    assert_eq!(first.fds.len(), 1);
    let mut received = first.data;
    while received.len() < expected.len() {
        let mut buf = [0u8; 8192];
        let n = rx.read(&mut buf).await.unwrap();
        assert_ne!(n, 0);
        received.extend_from_slice(&buf[..n]);
    }

    sender.await.unwrap();
    assert_eq!(received, expected);
}

/// See the blocking `shrink_sndbuf`: forces the first `sendmsg` to be short so
/// the completion loop in `send_fds_all` actually runs.
fn shrink_sndbuf(s: &UnixStream) {
    use std::os::unix::io::AsRawFd;
    let v: libc::c_int = 2048;
    // SAFETY: live socket, correct option length for SO_SNDBUF.
    let ret = unsafe {
        libc::setsockopt(
            s.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_SNDBUF,
            (&v as *const libc::c_int).cast(),
            std::mem::size_of_val(&v) as libc::socklen_t,
        )
    };
    assert_eq!(ret, 0, "setsockopt(SO_SNDBUF) failed");
}

#[tokio::test]
async fn async_partial_first_send_delivers_exactly_one_descriptor() {
    const SIZE: usize = 128 * 1024;

    let (tx, rx) = UnixStream::pair().unwrap();
    shrink_sndbuf(&tx);
    let file = tempfile::tempfile().unwrap();
    let payload = vec![b'x'; SIZE];

    let sender = tokio::spawn(async move {
        tx.send_fds_all(&payload, &[&file]).await.unwrap();
    });

    // recvmsg on every read: a retransmitted descriptor delivered to a plain
    // read() would be closed by the kernel and go unnoticed.
    let first = rx.recv_fds::<8>().await.unwrap();
    let mut seen_fds = first.fds.len();
    let mut total = first.data.len();
    while total < SIZE {
        let mut buf = [0u8; 8192];
        let (n, extra) = rx.recv_fds_into::<8>(&mut buf).await.unwrap();
        assert_ne!(n, 0);
        seen_fds += extra.len();
        total += n;
    }
    sender.await.unwrap();

    assert_eq!(total, SIZE);
    assert_eq!(
        seen_fds, 1,
        "descriptor retransmitted across the partial send"
    );
}

#[tokio::test]
async fn async_datagram_payload_truncation_is_an_error() {
    let (tx, rx) = UnixDatagram::pair().unwrap();
    tx.send(b"payload-larger-than-buffer").await.unwrap();
    let mut small = [0u8; 4];

    let err = rx.recv_fds_into::<0>(&mut small).await.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}

#[tokio::test]
async fn async_recv_exact_accepts_exact_count() {
    let (tx, rx) = UnixStream::pair().unwrap();
    let first = tempfile::tempfile().unwrap();
    let second = tempfile::tempfile().unwrap();
    tx.send_fds(b"two", &[&first, &second]).await.unwrap();

    let received = rx.recv_fds_exact::<2>().await.unwrap();
    assert_eq!(received.data, b"two");
    assert_eq!(received.fds.len(), 2);
}

#[tokio::test]
async fn async_recv_exact_rejects_too_few() {
    let (tx, rx) = UnixStream::pair().unwrap();
    let file = tempfile::tempfile().unwrap();
    tx.send_fds(b"one", &[&file]).await.unwrap();

    let err = rx.recv_fds_exact::<2>().await.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}

#[tokio::test]
async fn async_recv_exact_rejects_too_many() {
    let (tx, rx) = UnixStream::pair().unwrap();
    let first = tempfile::tempfile().unwrap();
    let second = tempfile::tempfile().unwrap();
    let third = tempfile::tempfile().unwrap();
    tx.send_fds(b"three", &[&first, &second, &third])
        .await
        .unwrap();

    let err = rx.recv_fds_exact::<2>().await.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}

#[tokio::test]
async fn async_recv_exact_datagram_accepts_exact_count() {
    let (tx, rx) = UnixDatagram::pair().unwrap();
    let first = tempfile::tempfile().unwrap();
    let second = tempfile::tempfile().unwrap();
    tx.send_fds(b"dg", &[&first, &second]).await.unwrap();

    let received = rx.recv_fds_exact::<2>().await.unwrap();
    assert_eq!(received.data, b"dg");
    assert_eq!(received.fds.len(), 2);
}
