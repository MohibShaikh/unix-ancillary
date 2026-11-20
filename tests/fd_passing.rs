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
    // Prove closure via EOF on the peer rather than probing a raw fd number
    // that a parallel test in this binary could immediately reuse.
    let (passed, mut peer) = UnixStream::pair().unwrap();
    tx.send_fds(b"x", &[&passed]).unwrap();
    drop(passed);

    let recv = rx.recv_fds::<1>().unwrap();
    assert_eq!(recv.fds.len(), 1);
    drop(recv);

    let mut byte = [0u8; 1];
    assert_eq!(
        peer.read(&mut byte).unwrap(),
        0,
        "peer should observe EOF once the received OwnedFd drops"
    );
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

/// Shrink the send buffer. A blocking `sendmsg` still waits and takes the whole
/// payload in one call on Linux; `tests/partial_send.rs` forces a short send.
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

#[test]
fn huge_descriptor_cap_does_not_panic() {
    let (tx, rx) = UnixStream::pair().unwrap();
    let file = tempfile::tempfile().unwrap();
    tx.send_fds(b"x", &[&file]).unwrap();

    let mut buf = [0u8; 8];
    let (n, fds) = rx.recv_fds_into::<{ usize::MAX }>(&mut buf).unwrap();
    assert_eq!((n, fds.len()), (1, 1));
}

#[test]
fn ancillary_types_implement_debug_exact_size_and_io_error() {
    let (tx, rx) = UnixStream::pair().unwrap();
    let file = tempfile::tempfile().unwrap();
    tx.send_fds(b"x", &[&file, &file]).unwrap();

    let mut tiny = [0u8; 1];
    let mut small_anc = SocketAncillary::new(&mut tiny);
    let anc_err = small_anc.add_fds(&[file.as_fd()]).unwrap_err();
    assert_eq!(anc_err, unix_ancillary::AncillaryError);
    let io_err: std::io::Error = anc_err.into();
    assert_eq!(io_err.kind(), std::io::ErrorKind::InvalidInput);
    assert!(!format!("{small_anc:?}").is_empty());

    let mut data = [0u8; 8];
    let mut storage = [0u8; 128];
    let mut recv_anc = SocketAncillary::new(&mut storage);
    unix_ancillary::cmsg_recvmsg(
        rx.as_fd(),
        &mut [std::io::IoSliceMut::new(&mut data)],
        &mut recv_anc,
    )
    .unwrap();
    assert!(!format!("{recv_anc:?}").is_empty());

    let mut msgs = recv_anc.messages();
    assert_eq!(msgs.len(), 1);
    assert!(!format!("{msgs:?}").is_empty());
    let msg = msgs.next().unwrap();
    assert!(!format!("{msg:?}").is_empty());
    if let AncillaryData::ScmRights(rights) = msg {
        assert_eq!(rights.len(), 2);
        assert!(!format!("{rights:?}").is_empty());
        assert_eq!(rights.count(), 2);
    } else {
        panic!("expected ScmRights");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn seqpacket_wrapped_as_stream_rejects_truncated_payload() {
    use std::os::fd::FromRawFd;

    let mut fds = [0; 2];
    // SAFETY: socketpair writes two descriptors into `fds`.
    let rc = unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
            0,
            fds.as_mut_ptr(),
        )
    };
    assert_eq!(rc, 0);
    // SAFETY: fresh descriptors owned exclusively by this test.
    let (tx, rx) = unsafe {
        (
            UnixStream::from_raw_fd(fds[0]),
            UnixStream::from_raw_fd(fds[1]),
        )
    };
    let file = tempfile::tempfile().unwrap();
    tx.send_fds(b"payload-too-long", &[&file]).unwrap();

    let mut small = [0u8; 4];
    let err = rx.recv_fds_into::<1>(&mut small).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}
