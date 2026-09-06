use std::fs::File;
use std::io::{IoSliceMut, ErrorKind};
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use unix_ancillary::{AncillaryData, SocketAncillary, UnixStreamExt};

fn first(anc: &SocketAncillary<'_>) -> OwnedFd {
    anc.messages().find_map(|msg| match msg {
        AncillaryData::ScmRights(mut rights) => rights.next(),
        _ => None,
    }).expect("one fd")
}

#[test]
fn send_buffer_must_not_own_borrowed_fd() {
    let file = File::open("/dev/null").unwrap();
    let mut buf = vec![0u8; SocketAncillary::buffer_size_for_rights(1)];
    let mut anc = SocketAncillary::new(&mut buf);
    anc.add_fds(&[file.as_fd()]).unwrap();
    drop(first(&anc)); // Entire caller is safe Rust.
    let result = file.metadata();
    std::mem::forget(file); // Avoid a second close during this diagnostic.
    assert!(result.is_ok(), "borrowed caller fd was closed: {result:?}");
}

#[test]
fn received_fd_must_not_have_two_owners() {
    let (tx, rx) = UnixStream::pair().unwrap();
    let file = File::open("/dev/null").unwrap();
    tx.send_fds(b"x", &[&file]).unwrap();
    let mut buf = vec![0u8; 128];
    let mut anc = SocketAncillary::new(&mut buf);
    let mut data = [0u8; 1];
    unix_ancillary::cmsg_recvmsg(rx.as_fd(), &mut [IoSliceMut::new(&mut data)], &mut anc).unwrap();
    let a = first(&anc);
    let b = first(&anc);
    let same = a.as_raw_fd() == b.as_raw_fd();
    std::mem::forget(b); // Avoid closing the same fd twice in this diagnostic.
    assert!(!same, "two OwnedFd values own fd {}", a.as_raw_fd());
}

fn fd_count() -> usize { std::fs::read_dir("/proc/self/fd").unwrap().count() }

#[test]
fn dropping_unread_receive_buffer_must_close_fds() {
    let (tx, rx) = UnixStream::pair().unwrap();
    let file = File::open("/dev/null").unwrap();
    let before = fd_count();
    for _ in 0..4 {
        tx.send_fds(b"x", &[&file]).unwrap();
        let mut buf = vec![0u8; 128];
        let mut anc = SocketAncillary::new(&mut buf);
        let mut data = [0u8; 1];
        unix_ancillary::cmsg_recvmsg(rx.as_fd(), &mut [IoSliceMut::new(&mut data)], &mut anc).unwrap();
        // No iteration, e.g. a credentials check failed or caller returned early.
    }
    assert_eq!(fd_count(), before, "unconsumed received fds leaked");
}

#[test]
fn legal_maximum_fd_message_with_passcred_must_succeed() {
    let (tx, rx) = UnixStream::pair().unwrap();
    unix_ancillary::set_passcred(&rx, true).unwrap();
    let file = File::open("/dev/null").unwrap();
    tx.send_fds(b"x", &[&file; 253]).unwrap();
    let result = rx.recv_fds_exact::<253>();
    assert!(result.is_ok(), "legal message rejected: {result:?}");
}

#[test]
fn unaligned_safe_buffer_must_not_abort() {
    if std::env::var_os("ANC_AUDIT_UNALIGNED_CHILD").is_some() {
        let file = File::open("/dev/null").unwrap();
        let mut buf = vec![0u8; 128];
        let offset = (0..8).find(|i| (buf.as_ptr() as usize + i) % 8 != 0).unwrap();
        let mut anc = SocketAncillary::new(&mut buf[offset..]);
        anc.add_fds(&[file.as_fd()]).unwrap();
        return;
    }
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "unaligned_safe_buffer_must_not_abort", "--nocapture"])
        .env("ANC_AUDIT_UNALIGNED_CHILD", "1").status().unwrap();
    assert!(status.success(), "safe unaligned slice aborted child: {status}");
}

#[test]
fn high_level_without_passcred_is_control() {
    let (tx, rx) = UnixStream::pair().unwrap();
    let file = File::open("/dev/null").unwrap();
    tx.send_fds(b"x", &[&file; 253]).unwrap();
    assert_eq!(rx.recv_fds_exact::<253>().unwrap().fds.len(), 253);
    rx.set_nonblocking(true).unwrap();
    assert_eq!(rx.recv_fds::<1>().unwrap_err().kind(), ErrorKind::WouldBlock);
}

#[test]
fn send_buffer_must_retain_descriptor_lifetime() {
    use std::io::Read;
    let (tx, rx) = UnixStream::pair().unwrap();
    let original = File::open("/dev/null").unwrap();
    let original_raw = original.as_raw_fd();
    let mut buf = vec![0u8; SocketAncillary::buffer_size_for_rights(1)];
    let mut anc = SocketAncillary::new(&mut buf);
    anc.add_fds(&[original.as_fd()]).unwrap();
    drop(original); // Compiles: buffer does not retain the BorrowedFd lifetime.
    let replacement = File::open("/dev/zero").unwrap();
    assert_eq!(replacement.as_raw_fd(), original_raw, "diagnostic needs fd reuse");
    unix_ancillary::cmsg_sendmsg(tx.as_fd(), &[std::io::IoSlice::new(b"x")], &anc).unwrap();
    let mut received: File = rx.recv_fds_exact::<1>().unwrap().fds.pop().unwrap().into();
    let n = received.read(&mut [0u8; 1]).unwrap();
    assert_eq!(n, 0, "intended /dev/null, but sent replacement /dev/zero");
}
