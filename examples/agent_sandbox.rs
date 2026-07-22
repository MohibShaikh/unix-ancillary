//! Agent / tool sandbox: a supervisor opens exactly the resources a sandboxed
//! task is allowed to touch, then passes those descriptors to the sandbox over
//! a Unix socket. The sandbox is never granted `open()` rights to the
//! filesystem — it can only use the capabilities (fds) it was handed. This is
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
    let mut allowed = tempfile::tempfile()?;
    allowed.write_all(b"data the sandboxed agent is allowed to read")?;
    allowed.flush()?;
    println!("[supervisor] granting one read-only capability to the sandbox");
    supervisor.send_fds(b"capability", &[&allowed]).await?;

    // The sandbox receives only the fd — it has no filesystem access and never
    // calls open(). It can only act through the descriptor it was granted.
    let recv = sandbox.recv_fds::<1>().await?;
    assert_eq!(&recv.data[..], b"capability");
    let mut cap: std::fs::File = recv.fds.into_iter().next().unwrap().into();
    cap.seek(std::io::SeekFrom::Start(0))?;
    let mut contents = String::new();
    cap.read_to_string(&mut contents)?;
    println!("[sandbox] used the granted capability, read: {contents:?}");

    Ok(())
}
