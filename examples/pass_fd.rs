//! Example: send a file descriptor between two ends of a Unix socketpair.

use std::io::{Read, Seek, Write};
use std::os::unix::net::UnixStream;
use unix_ancillary::UnixStreamExt;

fn main() -> std::io::Result<()> {
    let (tx, rx) = UnixStream::pair()?;

    let mut file = tempfile::tempfile()?;
    file.write_all(b"Hello from the other side!")?;
    file.flush()?;

    println!("Sending file descriptor...");
    tx.send_fds(b"here's a file", &[&file])?;

    println!("Receiving file descriptor...");
    let recv = rx.recv_fds::<1>()?;
    println!(
        "Received {} bytes: {:?}",
        recv.data.len(),
        std::str::from_utf8(&recv.data).unwrap()
    );
    println!("Received {} fd(s)", recv.fds.len());

    let mut received: std::fs::File = recv.fds.into_iter().next().unwrap().into();
    received.seek(std::io::SeekFrom::Start(0))?;
    let mut contents = String::new();
    received.read_to_string(&mut contents)?;
    println!("File contents via received fd: {contents:?}");

    Ok(())
}
