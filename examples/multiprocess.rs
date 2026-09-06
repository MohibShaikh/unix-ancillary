//! A parent sends a read-only file to a separately spawned child. Only the
//! private IPC socket path is passed as an argument; file contents travel via
//! the received descriptor. This demonstrates transfer, not sandbox enforcement.
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::Command;
use std::time::{Duration, Instant};
use unix_ancillary::UnixStreamExt;

const CONTENTS: &str = "read through a descriptor from the parent\n";
const TIMEOUT: Duration = Duration::from_secs(10);

fn child(path: &std::path::Path) -> io::Result<()> {
    let mut socket = UnixStream::connect(path)?;
    socket.set_read_timeout(Some(TIMEOUT))?;
    socket.set_write_timeout(Some(TIMEOUT))?;
    // One marker byte and one FD on a dedicated socket. No assumption about
    // arbitrary stream message boundaries is needed for this exchange.
    let mut marker = [0; 1];
    let (n, mut fds) = socket.recv_fds_exact_into::<1>(&mut marker)?;
    if n != 1 || marker != *b"F" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid file marker",
        ));
    }
    let mut file = File::from(fds.pop().expect("exact receive returned one fd"));
    let mut contents = String::new();
    file.read_to_string(&mut contents)?;
    if contents != CONTENTS || file.write_all(b"must be denied").is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid read-only capability",
        ));
    }
    socket.write_all(b"Y")?;
    println!(
        "child {} read the file; writes were denied",
        std::process::id()
    );
    Ok(())
}

fn main() -> io::Result<()> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() == Some(std::ffi::OsStr::new("--child")) {
        let path = args
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing socket path"))?;
        return child(std::path::Path::new(&path));
    }
    let directory = tempfile::tempdir()?; // Private directory protects the socket path.
    let path = directory.path().join("channel.sock");
    let listener = UnixListener::bind(&path)?;
    listener.set_nonblocking(true)?;
    let mut original = tempfile::NamedTempFile::new_in(directory.path())?;
    original.write_all(CONTENTS.as_bytes())?;
    let read_only = File::open(original.path())?;
    let mut worker = Command::new(std::env::current_exe()?)
        .arg("--child")
        .arg(&path)
        .spawn()?;
    let result = (|| {
        let deadline = Instant::now() + TIMEOUT;
        let mut socket = loop {
            match listener.accept() {
                Ok((socket, _)) => break socket,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    if worker.try_wait()?.is_some() || Instant::now() >= deadline {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "child did not connect",
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => return Err(e),
            }
        };
        // Accepted sockets can inherit the listener's nonblocking mode.
        socket.set_nonblocking(false)?;
        socket.set_read_timeout(Some(TIMEOUT))?;
        socket.set_write_timeout(Some(TIMEOUT))?;
        socket.send_fds_all(b"F", &[&read_only])?;
        let mut ack = [0; 1];
        socket.read_exact(&mut ack)?;
        if ack != *b"Y" {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid acknowledgment",
            ));
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = worker.kill();
    }
    let status = worker.wait()?;
    result?;
    if !status.success() {
        return Err(io::Error::other("child failed"));
    }
    println!(
        "parent {} completed the descriptor handoff",
        std::process::id()
    );
    Ok(())
}
