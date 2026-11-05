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
    // Set this now: XNU refuses setsockopt once the other end has closed.
    peer.set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
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

#[test]
fn would_block_partway_reports_how_much_went_and_can_finish() {
    use std::os::fd::BorrowedFd;
    use std::sync::mpsc;
    use unix_ancillary::PartialSend;

    let (tx, rx) = UnixStream::pair().unwrap();
    tx.set_nonblocking(true).unwrap();
    let file = tempfile::tempfile().unwrap();
    let payload = vec![7u8; 8 << 20];

    let err = tx.send_fds_all(&payload, &[&file]).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::WouldBlock);
    let sent = err
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<PartialSend>())
        .expect("payload says how much went")
        .sent;
    assert!(sent > 0 && sent < payload.len(), "{sent}");

    let (done_tx, done_rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut data = vec![0u8; 1 << 16];
        let (mut total, mut fds) = (0, 0);
        while total < 8 << 20 {
            let (n, got) = rx.recv_fds_into::<2>(&mut data).unwrap();
            total += n;
            fds += got.len();
        }
        done_tx.send((total, fds)).unwrap();
    });
    tx.set_nonblocking(false).unwrap();
    tx.send_fds_all(&payload[sent..], &[] as &[BorrowedFd])
        .unwrap();
    reader.join().unwrap();
    assert_eq!(done_rx.recv().unwrap(), (8 << 20, 1));
}

#[test]
fn slots_vectored_fills_buffers_in_order_and_refuses_other_flags() {
    use unix_ancillary::recv_fds_slots_vectored;

    let (tx, rx) = UnixStream::pair().unwrap();
    let file = tempfile::tempfile().unwrap();
    tx.send_fds(b"hello", &[&file, &file]).unwrap();

    let (mut head, mut tail) = ([0u8; 2], [0u8; 8]);
    let mut slots = [None];
    let got = recv_fds_slots_vectored(
        &rx,
        &mut [IoSliceMut::new(&mut head), IoSliceMut::new(&mut tail)],
        &mut slots,
        libc::MSG_DONTWAIT,
    )
    .unwrap();
    assert_eq!((got.bytes_read, got.fds, got.surplus_closed), (5, 1, 1));
    assert_eq!((&head[..], &tail[..3]), (&b"he"[..], &b"llo"[..]));
    assert!(slots[0].is_some());

    let err = recv_fds_slots_vectored(
        &rx,
        &mut [IoSliceMut::new(&mut head)],
        &mut slots,
        libc::MSG_PEEK,
    )
    .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidInput);
    assert!(slots[0].is_none());
}

#[test]
fn would_block_on_empty_slots_vectored_receive() {
    use unix_ancillary::recv_fds_slots_vectored;
    let (_tx, rx) = UnixStream::pair().unwrap();
    let mut data = [0u8; 1];
    let err = recv_fds_slots_vectored(
        &rx,
        &mut [IoSliceMut::new(&mut data)],
        &mut [None],
        libc::MSG_DONTWAIT,
    )
    .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::WouldBlock);
}

#[test]
fn ancillary_errors_convert_for_question_mark() {
    fn add<'a>(
        ancillary: &mut SocketAncillary<'a>,
        file: &'a std::fs::File,
    ) -> std::io::Result<()> {
        ancillary.add_fds(&[file.as_fd()])?;
        Ok(())
    }
    let file = tempfile::tempfile().unwrap();
    let mut storage = [0u8; 0];
    let mut ancillary = SocketAncillary::new(&mut storage);
    let err = add(&mut ancillary, &file).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidInput);
    assert!(!format!("{ancillary:?}").is_empty());
}
