#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Arbitrary bytes never become OwnedFd. Exercise every alignment offset
    // through the same bounded byte parser used for kernel receive data.
    for offset in 0..=data.len().min(std::mem::align_of::<usize>()) {
        unix_ancillary::__fuzz_parse(&data[offset..]);
    }
});
