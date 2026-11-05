//! Descriptors to and from an address, on sockets that are not connected.

use std::io::ErrorKind;
use std::os::unix::net::{SocketAddr, UnixDatagram};
use unix_ancillary::{recv_fds_from, send_fds_to};

#[test]
fn named_sender_round_trips_and_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let (a_path, b_path) = (dir.path().join("a"), dir.path().join("b"));
    let (a, b) = (
        UnixDatagram::bind(&a_path).unwrap(),
        UnixDatagram::bind(&b_path).unwrap(),
    );
    let file = tempfile::tempfile().unwrap();

    let to = SocketAddr::from_pathname(&b_path).unwrap();
    assert_eq!(send_fds_to(&a, &to, b"hi", &[&file]).unwrap(), 2);

    let (mut data, mut slots) = ([0u8; 8], [None]);
    let (got, from) = recv_fds_from(&b, &mut data, &mut slots).unwrap();
    assert_eq!((got.bytes_read, got.fds), (2, 1));
    assert_eq!(&data[..2], b"hi");
    assert!(slots[0].is_some());
    assert_eq!(from.unwrap().as_pathname(), Some(a_path.as_path()));
}

#[test]
fn unnamed_sender_has_no_address() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("b");
    let b = UnixDatagram::bind(&path).unwrap();
    let a = UnixDatagram::unbound().unwrap();
    let file = tempfile::tempfile().unwrap();

    let to = SocketAddr::from_pathname(&path).unwrap();
    send_fds_to(&a, &to, b"x", &[&file]).unwrap();

    let (mut data, mut slots) = ([0u8; 1], [None]);
    let (got, from) = recv_fds_from(&b, &mut data, &mut slots).unwrap();
    assert_eq!(got.fds, 1);
    assert!(from.is_none(), "{from:?}");
}

#[test]
fn sending_to_an_unnamed_address_is_invalid_input() {
    let a = UnixDatagram::unbound().unwrap();
    let unnamed = a.local_addr().unwrap();
    let none: [&std::fs::File; 0] = [];
    let err = send_fds_to(&a, &unnamed, b"x", &none).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidInput);
}

#[cfg(any(target_os = "linux", target_os = "android"))]
#[test]
fn abstract_addresses_round_trip() {
    #[cfg(target_os = "android")]
    use std::os::android::net::SocketAddrExt;
    #[cfg(target_os = "linux")]
    use std::os::linux::net::SocketAddrExt;

    let pid = std::process::id();
    let a_addr = SocketAddr::from_abstract_name(format!("ua-a-{pid}")).unwrap();
    let b_addr = SocketAddr::from_abstract_name(format!("ua-b-{pid}")).unwrap();
    let (a, b) = (
        UnixDatagram::bind_addr(&a_addr).unwrap(),
        UnixDatagram::bind_addr(&b_addr).unwrap(),
    );
    let file = tempfile::tempfile().unwrap();
    send_fds_to(&a, &b_addr, b"x", &[&file]).unwrap();

    let (mut data, mut slots) = ([0u8; 1], [None]);
    let (got, from) = recv_fds_from(&b, &mut data, &mut slots).unwrap();
    assert_eq!(got.fds, 1);
    assert_eq!(from.unwrap().as_abstract_name(), a_addr.as_abstract_name());
}
