//! `SYS_SHM_CREATE`'s length is refused, never summed unchecked: one whose
//! 2 MiB rounding wraps `u64` is `InvalidArgument`, and one whose rounding
//! fits but no memory could back is `ResourceExhausted`.
//!
//! The verdict is that this process reaches its last line: a trap in the
//! kernel's rounding ends the caller before its refusal can be read.

use toyos_abi::syscall::{self, SyscallError};

const PAGE_2M: u64 = 2 * 1024 * 1024;

/// Zero, refused before any rounding; the largest length whose rounding fits
/// `u64`, which a rounding that sums `size + PAGE_2M` before subtracting one
/// wraps on; then the first and last lengths whose rounding wraps.
const LENGTHS: [(u64, SyscallError); 4] = [
    (0, SyscallError::InvalidArgument),
    (u64::MAX - (PAGE_2M - 1), SyscallError::ResourceExhausted),
    (u64::MAX - (PAGE_2M - 2), SyscallError::InvalidArgument),
    (u64::MAX, SyscallError::InvalidArgument),
];

fn main() {
    let answers: Vec<_> = LENGTHS
        .iter()
        .map(|&(size, want)| (size, want, syscall::shm_create(size as usize)))
        .collect();
    for (size, want, answer) in answers {
        match answer {
            Err(e) if e == want => {}
            Err(e) => panic!("shm_create(len={size:#x}) was refused as {e:?}, not {want:?}"),
            Ok(h) => panic!("shm_create(len={size:#x}) was served as handle {}", h.0),
        }
    }

    // The control: the refusal is the length and not the call.
    let served = syscall::shm_create(4096).expect("a one-page region is served");
    syscall::close(served);

    println!("an shm length past memory or past u64 is refused by name, and one page is served");
}
