//! What an AArch64 thread finds in its registers after the kernel has had the
//! CPU: its own FP/SIMD state across the switches that took the CPU from it,
//! and nothing of the kernel's at its first instruction.

/// `fp_isolation`: this thread pins a distinctive FP/SIMD state — v0–v31,
/// `FPCR` and `FPSR` — and holds it while a sibling that loads another state
/// takes the CPU from it; the state it reads back must be the one it pinned.
/// The kernel saves and restores that state only where a thread stops
/// running, so on one CPU every switch is a chance to hand this thread the
/// sibling's.
pub mod fp_isolation {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
    use std::sync::Arc;

    /// The saved state: v0–v31, then `FPCR` and `FPSR`.
    #[repr(C, align(16))]
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    struct FpState {
        v: [u128; 32],
        fpcr: u64,
        fpsr: u64,
    }

    const fn state(tag: u128, fpcr: u64, fpsr: u64) -> FpState {
        let mut v = [0; 32];
        let mut i = 0;
        while i < 32 {
            v[i] = tag | (i as u128 + 1);
            i += 1;
        }
        FpState { v, fpcr, fpsr }
    }

    /// Default-NaN, flush-to-zero, round toward zero; every cumulative flag
    /// and `QC` raised.
    static PINNED: FpState = state(0xF9A3 << 112, 1 << 25 | 1 << 24 | 0b11 << 22, 1 << 27 | 0x1F);
    /// Round toward plus infinity, no flag.
    static NOISE: FpState = state(0x5A5A << 112, 0b01 << 22, 0);
    const EMPTY: FpState = state(0, 0, 0);

    /// Times the sibling must be seen to have run while this thread held its state.
    const SWITCHES: u64 = 3;

    /// Loads the state at `[x9]` into the FP/SIMD registers.
    macro_rules! load_state {
        () => {
            concat!(
                "ldp q0, q1, [x9, #0]\n", "ldp q2, q3, [x9, #32]\n", "ldp q4, q5, [x9, #64]\n",
                "ldp q6, q7, [x9, #96]\n", "ldp q8, q9, [x9, #128]\n", "ldp q10, q11, [x9, #160]\n",
                "ldp q12, q13, [x9, #192]\n", "ldp q14, q15, [x9, #224]\n", "ldp q16, q17, [x9, #256]\n",
                "ldp q18, q19, [x9, #288]\n", "ldp q20, q21, [x9, #320]\n", "ldp q22, q23, [x9, #352]\n",
                "ldp q24, q25, [x9, #384]\n", "ldp q26, q27, [x9, #416]\n", "ldp q28, q29, [x9, #448]\n",
                "ldp q30, q31, [x9, #480]\n",
                "ldr x12, [x9, #512]\n", "msr fpcr, x12\n",
                "ldr x12, [x9, #520]\n", "msr fpsr, x12\n",
            )
        };
    }

    /// Stores the FP/SIMD registers at `[$reg]`.
    macro_rules! store_state {
        ($reg:literal) => {
            concat!(
                "stp q0, q1, [", $reg, ", #0]\n", "stp q2, q3, [", $reg, ", #32]\n",
                "stp q4, q5, [", $reg, ", #64]\n", "stp q6, q7, [", $reg, ", #96]\n",
                "stp q8, q9, [", $reg, ", #128]\n", "stp q10, q11, [", $reg, ", #160]\n",
                "stp q12, q13, [", $reg, ", #192]\n", "stp q14, q15, [", $reg, ", #224]\n",
                "stp q16, q17, [", $reg, ", #256]\n", "stp q18, q19, [", $reg, ", #288]\n",
                "stp q20, q21, [", $reg, ", #320]\n", "stp q22, q23, [", $reg, ", #352]\n",
                "stp q24, q25, [", $reg, ", #384]\n", "stp q26, q27, [", $reg, ", #416]\n",
                "stp q28, q29, [", $reg, ", #448]\n", "stp q30, q31, [", $reg, ", #480]\n",
                "mrs x12, fpcr\n", "str x12, [", $reg, ", #512]\n",
                "mrs x12, fpsr\n", "str x12, [", $reg, ", #520]\n",
            )
        };
    }

    pub fn main(_args: Vec<String>) {
        let ran = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let sibling = {
            let (ran, stop) = (Arc::clone(&ran), Arc::clone(&stop));
            std::thread::spawn(move || noise_until(&ran, &stop))
        };
        let (mut before, mut after) = (EMPTY, EMPTY);
        pin_and_watch(&ran, &mut before, &mut after);
        stop.store(true, Relaxed);
        sibling.join().expect("the noise thread died");

        assert_eq!(before.v, PINNED.v, "the pinned registers did not read back as pinned");
        assert_ne!(before.fpcr, NOISE.fpcr, "FPCR read back as the noise's, so the arm cannot tell them apart");
        let lost: Vec<usize> = (0..32).filter(|&i| after.v[i] != before.v[i]).collect();
        assert!(
            lost.is_empty() && after.fpcr == before.fpcr && after.fpsr == before.fpsr,
            "the FP/SIMD state did not survive {SWITCHES} switches to a thread that loads another: \
             v{lost:?} changed, FPCR {:#x} became {:#x}, FPSR {:#x} became {:#x}",
            before.fpcr,
            after.fpcr,
            before.fpsr,
            after.fpsr,
        );
        println!("fp_isolation: v0-v31, FPCR and FPSR survived {SWITCHES} switches to a thread that loads another state");
    }

    /// Pin [`PINNED`], read it back into `before`, hold it until `ran` has
    /// been seen to move [`SWITCHES`] times, and read it into `after`. On one
    /// CPU `ran` moves only while this thread is off it. Nothing between the
    /// pin and the second read touches an FP/SIMD register.
    fn pin_and_watch(ran: &AtomicU64, before: &mut FpState, after: &mut FpState) {
        // SAFETY: reads `PINNED` and `ran`, writes `before` and `after`, each
        // named by its register and live across the block; every FP/SIMD
        // register it writes is declared clobbered, and `FPCR`/`FPSR` are put
        // back as found.
        unsafe {
            core::arch::asm!(
                "mrs x16, fpcr",
                "mrs x17, fpsr",
                load_state!(),
                store_state!("x10"),
                "ldr x14, [x13]",
                "mov x15, #{switches}",
                "2:",
                "ldr x12, [x13]",
                "cmp x12, x14",
                "b.eq 2b",
                "mov x14, x12",
                "subs x15, x15, #1",
                "b.ne 2b",
                store_state!("x11"),
                "msr fpcr, x16",
                "msr fpsr, x17",
                switches = const SWITCHES,
                in("x9") &raw const PINNED,
                in("x10") core::ptr::from_mut(before),
                in("x11") core::ptr::from_mut(after),
                in("x13") core::ptr::from_ref(ran),
                out("x12") _, out("x14") _, out("x15") _, out("x16") _, out("x17") _,
                out("v0") _, out("v1") _, out("v2") _, out("v3") _, out("v4") _, out("v5") _,
                out("v6") _, out("v7") _, out("v8") _, out("v9") _, out("v10") _, out("v11") _,
                out("v12") _, out("v13") _, out("v14") _, out("v15") _, out("v16") _, out("v17") _,
                out("v18") _, out("v19") _, out("v20") _, out("v21") _, out("v22") _, out("v23") _,
                out("v24") _, out("v25") _, out("v26") _, out("v27") _, out("v28") _, out("v29") _,
                out("v30") _, out("v31") _,
                options(nostack),
            );
        }
    }

    /// Load [`NOISE`] and count one round in `ran`, over and over, until `stop`.
    fn noise_until(ran: &AtomicU64, stop: &AtomicBool) {
        // SAFETY: reads `NOISE` and `stop`, writes `ran`; every FP/SIMD
        // register it writes is declared clobbered, and `FPCR`/`FPSR` are put
        // back as found.
        unsafe {
            core::arch::asm!(
                "mrs x16, fpcr",
                "mrs x17, fpsr",
                "2:",
                load_state!(),
                "ldr x12, [x13]",
                "add x12, x12, #1",
                "str x12, [x13]",
                "ldrb w12, [x14]",
                "cbz w12, 2b",
                "msr fpcr, x16",
                "msr fpsr, x17",
                in("x9") &raw const NOISE,
                in("x13") core::ptr::from_ref(ran),
                in("x14") core::ptr::from_ref(stop),
                out("x12") _, out("x16") _, out("x17") _,
                out("v0") _, out("v1") _, out("v2") _, out("v3") _, out("v4") _, out("v5") _,
                out("v6") _, out("v7") _, out("v8") _, out("v9") _, out("v10") _, out("v11") _,
                out("v12") _, out("v13") _, out("v14") _, out("v15") _, out("v16") _, out("v17") _,
                out("v18") _, out("v19") _, out("v20") _, out("v21") _, out("v22") _, out("v23") _,
                out("v24") _, out("v25") _, out("v26") _, out("v27") _, out("v28") _, out("v29") _,
                out("v30") _, out("v31") _,
                options(nostack),
            );
        }
    }
}

/// `first_entry`: a raw thread whose first instruction stores x1–x30 and
/// exits. The kernel hands a new thread its argument in x0 and its stack in
/// `SP_EL0`, and every other general register must reach that instruction
/// zero: whatever else is there is the kernel's.
pub mod first_entry {
    use toyos_abi::syscall::{thread_join, thread_spawn, SYS_THREAD_EXIT};

    /// x1 to x30, as the probe found them.
    #[repr(C, align(16))]
    struct Registers([u64; 30]);

    pub fn main(_args: Vec<String>) {
        let mut found = Registers([u64::MAX; 30]);
        let mut stack = vec![0u128; 1024];
        let base = stack.as_mut_ptr() as u64;
        let top = base + (stack.len() * 16) as u64;
        // SAFETY: `probe` touches only the `Registers` its argument names,
        // which outlives the join below, and never its stack.
        let tid = unsafe { thread_spawn(probe as *const () as u64, top, (&raw mut found) as u64, base) };
        assert!(tid < 1_000_000, "thread_spawn refused: {tid:#x}");
        assert_eq!(thread_join(tid), 0, "thread_join failed");
        drop(stack);
        let held: Vec<String> = (0..30)
            .filter(|&i| found.0[i] != 0)
            .map(|i| format!("x{}={:#x}", i + 1, found.0[i]))
            .collect();
        assert!(held.is_empty(), "a new thread's first instruction found {}", held.join(" "));
        println!("first_entry: x1-x30 were zero at a new thread's first instruction");
    }

    /// Store x1–x30 at `[x0]` before any of them is written, and exit the thread.
    #[unsafe(naked)]
    extern "C" fn probe() {
        core::arch::naked_asm!(
            "stp x1, x2, [x0, #0]",
            "stp x3, x4, [x0, #16]",
            "stp x5, x6, [x0, #32]",
            "stp x7, x8, [x0, #48]",
            "stp x9, x10, [x0, #64]",
            "stp x11, x12, [x0, #80]",
            "stp x13, x14, [x0, #96]",
            "stp x15, x16, [x0, #112]",
            "stp x17, x18, [x0, #128]",
            "stp x19, x20, [x0, #144]",
            "stp x21, x22, [x0, #160]",
            "stp x23, x24, [x0, #176]",
            "stp x25, x26, [x0, #192]",
            "stp x27, x28, [x0, #208]",
            "stp x29, x30, [x0, #224]",
            "mov x0, #{exit}",
            "mov x1, xzr",
            "svc #0",
            "brk #0",
            exit = const SYS_THREAD_EXIT,
        );
    }
}
