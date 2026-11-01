//! System information and control.

use toyos_abi::syscall;

/// The ABI's, re-exported rather than restated: two spellings of one layout are
/// a reader that walks off by a field the day one of them moves.
pub use toyos_abi::syscall::{SYSINFO_ENTRY_SIZE, SYSINFO_HEADER_SIZE};

/// Seconds since the Unix epoch, which is UTC, or `None` on a machine whose
/// clock never answered.
///
/// Cheap: the kernel serves it from an anchor it took at boot plus the
/// monotonic clock, so calling it in a loop costs a syscall each and no device
/// access at all.
pub fn clock_epoch() -> Option<u64> {
    syscall::clock_epoch()
}

/// The machine's header: total and used memory, the CPU count, the live-thread
/// count, the uptime, and the busy and available CPU nanoseconds a percentage
/// is derived from.
///
/// **Ambient, and the buffer is what says so.** The same syscall answers the
/// process roster after this header, and writing one entry of that costs
/// [`Rights::ROSTER`] on a `SysCap` — so this takes an array of exactly the
/// header's length and a caller that only wants machine facts cannot express
/// the privileged question by accident. [`crate::syscap::SysCap::roster`] is
/// the other half.
///
/// [`Rights::ROSTER`]: toyos_abi::handle::Rights::ROSTER
pub fn sysinfo(buf: &mut [u8; SYSINFO_HEADER_SIZE]) -> usize {
    syscall::sysinfo(toyos_abi::handle::HANDLE_INVALID, buf)
}

/// The machine's physical memory in bytes, as the kernel reports it.
pub fn memory_total() -> u64 {
    let mut buf = [0u8; SYSINFO_HEADER_SIZE];
    let n = sysinfo(&mut buf);
    assert!(n >= SYSINFO_HEADER_SIZE, "sysinfo returned {n} bytes");
    syscall::SysinfoHeader::decode(&buf).memory_total
}

// Powering the machine off used to be a free function here, over a syscall that
// took no argument. It is [`crate::syscap::SysCap::shutdown`] now: it is an
// authority over the whole machine, and every one of those is a bit on a
// capability rather than a call anything can make.

