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
