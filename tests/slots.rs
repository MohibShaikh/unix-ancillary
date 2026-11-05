//! `recv_fds_slots`. Own test binary because the leak checks read the
//! process-global fd table; tests serialize through `FD_LOCK`.

use std::os::unix::net::{UnixDatagram, UnixStream};
use std::sync::{Mutex, MutexGuard};
use unix_ancillary::{recv_fds_slots, UnixDatagramExt, UnixStreamExt};

static FD_LOCK: Mutex<()> = Mutex::new(());

fn fd_lock() -> MutexGuard<'static, ()> {
    FD_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn count_open_fds() -> usize {
    (0..4096)
        // SAFETY: F_GETFD on any int is defined; returns -1/EBADF if closed.
        .filter(|&fd| unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0)
        .count()
}

#[test]
fn fills_slots_closes_surplus_and_replaces_old_contents() {
    let _guard = fd_lock();
    let (tx, rx) = UnixStream::pair().unwrap();
    let file = tempfile::tempfile().unwrap();
    let before = count_open_fds();

    tx.send_fds(b"hello", &[&file, &file, &file]).unwrap();
    let mut data = [0u8; 8];
    let mut slots = [None, None];
    let got = recv_fds_slots(&rx, &mut data, &mut slots).unwrap();
    assert_eq!((got.bytes_read, got.fds, got.surplus_closed), (5, 2, 1));
    assert_eq!(&data[..5], b"hello");
    assert!(slots.iter().all(Option::is_some));
    assert_eq!(count_open_fds(), before + 2);

    tx.send_fds(b"x", &[&file]).unwrap();
    let got = recv_fds_slots(&rx, &mut data, &mut slots).unwrap();
    assert_eq!((got.fds, got.surplus_closed), (1, 0));
    assert!(slots[0].is_some() && slots[1].is_none());
    assert_eq!(count_open_fds(), before + 1);

    drop(slots);
    assert_eq!(count_open_fds(), before);
}

#[test]
fn received_descriptors_have_cloexec() {
    use std::os::fd::AsRawFd;
    let _guard = fd_lock();
    let (tx, rx) = UnixStream::pair().unwrap();
    let file = tempfile::tempfile().unwrap();
    tx.send_fds(b"x", &[&file]).unwrap();
    let mut data = [0u8; 1];
    let mut slots = [None];
    recv_fds_slots(&rx, &mut data, &mut slots).unwrap();
    let raw = slots[0].as_ref().unwrap().as_raw_fd();
    // SAFETY: F_GETFD on a live descriptor.
    let flags = unsafe { libc::fcntl(raw, libc::F_GETFD) };
    assert_eq!(flags & libc::FD_CLOEXEC, libc::FD_CLOEXEC);
}

#[test]
fn oversized_datagram_is_an_error_with_empty_slots() {
    let _guard = fd_lock();
    let (tx, rx) = UnixDatagram::pair().unwrap();
    let file = tempfile::tempfile().unwrap();
    let before = count_open_fds();
    tx.send_fds(b"too long", &[&file]).unwrap();

    let mut small = [0u8; 2];
    let mut slots = [None];
    let err = recv_fds_slots(&rx, &mut small, &mut slots).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(slots[0].is_none());
    assert_eq!(count_open_fds(), before);
}

#[test]
fn nothing_arriving_keeps_wouldblock_for_readiness_adapters() {
    let _guard = fd_lock();
    let (_tx, rx) = UnixStream::pair().unwrap();
    rx.set_nonblocking(true).unwrap();
    let mut data = [0u8; 1];
    let mut slots = [None];
    let err = recv_fds_slots(&rx, &mut data, &mut slots).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::WouldBlock);
}

#[cfg(feature = "tokio")]
#[test]
fn works_on_a_tokio_socket_after_waiting() {
    use tokio::io::Interest;
    let _guard = fd_lock();
    tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()
        .unwrap()
        .block_on(async {
            let (tx, rx) = tokio::net::UnixStream::pair().unwrap();
            let file = tempfile::tempfile().unwrap();
            let mut data = [0u8; 8];
            let mut slots = [None];
            let receive = rx.async_io(Interest::READABLE, || {
                recv_fds_slots(&rx, &mut data, &mut slots)
            });
            let send = async {
                tokio::task::yield_now().await;
                unix_ancillary::AsyncUnixStreamExt::send_fds(&tx, b"hi", &[&file]).await
            };
            let (got, sent) = tokio::join!(receive, send);
            assert_eq!(sent.unwrap(), 2);
            let got = got.unwrap();
            assert_eq!((got.bytes_read, got.fds), (2, 1));
            assert!(slots[0].is_some());
        });
}
