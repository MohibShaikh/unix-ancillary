//! Privilege separation: a privileged process binds a socket, then hands the
//! listening socket to an unprivileged worker over a Unix socket. The worker
//! serves connections without ever holding the right to bind the port itself
//! — a classic Unix privsep pattern (nginx, postfix, OpenSSH all do this).
//!
//! The received listener arrives as an `OwnedFd` with `FD_CLOEXEC` set, so the
//! worker can't accidentally leak the listening socket into anything it later
//! `exec`s.
//!
//! Both roles run in one process over a socketpair for a self-contained,
//! runnable demo. In production the worker is a separate, privilege-dropped
//! process on the other end of the socket (fork/exec).

use std::net::TcpListener;
use std::os::unix::io::OwnedFd;
use std::os::unix::net::UnixStream;

use unix_ancillary::UnixStreamExt;

fn main() -> std::io::Result<()> {
    let (priv_side, worker_side) = UnixStream::pair()?;

    // --- privileged side: bind, then pass the listener fd to the worker ---
    // Port 0 = any free port, so the example needs no privileges to run; in
    // production this is where you'd bind :80 / :443 as root.
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?;
    println!("[privileged] bound {addr}, handing the listening socket to worker");
    priv_side.send_fds(b"listener", &[&listener])?;
    drop(listener); // privileged side is done with it

    // --- worker side: receive the listener fd and serve from it ---
    let recv = worker_side.recv_fds::<1>()?;
    assert_eq!(&recv.data[..], b"listener");
    let fd: OwnedFd = recv.fds.into_iter().next().unwrap();
    let worker_listener = TcpListener::from(fd);
    println!(
        "[worker] received the listening socket, now serving on {} \
         (never called bind() itself)",
        worker_listener.local_addr()?
    );
    // worker_listener.accept()? ... in a real server the worker loops here.

    Ok(())
}
