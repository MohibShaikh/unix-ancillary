use std::io::{Read, Write};
use std::os::unix::io::{AsFd, AsRawFd};
use std::os::unix::net::UnixStream;
use unix_ancillary::{AncillaryData, SocketAncillary, UnixStreamExt};

#[test]
fn send_recv_single_fd() {
    let (tx, rx) = UnixStream::pair().unwrap();

    let file = tempfile::tempfile().unwrap();
    // Write something so we can verify the received fd works
    let mut file = file;
    file.write_all(b"hello from fd").unwrap();
    file.flush().unwrap();

    tx.send_fds(b"msg", &[&file]).unwrap();

    let (n, data, fds) = rx.recv_fds::<1>().unwrap();
    assert_eq!(&data[..n], b"msg");
    assert_eq!(fds.len(), 1);

    // Verify the received fd is valid and points to the same file
    let mut received_file: std::fs::File = fds.into_iter().next().unwrap().into();
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

    let (n, data, fds) = rx.recv_fds::<3>().unwrap();
    assert_eq!(&data[..n], b"multi");
    assert_eq!(fds.len(), 3);
}

#[test]
fn owned_fd_closes_on_drop() {
    let (tx, rx) = UnixStream::pair().unwrap();

    let file = tempfile::tempfile().unwrap();
    tx.send_fds(b"x", &[&file]).unwrap();

    let (_n, _data, fds) = rx.recv_fds::<1>().unwrap();
    let received_raw = fds[0].as_raw_fd();

    // Drop the received fds
    drop(fds);

    // The received fd should now be closed — trying to use it should fail
    unsafe {
        let ret = libc::fcntl(received_raw, libc::F_GETFD);
        assert_eq!(ret, -1, "fd should be closed after OwnedFd drop");
    }
}

#[test]
fn low_level_api() {
    let (tx, rx) = UnixStream::pair().unwrap();

    let file = tempfile::tempfile().unwrap();

    // Send using low-level API
    let mut buf = [0u8; 64];
    let mut ancillary = SocketAncillary::new(&mut buf);
    ancillary.add_fds(&[file.as_fd()]).unwrap();

    let iov = [std::io::IoSlice::new(b"low-level")];
    unix_ancillary::cmsg_sendmsg(tx.as_fd(), &iov, &ancillary).unwrap();

    // Receive using low-level API
    let mut data_buf = [0u8; 64];
    let mut anc_buf = [0u8; 64];
    let mut recv_anc = SocketAncillary::new(&mut anc_buf);
    let mut iov = [std::io::IoSliceMut::new(&mut data_buf)];
    let n = unix_ancillary::cmsg_recvmsg(rx.as_fd(), &mut iov, &mut recv_anc).unwrap();

    assert_eq!(&data_buf[..n], b"low-level");

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
    let mut buf = [0u8; 1]; // Too small
    let mut ancillary = SocketAncillary::new(&mut buf);
    let file = tempfile::tempfile().unwrap();
    assert!(ancillary.add_fds(&[file.as_fd()]).is_err());
}
