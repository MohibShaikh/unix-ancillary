//! The contract a framed receiver built on `recv_fds_into` depends on: ask for
//! one more descriptor than allowed to detect an oversized batch, and treat
//! the descriptors as owned from the first chunk. Own test binary because the
//! leak checks read the process-global fd table.
#![cfg(feature = "tokio")]

use std::future::{poll_fn, Future};
use std::os::unix::net::UnixStream as StdUnixStream;
use std::sync::{Mutex, MutexGuard};
use std::task::Poll;
use tokio::net::UnixStream;
use unix_ancillary::{AsyncUnixStreamExt, UnixStreamExt};

const ALLOWED: usize = 5;

static FD_LOCK: Mutex<()> = Mutex::new(());

fn fd_lock() -> MutexGuard<'static, ()> {
    FD_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Hold the lock around the whole runtime: building and dropping one opens and
/// closes descriptors of its own, which would disturb another test's count.
fn run(test: impl Future<Output = ()>) {
    let _guard = fd_lock();
    tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()
        .unwrap()
        .block_on(test);
}

fn count_open_fds() -> usize {
    (0..4096)
        // SAFETY: F_GETFD on any int is defined; returns -1/EBADF if closed.
        .filter(|&fd| unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0)
        .count()
}

fn pair() -> (StdUnixStream, UnixStream) {
    let (tx, rx) = StdUnixStream::pair().unwrap();
    rx.set_nonblocking(true).unwrap();
    (tx, UnixStream::from_std(rx).unwrap())
}

#[test]
fn oversized_batch_returns_ok_and_closes_the_surplus() {
    run(async {
        let (tx, rx) = pair();
        let file = tempfile::tempfile().unwrap();
        let before = count_open_fds();

        tx.send_fds(b"frame\n", &[&file; ALLOWED + 5]).unwrap();
        let mut buffer = [0u8; 4096];
        let (n, fds) = rx
            .recv_fds_into::<{ ALLOWED + 1 }>(&mut buffer)
            .await
            .unwrap();

        assert_eq!(&buffer[..n], b"frame\n");
        assert_eq!(fds.len(), ALLOWED + 1);
        assert_eq!(
            count_open_fds(),
            before + ALLOWED + 1,
            "surplus stayed open"
        );
        drop(fds);
        assert_eq!(count_open_fds(), before);
    });
}

#[test]
fn descriptors_arrive_with_the_first_chunk_of_a_large_payload() {
    run(async {
        let (tx, rx) = pair();
        let file = tempfile::tempfile().unwrap();
        let before = count_open_fds();
        let payload = vec![b'x'; 16 * 1024];

        let sender = std::thread::spawn(move || {
            tx.send_fds_all(&payload, &[&file]).unwrap();
            file
        });

        let mut buffer = [0u8; 4096];
        let (mut total, fds) = rx
            .recv_fds_into::<{ ALLOWED + 1 }>(&mut buffer)
            .await
            .unwrap();
        assert_eq!(fds.len(), 1);
        drop(fds);
        while total < 16 * 1024 {
            let (n, fds) = rx
                .recv_fds_into::<{ ALLOWED + 1 }>(&mut buffer)
                .await
                .unwrap();
            assert!(fds.is_empty(), "descriptors were delivered twice");
            assert_ne!(n, 0, "stream ended early");
            total += n;
        }
        // The thread dropped tx; the file comes back and closes here.
        drop(sender.join().unwrap());
        assert_eq!(count_open_fds() + 2, before);
    });
}

#[test]
fn peer_close_delivers_descriptors_then_end_of_stream() {
    run(async {
        let (tx, rx) = pair();
        let file = tempfile::tempfile().unwrap();
        let before = count_open_fds();

        tx.send_fds(b"x", &[&file]).unwrap();
        drop(tx);

        let mut buffer = [0u8; 16];
        let (n, fds) = rx
            .recv_fds_into::<{ ALLOWED + 1 }>(&mut buffer)
            .await
            .unwrap();
        assert_eq!((n, fds.len()), (1, 1));
        drop(fds);
        let (n, fds) = rx
            .recv_fds_into::<{ ALLOWED + 1 }>(&mut buffer)
            .await
            .unwrap();
        assert_eq!((n, fds.len()), (0, 0));
        assert_eq!(count_open_fds() + 1, before);
    });
}

#[test]
fn dropping_a_pending_receive_loses_nothing() {
    run(async {
        let (tx, rx) = pair();
        let file = tempfile::tempfile().unwrap();
        let before = count_open_fds();

        for _ in 0..50 {
            let mut buffer = [0u8; 16];
            let mut pending = Box::pin(rx.recv_fds_into::<{ ALLOWED + 1 }>(&mut buffer));
            poll_fn(|cx| {
                assert!(pending.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
        }

        tx.send_fds(b"x", &[&file]).unwrap();
        let mut buffer = [0u8; 16];
        let (n, fds) = rx
            .recv_fds_into::<{ ALLOWED + 1 }>(&mut buffer)
            .await
            .unwrap();
        assert_eq!((n, fds.len()), (1, 1));
        drop(fds);
        assert_eq!(count_open_fds(), before);
    });
}
