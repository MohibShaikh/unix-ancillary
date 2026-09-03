use std::io::{Read, Write};
use std::os::unix::io::{AsFd, AsRawFd};
use std::os::unix::net::{UnixDatagram, UnixStream};
use unix_ancillary::{AncillaryData, SocketAncillary, UnixDatagramExt, UnixStreamExt};

#[test]
fn send_recv_single_fd() {
    let (tx, rx) = UnixStream::pair().unwrap();

    let mut file = tempfile::tempfile().unwrap();
    file.write_all(b"hello from fd").unwrap();
    file.flush().unwrap();

    tx.send_fds(b"msg", &[&file]).unwrap();

    let recv = rx.recv_fds::<1>().unwrap();
    assert_eq!(&recv.data[..], b"msg");
    assert_eq!(recv.fds.len(), 1);

    let mut received_file: std::fs::File = recv.fds.into_iter().next().unwrap().into();
    use std::io::Seek;
    received_file.seek(std::io::SeekFrom::Start(0)).unwrap();
    let mut contents = String::new();
    received_file.read_to_string(&mut contents).unwrap();
    assert_eq!(contents, "hello from fd");
}

#[test]
fn send_recv_multiple_fds() {
    let (tx, rx) = UnixStream::pair().unwrap();

    let f1 = tempfile::tempfile().unwrap();
    let f2 = tempfile::tempfile().unwrap();
    let f3 = tempfile::tempfile().unwrap();

    tx.send_fds(b"multi", &[&f1, &f2, &f3]).unwrap();

    let recv = rx.recv_fds::<3>().unwrap();
    assert_eq!(&recv.data[..], b"multi");
    assert_eq!(recv.fds.len(), 3);
}

#[test]
fn owned_fd_closes_on_drop() {
    let (tx, rx) = UnixStream::pair().unwrap();

    let file = tempfile::tempfile().unwrap();
    tx.send_fds(b"x", &[&file]).unwrap();

    let recv = rx.recv_fds::<1>().unwrap();
    let received_raw = recv.fds[0].as_raw_fd();

    drop(recv);

    // SAFETY: probing whether the fd is still open via fcntl.
    unsafe {
        let ret = libc::fcntl(received_raw, libc::F_GETFD);
        assert_eq!(ret, -1, "fd should be closed after OwnedFd drop");
    }
}

#[test]
fn recv_fds_into_with_user_buffer() {
    let (tx, rx) = UnixStream::pair().unwrap();
    let file = tempfile::tempfile().unwrap();
    tx.send_fds(b"into", &[&file]).unwrap();

    let mut buf = [0u8; 16];
    let (n, fds) = rx.recv_fds_into::<1>(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"into");
    assert_eq!(fds.len(), 1);
}

#[test]
fn low_level_api() {
    let (tx, rx) = UnixStream::pair().unwrap();

    let file = tempfile::tempfile().unwrap();

    let mut buf = [0u8; 64];
    let mut ancillary = SocketAncillary::new(&mut buf);
    ancillary.add_fds(&[file.as_fd()]).unwrap();

    let iov = [std::io::IoSlice::new(b"low-level")];
    unix_ancillary::cmsg_sendmsg(tx.as_fd(), &iov, &ancillary).unwrap();

    let mut data_buf = [0u8; 64];
    let mut anc_buf = [0u8; 64];
    let mut recv_anc = SocketAncillary::new(&mut anc_buf);
    let mut iov = [std::io::IoSliceMut::new(&mut data_buf)];
    let result = unix_ancillary::cmsg_recvmsg(rx.as_fd(), &mut iov, &mut recv_anc).unwrap();

    assert_eq!(&data_buf[..result.bytes_read], b"low-level");
    assert!(!result.truncated);

    let mut fd_count = 0;
    for msg in recv_anc.messages() {
        // AncillaryData is #[non_exhaustive]. This test is a separate crate, so
        // it sees the same requirement a downstream user does: no exhaustive
        // match on the enum.
        if let AncillaryData::ScmRights(rights) = msg {
            fd_count += rights.count();
        }
    }
    assert_eq!(fd_count, 1);
}

#[test]
fn buffer_size_calculation() {
    let size1 = SocketAncillary::buffer_size_for_rights(1);
    let size4 = SocketAncillary::buffer_size_for_rights(4);
    assert!(size1 > 0);
    assert!(size4 > size1);
}

#[test]
fn ancillary_buffer_too_small() {
    let mut buf = [0u8; 1];
    let mut ancillary = SocketAncillary::new(&mut buf);
    let file = tempfile::tempfile().unwrap();
    assert!(ancillary.add_fds(&[file.as_fd()]).is_err());
}

#[test]
fn stream_rejects_fds_without_payload_byte() {
    let (tx, _rx) = UnixStream::pair().unwrap();
    let file = tempfile::tempfile().unwrap();

    let err = tx.send_fds(b"", &[&file]).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn stream_allows_empty_payload_without_fds() {
    let (tx, _rx) = UnixStream::pair().unwrap();
    let empty: &[std::fs::File] = &[];
    assert_eq!(tx.send_fds(b"", empty).unwrap(), 0);
}

#[test]
fn send_to_closed_peer_returns_error_without_sigpipe() {
    if std::env::var_os("UNIX_ANCILLARY_SIGPIPE_CHILD").is_some() {
        // Undo the runtime's SIG_IGN so an unprotected send really dies.
        // SAFETY: SIG_DFL is a valid disposition for SIGPIPE.
        unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };

        let (tx, rx) = UnixStream::pair().unwrap();
        drop(rx);
        let file = tempfile::tempfile().unwrap();
        let result = tx.send_fds(b"x", &[&file]);
        // Recorded so CI shows what each kernel actually returns. Linux gives
        // BrokenPipe; the assertion stays on "an error, and we are still
        // alive" because the errno is not the property under test — the
        // parent's exit-status check is.
        eprintln!("send to closed peer returned: {result:?}");
        assert!(
            result.is_err(),
            "send to a closed peer should fail, got {result:?}"
        );
        return;
    }

    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("send_to_closed_peer_returns_error_without_sigpipe")
        .arg("--nocapture")
        .env("UNIX_ANCILLARY_SIGPIPE_CHILD", "1")
        .status()
        .unwrap();

    assert!(status.success(), "child terminated with {status}");
}

#[test]
fn send_fds_all_transfers_payload_and_descriptor() {
    let (tx, rx) = UnixStream::pair().unwrap();
    let file = tempfile::tempfile().unwrap();
    let payload = vec![b'x'; 128 * 1024];

    let sender = std::thread::spawn(move || tx.send_fds_all(&payload, &[&file]).unwrap());

    let first = rx.recv_fds::<1>().unwrap();
    assert_eq!(first.fds.len(), 1);
    let mut received = first.data;
    while received.len() < 128 * 1024 {
        let mut buf = [0u8; 8192];
        let n = std::io::Read::read(&mut &rx, &mut buf).unwrap();
        assert_ne!(n, 0);
        received.extend_from_slice(&buf[..n]);
    }
    sender.join().unwrap();
    assert_eq!(received, vec![b'x'; 128 * 1024]);
}

/// Shrink the send buffer so the first `sendmsg` cannot accept the whole
/// payload. Without this the kernel takes all 128 KiB in one call and the
/// completion loop in `send_fds_all` is never exercised.
fn shrink_sndbuf(s: &UnixStream) {
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

#[test]
fn partial_first_send_delivers_exactly_one_descriptor() {
    const SIZE: usize = 128 * 1024;

    let (tx, rx) = UnixStream::pair().unwrap();
    shrink_sndbuf(&tx);
    let file = tempfile::tempfile().unwrap();
    let payload = vec![b'x'; SIZE];

    let sender = std::thread::spawn(move || tx.send_fds_all(&payload, &[&file]).unwrap());

    // Drain with recvmsg, never a plain read: the kernel silently closes
    // descriptors delivered to a read() with no control buffer, so a
    // retransmission would be invisible.
    let first = rx.recv_fds::<8>().unwrap();
    let mut seen_fds = first.fds.len();
    let mut total = first.data.len();
    while total < SIZE {
        let mut buf = [0u8; 8192];
        let (n, extra) = rx.recv_fds_into::<8>(&mut buf).unwrap();
        assert_ne!(n, 0);
        seen_fds += extra.len();
        total += n;
    }
    sender.join().unwrap();

    assert_eq!(total, SIZE);
    assert_eq!(
        seen_fds, 1,
        "descriptor retransmitted across the partial send"
    );
}

#[test]
fn datagram_payload_truncation_is_an_error() {
    let (tx, rx) = UnixDatagram::pair().unwrap();
    tx.send(b"payload-larger-than-buffer").unwrap();
    let mut small = [0u8; 4];

    let err = rx.recv_fds_into::<0>(&mut small).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}

#[test]
fn recv_exact_accepts_exact_count() {
    let (tx, rx) = UnixStream::pair().unwrap();
    let first = tempfile::tempfile().unwrap();
    let second = tempfile::tempfile().unwrap();
    tx.send_fds(b"two", &[&first, &second]).unwrap();

    let received = rx.recv_fds_exact::<2>().unwrap();
    assert_eq!(received.data, b"two");
    assert_eq!(received.fds.len(), 2);
}

#[test]
fn recv_exact_rejects_too_few() {
    let (tx, rx) = UnixStream::pair().unwrap();
    let file = tempfile::tempfile().unwrap();
    tx.send_fds(b"one", &[&file]).unwrap();

    let err = rx.recv_fds_exact::<2>().unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}

#[test]
fn recv_exact_rejects_too_many() {
    let (tx, rx) = UnixStream::pair().unwrap();
    let first = tempfile::tempfile().unwrap();
    let second = tempfile::tempfile().unwrap();
    let third = tempfile::tempfile().unwrap();
    tx.send_fds(b"three", &[&first, &second, &third]).unwrap();

    let err = rx.recv_fds_exact::<2>().unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}

#[test]
fn recv_exact_datagram_accepts_exact_count() {
    let (tx, rx) = UnixDatagram::pair().unwrap();
    let first = tempfile::tempfile().unwrap();
    let second = tempfile::tempfile().unwrap();
    tx.send_fds(b"dg", &[&first, &second]).unwrap();

    let received = rx.recv_fds_exact::<2>().unwrap();
    assert_eq!(received.data, b"dg");
    assert_eq!(received.fds.len(), 2);
}
