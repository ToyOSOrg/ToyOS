//! A request asked from a CPU other than the boot processor.

/// Run `ask` on a thread held to the machine's last CPU, which is the boot
/// processor only on a machine of one: what `ask` answered. The hold moves
/// the thread before `ask` starts and keeps it there, so every CPU `ask`
/// reads, and every one the kernel names for its requests, is that one.
pub fn run<R: Send>(ask: impl FnOnce() -> R + Send) -> R {
    let last = toyos_abi::syscall::cpu_count() as usize - 1;
    std::thread::scope(|threads| {
        threads
            .spawn(|| {
                assert!(core_affinity::set_for_current(core_affinity::CoreId { id: last }), "a thread held to CPU {last}");
                ask()
            })
            .join()
            .expect("the thread held off the boot processor")
    })
}
