use std::io;
use std::os::unix::io::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::net::{UnixDatagram, UnixStream};

use crate::ancillary::{AncillaryData, SocketAncillary};
use crate::cmsg;

/// Extension trait for `UnixStream` adding fd-passing convenience methods.
pub trait UnixStreamExt {
    /// Send data and file descriptors over the stream.
    ///
    /// The `fds` slice contains borrowed references to file descriptors to send.
    /// The caller retains ownership.
    fn send_fds(&self, data: &[u8], fds: &[impl AsFd]) -> io::Result<usize>;

    /// Receive data and up to `N` file descriptors from the stream.
    ///
    /// Returns `(bytes_read, data_buffer, received_fds)`.
    /// Each received fd is an `OwnedFd` that will be closed on drop.
    fn recv_fds<const N: usize>(&self) -> io::Result<(usize, Vec<u8>, Vec<OwnedFd>)>;
}

impl UnixStreamExt for UnixStream {
    fn send_fds(&self, data: &[u8], fds: &[impl AsFd]) -> io::Result<usize> {
        let borrowed: Vec<BorrowedFd<'_>> = fds.iter().map(|f| f.as_fd()).collect();

        let mut buf = vec![0u8; SocketAncillary::buffer_size_for_rights(borrowed.len())];
        let mut ancillary = SocketAncillary::new(&mut buf);
        ancillary
            .add_fds(&borrowed)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

        let iov = [io::IoSlice::new(data)];
        cmsg::sendmsg_vectored(self.as_fd(), &iov, ancillary.buffer, ancillary.length)
    }

    fn recv_fds<const N: usize>(&self) -> io::Result<(usize, Vec<u8>, Vec<OwnedFd>)> {
        let mut data_buf = vec![0u8; 4096];
        let mut anc_buf = vec![0u8; SocketAncillary::buffer_size_for_rights(N)];

        let mut iov = [io::IoSliceMut::new(&mut data_buf)];
        let result = cmsg::recvmsg_vectored(self.as_fd(), &mut iov, &mut anc_buf)?;

        let mut ancillary = SocketAncillary {
            buffer: &mut anc_buf,
            length: result.ancillary_len,
            truncated: result.truncated,
        };

        let mut fds = Vec::new();
        for msg in ancillary.messages() {
            match msg {
                AncillaryData::ScmRights(rights) => {
                    fds.extend(rights);
                }
            }
        }

        // Clear truncated flag since we consumed the fds
        ancillary.truncated = false;

        data_buf.truncate(result.bytes_read);
        Ok((result.bytes_read, data_buf, fds))
    }
}

/// Extension trait for `UnixDatagram` adding fd-passing convenience methods.
///
/// The socket must be connected (via `connect()`) before using `send_fds`.
pub trait UnixDatagramExt {
    /// Send data and file descriptors over a connected datagram socket.
    fn send_fds(&self, data: &[u8], fds: &[impl AsFd]) -> io::Result<usize>;

    /// Receive data and up to `N` file descriptors from the datagram socket.
    fn recv_fds<const N: usize>(&self) -> io::Result<(usize, Vec<u8>, Vec<OwnedFd>)>;
}

impl UnixDatagramExt for UnixDatagram {
    fn send_fds(&self, data: &[u8], fds: &[impl AsFd]) -> io::Result<usize> {
        let borrowed: Vec<BorrowedFd<'_>> = fds.iter().map(|f| f.as_fd()).collect();

        let mut buf = vec![0u8; SocketAncillary::buffer_size_for_rights(borrowed.len())];
        let mut ancillary = SocketAncillary::new(&mut buf);
        ancillary
            .add_fds(&borrowed)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

        let iov = [io::IoSlice::new(data)];
        cmsg::sendmsg_vectored(self.as_fd(), &iov, ancillary.buffer, ancillary.length)
    }

    fn recv_fds<const N: usize>(&self) -> io::Result<(usize, Vec<u8>, Vec<OwnedFd>)> {
        let mut data_buf = vec![0u8; 65536];
        let mut anc_buf = vec![0u8; SocketAncillary::buffer_size_for_rights(N)];

        let mut iov = [io::IoSliceMut::new(&mut data_buf)];
        let result = cmsg::recvmsg_vectored(self.as_fd(), &mut iov, &mut anc_buf)?;

        let mut ancillary = SocketAncillary {
            buffer: &mut anc_buf,
            length: result.ancillary_len,
            truncated: result.truncated,
        };

        let mut fds = Vec::new();
        for msg in ancillary.messages() {
            match msg {
                AncillaryData::ScmRights(rights) => {
                    fds.extend(rights);
                }
            }
        }

        ancillary.truncated = false;

        data_buf.truncate(result.bytes_read);
        Ok((result.bytes_read, data_buf, fds))
    }
}
