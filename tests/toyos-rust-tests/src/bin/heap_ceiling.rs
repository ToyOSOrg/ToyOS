//! The kernel heap's ceiling.
//!
//! `KernelPageSource` hands dlmalloc one 2 MiB page and can hand it no more,
//! so `mm::MAX_HEAP_ALLOC` is the largest single allocation the kernel heap
//! can serve. Asking for more is a kernel bug and halts the machine, which
//! `heap_over_ceiling_halts` asserts on a boot of its own.

// `SYS_DEBUG` actions a `test-actuators` kernel provides. The first two take
// one kernel heap allocation each and release it again — at
// `mm::MAX_HEAP_ALLOC`, and at `MAX_HEAP_ALLOC` with 4096-byte alignment; the
// last lowers `SYS_SYSINFO`'s thread bound to the machine's live threads.
use toyos_abi::syscall::debug_action::{
    HEAP_AT_CEILING, HEAP_AT_CEILING_PAGE_ALIGNED, LOWER_SYSINFO_BOUND,
};

/// `SyscallError::ResourceExhausted`, as `SyscallError::to_u64` encodes it.
const RESOURCE_EXHAUSTED: u64 = u64::MAX - 7;

fn main() {
    at_ceiling_is_servable();
    aligned_at_ceiling_is_refused_not_fatal();
    sysinfo_refuses_rather_than_allocating_past_the_ceiling();
    println!("all heap ceiling tests passed");
}

/// A syscall whose allocation is derived from something userland grows.
///
/// `SYS_SYSINFO` collects one 24-byte entry per live thread so it can sort
/// them, and the caller's buffer bounds what is *written*, not what is built.
/// Nothing caps the thread count, so ~87,000 threads made an ordinary syscall
/// ask the heap for more than `MAX_HEAP_ALLOC` and trip the assert three
/// functions above — from any process, with no privilege.
///
/// [`LOWER_SYSINFO_BOUND`] puts the machine's live threads in
/// `MAX_SYSINFO_THREADS`'s place, because 65,536 threads is 8 GiB of kernel
/// stacks and no guest can make them. The count, the comparison and the
/// refusal are the shipped ones.
///
/// **Armed here rather than compiled in, and the arming is itself an
/// assertion**: the bound is the shipped 65,536 until this call, so a kernel
/// that answered it and did nothing would fail at the loop below rather than
/// pass. As a `#[cfg]` the 16 rode into every kernel the suite booted, and
/// `SYS_SYSINFO` answered against it in every guest.
fn sysinfo_refuses_rather_than_allocating_past_the_ceiling() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let live = sysinfo_live().expect("sysinfo already refuses with no threads of ours");
    let rc = toyos_abi::syscall::debug(LOWER_SYSINFO_BOUND);
    assert_eq!(rc, 0, "SYS_DEBUG {LOWER_SYSINFO_BOUND} did not lower the bound (rc={rc:#x})");

    let stop = Arc::new(AtomicBool::new(false));
    let mut parked = Vec::new();
    let mut refused_at = None;
    // Past the bound with room, and far short of anything that would matter
    // to a guest with one CPU.
    for i in 0..64 {
        let flag = Arc::clone(&stop);
        parked.push(std::thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }));
        if sysinfo_live().is_none() {
            refused_at = Some(i + 1);
            break;
        }
    }

    let at = refused_at.unwrap_or_else(|| {
        stop.store(true, Ordering::Relaxed);
        panic!("64 extra threads and sysinfo never refused — its collection is unbounded")
    });

    stop.store(true, Ordering::Relaxed);
    for t in parked {
        t.join().expect("join a parked thread");
    }
    // A bound, not a one-way door: with the threads gone it answers again.
    assert!(sysinfo_live().is_some(), "sysinfo stayed refused after the threads exited");
    println!(
        "  PASS: sysinfo refused past its bound at {at} extra threads over {live} live before arming, and recovered"
    );
}

/// The live threads `SYS_SYSINFO`'s header counts, or `None` when it refused.
/// The ABI wrapper reports an error as `0`, and the header is the smallest
/// buffer it accepts.
fn sysinfo_live() -> Option<u32> {
    let mut buf = [0u8; toyos::system::SYSINFO_HEADER_SIZE];
    (toyos::system::sysinfo(&mut buf) == buf.len())
        .then(|| toyos_abi::syscall::SysinfoHeader::decode(&buf).entries)
}

/// The documented ceiling is a size the heap actually serves.
///
/// This process makes the call itself, so a kernel that asserts here, or an
/// allocation that comes back null, kills this test. `MAX_HEAP_ALLOC` is
/// `PAGE_2M - 4096` and the 4 KiB is headroom for dlmalloc's own chunk and
/// segment bookkeeping — arithmetic that was reasoned about and never run.
///
/// It is also the negative side of `heap_over_ceiling_halts`: an assert that
/// simply refused every large allocation would satisfy that one and fail this.
fn at_ceiling_is_servable() {
    let rc = toyos_abi::syscall::debug(HEAP_AT_CEILING);
    assert_eq!(
        rc, 0,
        "an allocation at MAX_HEAP_ALLOC was refused (rc={rc:#x}) — the documented \
         ceiling is above the real one"
    );
    println!("  PASS: MAX_HEAP_ALLOC is servable");
}

/// The same size, page-aligned, is more than the page source can back — and
/// that is an error return, not a dead machine.
///
/// `memalign` pads by the alignment before it asks for backing, so this request
/// satisfies `MAX_HEAP_ALLOC` and still reaches the page source
/// asking for 2,162,688 bytes.
fn aligned_at_ceiling_is_refused_not_fatal() {
    let rc = toyos_abi::syscall::debug(HEAP_AT_CEILING_PAGE_ALIGNED);
    assert_eq!(
        rc, RESOURCE_EXHAUSTED,
        "a page-aligned allocation at MAX_HEAP_ALLOC returned {rc:#x}; expected the \
         page source to refuse it"
    );
    println!("  PASS: an allocation the page source cannot back is refused, not fatal");
}
