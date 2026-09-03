// The exact call shapes runner-systemd-nspawn 0.1.3 uses (systemd_dbus.rs:47,
// 64, 87). Compiles as a guard that the non_exhaustive and vectored additions
// stay source-compatible with the one real downstream.
use std::os::unix::net::UnixStream;
use unix_ancillary::UnixStreamExt as _;

fn main() -> std::io::Result<()> {
    let (tx, rx) = UnixStream::pair()?;
    let (s, _keep) = UnixStream::pair()?;
    tx.send_fds(b"hello", &[&s])
        .expect("send system_bus_socket fd");
    let recv = rx.recv_fds::<1>()?;
    println!("data={:?} fds={}", recv.data, recv.fds.len());
    Ok(())
}
