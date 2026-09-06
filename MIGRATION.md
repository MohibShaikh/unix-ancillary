# Migrating to 0.6.0

0.6.0 repairs the safe low-level ancillary API and
retains the high-level blocking and Tokio FD-passing call shapes.

## From unix-ancillary 0.5

`SocketAncillary::messages` now requires `&mut self` and drains received
messages. A second call returns no previous messages. Dropping a message
iterator or a `ScmRights` iterator closes descriptors it has not yielded.
Dropping, clearing, or receiving again into the buffer closes unread descriptors.
Even a failed receive replaces the previous message state with an empty state.

```rust
use std::io::IoSliceMut;
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use unix_ancillary::{cmsg_recvmsg, AncillaryData, SocketAncillary};

fn receive(socket: &UnixStream) -> std::io::Result<Vec<std::os::fd::OwnedFd>> {
    let mut bytes = [0u8; 1];
    let mut storage = [0u8; 128];
    let mut ancillary = SocketAncillary::new(&mut storage);
    let result = cmsg_recvmsg(socket.as_fd(), &mut [IoSliceMut::new(&mut bytes)], &mut ancillary)?;
    if result.truncated || result.data_truncated {
        // All delivered descriptors close on return, even without iteration.
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "truncated message"));
    }
    let mut fds = Vec::new();
    for message in ancillary.messages() {
        match message {
            AncillaryData::ScmRights(rights) => fds.extend(rights),
            _ => {} // Non-exhaustive; include other kinds your protocol accepts.
        }
    }
    Ok(fds)
}
```

`add_fds` now ties descriptor borrows to the ancillary object's lifetime.
Keep the original files alive until the send buffer is dropped. Drop and
reconstruct the buffer to release that compile-time lifetime constraint;
`clear` resets its contents but cannot shorten the type's lifetime.

```rust
use std::io::IoSlice;
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use unix_ancillary::{cmsg_sendmsg, SocketAncillary};

fn send(socket: &UnixStream, file: &std::fs::File) -> std::io::Result<usize> {
    let mut storage = [0u8; 128];
    let mut ancillary = SocketAncillary::new(&mut storage);
    ancillary.add_fds(&[file.as_fd()]).unwrap();
    cmsg_sendmsg(socket.as_fd(), &[IoSlice::new(b"F")], &ancillary)
}
```

Send buffers no longer yield `OwnedFd` through `messages`. To forward received
descriptors, drain them into a `Vec<OwnedFd>`, then construct a separate send
buffer borrowing those descriptors. Calling `cmsg_sendmsg` on a receive buffer
returns `InvalidInput`; `add_fds`/`add_credentials` also reject a receive buffer
until it is cleared.

Ancillary truncation in convenience receives now returns `InvalidData` instead
of `Other`. Every delivered descriptor closes on the error. Low-level receives
continue returning their truncation flags. Linux/Android convenience receives
reserve additional space for automatically supplied credentials.

## From raw libc or sendfd

Replace raw descriptor arrays and manual closing with `AsFd` inputs and
`OwnedFd` outputs. For a dedicated one-byte, one-FD exchange:

```rust
use std::os::unix::net::UnixStream;
use unix_ancillary::UnixStreamExt;

fn transfer(tx: &UnixStream, rx: &UnixStream, file: &std::fs::File)
    -> std::io::Result<std::os::fd::OwnedFd>
{
    tx.send_fds_all(b"F", &[file])?;
    let mut marker = [0u8; 1];
    let (n, mut fds) = rx.recv_fds_exact_into::<1>(&mut marker)?;
    if n != 1 || marker != *b"F" {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid marker"));
    }
    Ok(fds.pop().expect("exact receive returned one fd"))
}
```

Use `File::from(fd)`, `UnixStream::from(fd)`, or another `From<OwnedFd>`
implementation to take ownership of the received resource. Do not additionally
close the raw descriptor. An FD can refer to an unexpected resource; validate
its type and protocol role when the peer is not trusted.

A stream is still a byte stream. `recv_fds_exact` validates one receive's count;
it does not read a complete frame. If `send_fds_all` errors or is cancelled,
descriptors and a prefix may already have arrived. Do not retry the whole send
with the same descriptors. Coordinate concurrent writers and recover through
your protocol, commonly by discarding the connection.

If the project already uses rustix ancillary buffers, consider keeping that
integration: it also offers owned descriptors and more low-level control.

## From 0.3 or 0.4

The 0.5 `AncillaryData` enum is non-exhaustive; add a wildcard to matches.
0.4 introduced strict receive-count methods, complete stream sends, and errors
for datagram payload truncation. Low-level `RecvResult` is non-exhaustive and
has `data_truncated`. These changes do not add stream message boundaries.

## Validation in your application

Run your actual handoff against the new version, including surplus descriptors,
truncation, early returns, and child exit. `cargo run --example multiprocess`
demonstrates a separate child receiving a read-only file. The
`dep_api_check` example preserves the blocking call shapes used by
runner-systemd-nspawn; it is not a full build of that downstream application.
