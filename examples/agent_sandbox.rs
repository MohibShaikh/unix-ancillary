//! Capability transfer for a future agent / tool sandbox: a supervisor opens exactly the resources a sandboxed
//! task is allowed to touch, then passes those descriptors over a Unix socket.
//! This demo does not enforce a sandbox or revoke filesystem access. This is
//! the capability-passing model behind seccomp/landlock sandboxes and is a
//! natural fit for confining LLM-agent tool execution.
//!
//! Received fds are `OwnedFd` with `FD_CLOEXEC` set, so a granted capability
//! can't leak into a subprocess the sandbox later execs.
//!
//! Run with: `cargo run --example agent_sandbox --features tokio`
//!
//! Single-process socketpair demo; in production the sandbox is a separate
//! process behind seccomp/namespaces on the other end of the socket.

use std::io::{Read, Seek, Write};

use tokio::net::UnixStream;
use unix_ancillary::AsyncUnixStreamExt;

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    let (supervisor, sandbox) = UnixStream::pair()?;

    // Supervisor pre-opens the one file the task is allowed to read and passes
    // it as a capability — no path ever crosses the boundary.
    let mut allowed = tempfile::NamedTempFile::new()?;
    allowed.write_all(b"data the sandboxed agent is allowed to read")?;
    allowed.flush()?;
    println!("[supervisor] granting one read-only capability to the sandbox");
    let read_only = std::fs::File::open(allowed.path())?;
    supervisor
        .send_fds_all(b"capability", &[&read_only])
        .await?;

    // Use the granted read-only fd. A real sandbox must separately restrict
    // filesystem access; this single-process demo does not do that.
    let recv = sandbox.recv_fds::<1>().await?;
    assert_eq!(&recv.data[..], b"capability");
    let mut cap: std::fs::File = recv.fds.into_iter().next().unwrap().into();
    assert!(
        cap.write_all(b"must be denied").is_err(),
        "granted capability must be read-only"
    );
    cap.seek(std::io::SeekFrom::Start(0))?;
    let mut contents = String::new();
    cap.read_to_string(&mut contents)?;
    println!("[sandbox] used the granted capability, read: {contents:?}");

    Ok(())
}
