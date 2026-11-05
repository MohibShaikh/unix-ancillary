//! A blocking `send_fds_all` whose first `sendmsg` really does stop short. The
//! send timeout makes the kernel return the bytes accepted so far while the
//! reader is still asleep, so the completion loop has to run.
#![cfg(target_os = "linux")]

use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::time::Duration;
use unix_ancillary::UnixStreamExt;

#[test]
fn completion_loop_finishes_the_payload_with_one_descriptor() {
    let (tx, mut rx) = UnixStream::pair().unwrap();
    tx.set_write_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    let sndbuf: libc::c_int = 2048;
    // SAFETY: live socket, correct option length for SO_SNDBUF.
    let rc = unsafe {
        libc::setsockopt(
            tx.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_SNDBUF,
            (&sndbuf as *const libc::c_int).cast(),
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    assert_eq!(rc, 0);

    let file = tempfile::tempfile().unwrap();
    let payload = vec![b'x'; 256 * 1024];
    let expected = payload.len();
    let sender = std::thread::spawn(move || tx.send_fds_all(&payload, &[&file]));

    // Stay asleep past the first timeout, and wake before a second one.
    std::thread::sleep(Duration::from_millis(300));

    let mut buffer = [0u8; 4096];
    let (mut total, fds) = rx.recv_fds_into::<8>(&mut buffer).unwrap();
    assert_eq!(fds.len(), 1, "descriptor must ride the first send");
    let mut descriptors = fds.len();
    while total < expected {
        let (n, fds) = rx.recv_fds_into::<8>(&mut buffer).unwrap();
        assert_ne!(n, 0, "stream ended early");
        total += n;
        descriptors += fds.len();
    }
    sender.join().unwrap().unwrap();
    assert_eq!(descriptors, 1, "descriptors were retransmitted");
    assert_eq!(total, expected);
    let mut rest = [0u8; 1];
    rx.set_nonblocking(true).unwrap();
    assert!(rx.read(&mut rest).is_err() || rest[0] == 0);
}
