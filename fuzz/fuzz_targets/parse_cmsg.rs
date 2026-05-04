#![no_main]

//! Fuzz the cmsg parser with arbitrary kernel-shaped buffers.
//!
//! The parser walks a byte buffer using `CMSG_FIRSTHDR` / `CMSG_NXTHDR` /
//! `CMSG_DATA` macros from `libc`, doing pointer arithmetic in unsafe code.
//! This target hammers it with random bytes and asserts the iterator
//! terminates without UB. Any crash, hang, or sanitizer hit is a real bug.
//!
//! Note: we deliberately do NOT consume the resulting `OwnedFd`s — those
//! values would be the byte-pattern interpreted as a file descriptor, not a
//! kernel-issued one, and dropping them would call `close()` on an arbitrary
//! integer. Counting the iterator with `rights.count()` advances it without
//! producing `OwnedFd`s in the user-visible sense (the iterator does still
//! construct them transiently — see safety note below).

use libfuzzer_sys::fuzz_target;
use std::mem::ManuallyDrop;

fuzz_target!(|data: &[u8]| {
    // SAFETY: every yielded `OwnedFd` is wrapped in `ManuallyDrop` below,
    // so we never close arbitrary integers interpreted as fds.
    let messages = unsafe { unix_ancillary::__fuzz_parse(data) };

    let mut total = 0usize;
    for msg in messages {
        match msg {
            unix_ancillary::AncillaryData::ScmRights(rights) => {
                // Wrap each `OwnedFd` in `ManuallyDrop` so we don't `close()`
                // an arbitrary integer interpreted as a file descriptor —
                // the bytes here are fuzz input, not a kernel-issued fd.
                for fd in rights {
                    let _hold = ManuallyDrop::new(fd);
                    total = total.wrapping_add(1);
                    if total > 1_000_000 {
                        // Bound iteration to catch potential pathological
                        // cmsg layouts that loop forever.
                        return;
                    }
                }
            }
        }
    }
});
