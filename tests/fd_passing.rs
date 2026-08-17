use std::io::{Read, Write};
use std::os::unix::io::{AsFd, AsRawFd};
use std::os::unix::net::UnixStream;
use unix_ancillary::{AncillaryData, SocketAncillary, UnixStreamExt};

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
        match msg {
            AncillaryData::ScmRights(rights) => {
                fd_count += rights.count();
            }
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
