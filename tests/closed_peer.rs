//! A send to a closed peer must report `BrokenPipe` on every platform, with or
//! without descriptors attached.

use std::io::ErrorKind;
use std::os::unix::net::UnixStream;
use unix_ancillary::UnixStreamExt;

#[test]
fn send_with_fds_reports_broken_pipe() {
    let (tx, rx) = UnixStream::pair().unwrap();
    drop(rx);
    let file = tempfile::tempfile().unwrap();
    let err = tx.send_fds(b"x", &[&file]).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::BrokenPipe, "{err:?}");
}

#[test]
fn send_without_fds_reports_broken_pipe() {
    let (tx, rx) = UnixStream::pair().unwrap();
    drop(rx);
    let none: [&std::fs::File; 0] = [];
    let err = tx.send_fds(b"x", &none).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::BrokenPipe, "{err:?}");
}
