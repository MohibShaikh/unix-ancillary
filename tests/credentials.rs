#![cfg(any(target_os = "linux", target_os = "android"))]

use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use unix_ancillary::{set_passcred, AncillaryData, ScmCredentials, SocketAncillary, UnixStreamExt};

fn me() -> (i32, u32, u32) {
    (
        std::process::id() as i32,
        unsafe { libc::getuid() },
        unsafe { libc::getgid() },
    )
}

// The whole argument for putting credentials in this crate: one recvmsg
// returns descriptors and credentials out of the same cmsg buffer.
#[test]
fn fds_and_credentials_arrive_from_one_recvmsg() {
    let (tx, rx) = UnixStream::pair().unwrap();
    let (payload, _keep) = UnixStream::pair().unwrap();
    set_passcred(&rx, true).unwrap();

    let mut buf = vec![
        0u8;
        SocketAncillary::buffer_size_for_rights(1)
            + SocketAncillary::buffer_size_for_credentials()
    ];
    let mut out = SocketAncillary::new(&mut buf);
    out.add_fds(&[payload.as_fd()]).unwrap();
    out.add_credentials(ScmCredentials::for_this_process())
        .unwrap();

    unix_ancillary::cmsg_sendmsg(tx.as_fd(), &[std::io::IoSlice::new(b"x")], &out).unwrap();

    let mut data = [0u8; 8];
    let mut anc_buf = vec![0u8; 256];
    let mut parsed = SocketAncillary::new(&mut anc_buf);
    unix_ancillary::cmsg_recvmsg(
        rx.as_fd(),
        &mut [std::io::IoSliceMut::new(&mut data)],
        &mut parsed,
    )
    .unwrap();

    let mut saw_fd = 0;
    let mut saw_creds = None;
    for msg in parsed.messages() {
        match msg {
            AncillaryData::ScmRights(r) => saw_fd += r.count(),
            AncillaryData::Credentials(c) => saw_creds = Some(c),
            _ => {}
        }
    }

    assert_eq!(saw_fd, 1, "descriptor should arrive");
    let c = saw_creds.expect("credentials should arrive in the same message");
    let (pid, uid, gid) = me();
    assert_eq!((c.pid(), c.uid(), c.gid()), (pid, uid, gid));
}

// With SO_PASSCRED on, the kernel attaches credentials whether or not the
// sender asked. That is the common case and it should not need add_credentials.
#[test]
fn kernel_supplies_credentials_when_sender_sends_none() {
    let (tx, rx) = UnixStream::pair().unwrap();
    set_passcred(&rx, true).unwrap();

    tx.send_fds(b"hi", &[] as &[std::os::unix::net::UnixStream])
        .unwrap();

    let mut data = [0u8; 8];
    let mut anc_buf = vec![0u8; 256];
    let mut parsed = SocketAncillary::new(&mut anc_buf);
    unix_ancillary::cmsg_recvmsg(
        rx.as_fd(),
        &mut [std::io::IoSliceMut::new(&mut data)],
        &mut parsed,
    )
    .unwrap();

    let creds: Vec<_> = parsed
        .messages()
        .filter_map(|m| match m {
            AncillaryData::Credentials(c) => Some(c),
            _ => None,
        })
        .collect();

    assert_eq!(creds.len(), 1, "kernel should supply credentials");
    assert_eq!(creds[0].uid(), unsafe { libc::getuid() });
}

// Without SO_PASSCRED nothing arrives. Proves the flag is what does the work
// and that we are not fabricating the values locally.
#[test]
fn no_credentials_without_passcred() {
    let (tx, rx) = UnixStream::pair().unwrap();

    tx.send_fds(b"hi", &[] as &[std::os::unix::net::UnixStream])
        .unwrap();

    let mut data = [0u8; 8];
    let mut anc_buf = vec![0u8; 256];
    let mut parsed = SocketAncillary::new(&mut anc_buf);
    unix_ancillary::cmsg_recvmsg(
        rx.as_fd(),
        &mut [std::io::IoSliceMut::new(&mut data)],
        &mut parsed,
    )
    .unwrap();

    assert!(parsed
        .messages()
        .all(|m| !matches!(m, AncillaryData::Credentials(_))));
}

#[test]
fn maximum_rights_with_automatic_credentials_stream() {
    let (tx, rx) = UnixStream::pair().unwrap();
    set_passcred(&rx, true).unwrap();
    let file = std::fs::File::open("/dev/null").unwrap();
    tx.send_fds(b"x", &[&file; 253]).unwrap();
    assert_eq!(rx.recv_fds_exact::<253>().unwrap().fds.len(), 253);
}

#[test]
fn maximum_rights_with_automatic_credentials_datagram() {
    use std::os::unix::net::UnixDatagram;
    use unix_ancillary::UnixDatagramExt;
    let (tx, rx) = UnixDatagram::pair().unwrap();
    set_passcred(&rx, true).unwrap();
    let file = std::fs::File::open("/dev/null").unwrap();
    tx.send_fds(b"x", &[&file; 253]).unwrap();
    assert_eq!(rx.recv_fds_exact::<253>().unwrap().fds.len(), 253);
}

#[test]
fn slots_and_owned_receives_carry_credentials() {
    use unix_ancillary::recv_fds_slots;
    let (tx, rx) = UnixStream::pair().unwrap();
    set_passcred(&rx, true).unwrap();
    let file = tempfile::tempfile().unwrap();
    let me = ScmCredentials::for_this_process();

    tx.send_fds(b"x", &[&file]).unwrap();
    let (mut data, mut slots) = ([0u8; 1], [None]);
    let got = recv_fds_slots(&rx, &mut data, &mut slots).unwrap();
    assert_eq!((got.fds, got.credentials), (1, Some(me)));

    tx.send_fds(b"x", &[&file]).unwrap();
    let got = rx.recv_fds::<1>().unwrap();
    assert_eq!((got.fds.len(), got.credentials), (1, Some(me)));
}

#[test]
fn credentials_are_none_without_passcred() {
    let (tx, rx) = UnixStream::pair().unwrap();
    let file = tempfile::tempfile().unwrap();
    tx.send_fds(b"x", &[&file]).unwrap();
    assert_eq!(rx.recv_fds::<1>().unwrap().credentials, None);
}
