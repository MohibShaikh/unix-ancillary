//! Heap allocations per handoff, counted per thread so the test harness cannot
//! disturb the numbers. Linux only: other platforms have not had their counts
//! measured.
#![cfg(target_os = "linux")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::io::{IoSlice, IoSliceMut};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use unix_ancillary::{cmsg_recvmsg, cmsg_sendmsg, recv_fds_slots, SocketAncillary, UnixStreamExt};

struct Counting;

thread_local! {
    static COUNT: Cell<usize> = const { Cell::new(0) };
}

fn bump() {
    let _ = COUNT.try_with(|count| count.set(count.get() + 1));
}

// SAFETY: every call forwards to System with its arguments unchanged.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        bump();
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        bump();
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        bump();
        unsafe { System.realloc(ptr, layout, size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn allocations<T>(f: impl FnOnce() -> T) -> (usize, T) {
    let before = COUNT.with(Cell::get);
    let value = f();
    (COUNT.with(Cell::get) - before, value)
}

#[test]
fn convenience_send_does_not_allocate() {
    let (tx, _rx) = UnixStream::pair().unwrap();
    let file = std::fs::File::open("/dev/null").unwrap();
    let (count, sent) = allocations(|| tx.send_fds(b"x", &[&file]).unwrap());
    assert_eq!((count, sent), (0, 1));
}

#[test]
fn receive_allocates_only_the_returned_vec() {
    let (tx, rx) = UnixStream::pair().unwrap();
    let file = std::fs::File::open("/dev/null").unwrap();
    tx.send_fds(b"x", &[&file, &file, &file]).unwrap();
    let mut data = [0u8; 8];
    let (count, (_, fds)) = allocations(|| rx.recv_fds_into::<2>(&mut data).unwrap());
    assert_eq!(fds.len(), 2);
    assert_eq!(count, 1);
}

#[test]
fn receive_without_descriptors_does_not_allocate() {
    let (tx, rx) = UnixStream::pair().unwrap();
    let none: [&std::fs::File; 0] = [];
    tx.send_fds(b"x", &none).unwrap();
    let mut data = [0u8; 8];
    let (count, (n, fds)) = allocations(|| rx.recv_fds_into::<2>(&mut data).unwrap());
    assert_eq!((count, n, fds.len()), (0, 1, 0));
}

#[test]
fn low_level_send_does_not_allocate() {
    let (tx, _rx) = UnixStream::pair().unwrap();
    let file = std::fs::File::open("/dev/null").unwrap();
    let mut storage = [0u8; 64];
    let (count, sent) = allocations(|| {
        let mut ancillary = SocketAncillary::new(&mut storage);
        ancillary.add_fds(&[file.as_fd()]).unwrap();
        cmsg_sendmsg(tx.as_fd(), &[IoSlice::new(b"x")], &ancillary).unwrap()
    });
    assert_eq!((count, sent), (0, 1));
}

#[test]
fn slots_receive_does_not_allocate() {
    let (tx, rx) = UnixStream::pair().unwrap();
    let file = std::fs::File::open("/dev/null").unwrap();
    tx.send_fds(b"x", &[&file, &file, &file]).unwrap();
    let mut data = [0u8; 8];
    let mut slots = [None, None];
    let (count, got) = allocations(|| recv_fds_slots(&rx, &mut data, &mut slots).unwrap());
    assert_eq!((count, got.fds, got.surplus_closed), (0, 2, 1));
}

#[test]
fn slots_receive_stays_off_the_heap_with_many_slots() {
    let (tx, rx) = UnixStream::pair().unwrap();
    let file = std::fs::File::open("/dev/null").unwrap();
    tx.send_fds(b"x", &[&file]).unwrap();
    let mut data = [0u8; 8];
    let mut slots: Vec<Option<std::os::fd::OwnedFd>> = (0..5000).map(|_| None).collect();
    let (count, got) = allocations(|| recv_fds_slots(&rx, &mut data, &mut slots).unwrap());
    assert_eq!((count, got.fds), (0, 1));
}

#[test]
fn low_level_receive_allocation_does_not_grow_with_descriptors() {
    let (tx, rx) = UnixStream::pair().unwrap();
    let file = std::fs::File::open("/dev/null").unwrap();
    let mut storage = vec![0u8; SocketAncillary::buffer_size_for_rights(64)];
    let mut ancillary = SocketAncillary::new(&mut storage);
    let mut data = [0u8; 8];
    for _ in 0..2 {
        tx.send_fds(b"x", &[&file; 64]).unwrap();
        let (count, _) = allocations(|| {
            cmsg_recvmsg(
                rx.as_fd(),
                &mut [IoSliceMut::new(&mut data)],
                &mut ancillary,
            )
            .unwrap()
        });
        assert!(count <= 2, "{count} allocations");
    }
}

#[test]
fn reused_low_level_receive_allocates_once_per_rights_message() {
    let (tx, rx) = UnixStream::pair().unwrap();
    let file = std::fs::File::open("/dev/null").unwrap();
    let mut storage = [0u8; 64];
    let mut ancillary = SocketAncillary::new(&mut storage);
    let mut data = [0u8; 8];
    tx.send_fds(b"x", &[&file]).unwrap();
    cmsg_recvmsg(
        rx.as_fd(),
        &mut [IoSliceMut::new(&mut data)],
        &mut ancillary,
    )
    .unwrap();
    tx.send_fds(b"x", &[&file]).unwrap();
    let (count, _) = allocations(|| {
        cmsg_recvmsg(
            rx.as_fd(),
            &mut [IoSliceMut::new(&mut data)],
            &mut ancillary,
        )
        .unwrap()
    });
    assert_eq!(count, 1);
}
