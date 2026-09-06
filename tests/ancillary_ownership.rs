//! Descriptor-table checks serialize within this otherwise isolated test binary.
use std::fs::File;
use std::io::{IoSlice, IoSliceMut};
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::Mutex;
use unix_ancillary::{cmsg_recvmsg, cmsg_sendmsg, AncillaryData, SocketAncillary, UnixStreamExt};

static FD_LOCK: Mutex<()> = Mutex::new(());

fn fd_count() -> usize {
    (0..1024)
        .filter(|&fd| unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0)
        .count()
}

fn receive(rx: &UnixStream, anc: &mut SocketAncillary<'_>) {
    cmsg_recvmsg(rx.as_fd(), &mut [IoSliceMut::new(&mut [0; 1])], anc).unwrap();
}

fn first(anc: &mut SocketAncillary<'_>) -> Option<OwnedFd> {
    anc.messages().find_map(|msg| match msg {
        AncillaryData::ScmRights(mut rights) => rights.next(),
        _ => None,
    })
}

#[test]
fn send_buffer_never_yields_owners() {
    let _lock = FD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let file = File::open("/dev/null").unwrap();
    let mut buf = vec![0; 128];
    let mut anc = SocketAncillary::new(&mut buf);
    anc.add_fds(&[file.as_fd()]).unwrap();
    let unexpected = first(&mut anc);
    let had_owner = unexpected.is_some();
    // Keep the red-state diagnostic from closing the caller's descriptor.
    std::mem::forget(unexpected);
    assert!(
        !had_owner,
        "send buffer manufactured an owner of a borrowed fd"
    );
    assert!(file.metadata().is_ok());
}

#[test]
fn receive_messages_are_consumed_once() {
    let _lock = FD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (tx, rx) = UnixStream::pair().unwrap();
    let file = File::open("/dev/null").unwrap();
    tx.send_fds(b"x", &[&file]).unwrap();
    let mut buf = vec![0; 128];
    let mut anc = SocketAncillary::new(&mut buf);
    receive(&rx, &mut anc);
    let fd = first(&mut anc).unwrap();
    let repeated = first(&mut anc);
    let duplicated = repeated.is_some();
    std::mem::forget(repeated); // Avoid double-close in the red state.
    assert!(!duplicated, "receive buffer yielded the same fd twice");
    assert!(File::from(fd).metadata().is_ok());
}

fn check_cleanup(action: impl FnOnce(&UnixStream, &UnixStream, &File, &mut SocketAncillary<'_>)) {
    let _lock = FD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let before = fd_count();
    {
        let (tx, rx) = UnixStream::pair().unwrap();
        let file = File::open("/dev/null").unwrap();
        let mut buf = vec![0; 128];
        let mut anc = SocketAncillary::new(&mut buf);
        tx.send_fds(b"x", &[&file, &file, &file]).unwrap();
        receive(&rx, &mut anc);
        action(&tx, &rx, &file, &mut anc);
    }
    assert_eq!(fd_count(), before, "unconsumed received descriptors leaked");
}

#[test]
fn dropping_receive_buffer_closes_unread_fds() {
    check_cleanup(|_, _, _, _| {});
}

#[test]
fn clearing_receive_buffer_closes_unread_fds() {
    check_cleanup(|_, _, _, anc| anc.clear());
}

#[test]
fn reusing_receive_buffer_closes_old_fds() {
    check_cleanup(|tx, rx, file, anc| {
        tx.send_fds(b"y", &[file]).unwrap();
        receive(rx, anc);
        assert!(first(anc).is_some());
    });
}

#[test]
fn partial_rights_iteration_closes_remainder() {
    check_cleanup(|_, _, _, anc| {
        drop(first(anc));
    });
}

#[test]
fn dropping_messages_without_iteration_closes_fds() {
    check_cleanup(|_, _, _, anc| {
        drop(anc.messages());
    });
}

#[test]
fn failed_receive_does_not_retain_previous_messages() {
    check_cleanup(|_, rx, _, anc| {
        rx.set_nonblocking(true).unwrap();
        let err = cmsg_recvmsg(rx.as_fd(), &mut [IoSliceMut::new(&mut [0; 1])], anc).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::WouldBlock);
        let stale = first(anc);
        assert!(stale.is_none(), "failed receive left stale messages");
    });
}

#[test]
fn receive_buffer_cannot_be_sent_until_cleared() {
    check_cleanup(|tx, _, _, anc| {
        let err = cmsg_sendmsg(tx.as_fd(), &[IoSlice::new(b"x")], anc).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        anc.clear();
        assert_eq!(
            cmsg_sendmsg(tx.as_fd(), &[IoSlice::new(b"x")], anc).unwrap(),
            1
        );
    });
}

#[test]
fn unaligned_buffers_roundtrip() {
    let _lock = FD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if std::env::var_os("UNIX_ANCILLARY_ALIGNMENT_CHILD").is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "unaligned_buffers_roundtrip", "--nocapture"])
            .env("UNIX_ANCILLARY_ALIGNMENT_CHILD", "1")
            .status()
            .unwrap();
        assert!(
            status.success(),
            "safe byte slices caused child failure: {status}"
        );
        return;
    }
    for offset in 0..std::mem::align_of::<libc::cmsghdr>() {
        let (tx, rx) = UnixStream::pair().unwrap();
        let file = File::open("/dev/null").unwrap();
        let mut send_buf = [0; 128];
        let mut anc = SocketAncillary::new(&mut send_buf[offset..]);
        anc.add_fds(&[file.as_fd()]).unwrap();
        // Multiple headers exercise offset advancement as well as the first header.
        anc.add_fds(&[file.as_fd()]).unwrap();
        cmsg_sendmsg(tx.as_fd(), &[IoSlice::new(b"x")], &anc).unwrap();
        let mut recv_buf = [0; 128];
        let mut recv = SocketAncillary::new(&mut recv_buf[offset..]);
        receive(&rx, &mut recv);
        let mut fds = Vec::new();
        for msg in recv.messages() {
            if let AncillaryData::ScmRights(rights) = msg {
                fds.extend(rights);
            }
        }
        assert_eq!(fds.len(), 2);
        for fd in fds {
            assert_ne!(
                unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC,
                0
            );
            assert!(File::from(fd).metadata().is_ok());
        }
    }
}
