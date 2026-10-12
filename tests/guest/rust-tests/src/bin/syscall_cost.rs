//! What a transition out of Ring 3 costs, in cycles.
//!
//! The syscall entry, which is the busiest of the five places `arch::entry`'s
//! bracket sits. The exception entry pays the same two instructions, and there
//! is no cheap way to measure it here: the only demand-paged memory a userland
//! program can reach is its own file-backed pages — `sys_mmap` allocates and
//! maps its whole region up front — so a workload of N faults costs N × 2 MiB
//! of test image.
//!
//! **No threshold, and the host reads the numbers instead.** A bound measured
//! under TCG is meaningless and one measured on metal drifts. The number
//! is for a same-session A/B against another build of this tree; what the host
//! refuses is a counter that did not move (`check_syscall_cost`).
//!
//! Reported as the minimum over repetitions rather than the mean: the minimum
//! is the run with the least interference, and on a host running eleven other
//! guests interference is all the mean measures.
//!
//! Scout, never landed: each repetition is also read in core cycles, as TSC
//! cycles times the busiest CPU's `APERF`/`MPERF` across it, and the entry's
//! two suspects are timed alone in Ring 3 beside it.

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::syscap::SysCap;
use toyos_abi::clock::nanos_since_boot as clock_nanos;
use toyos_abi::counters::{Counter, RawRecord, Record};

const REPS: usize = 9;
const SYSCALLS_PER_REP: u64 = 20_000;

fn rdtsc() -> u64 {
    let lo: u32;
    let hi: u32;
    unsafe {
        core::arch::asm!(
            "lfence",
            "rdtsc",
            out("eax") lo,
            out("edx") hi,
            options(nomem, nostack),
        );
    }
    ((hi as u64) << 32) | lo as u64
}

fn records(cap: &SysCap) -> Vec<Record> {
    let mut raw = vec![RawRecord::EMPTY; toyos_abi::syscall::cpu_count() as usize];
    let n = cap.counters(&mut raw).expect("the estate's capability reads the counters");
    raw[..n].iter().map(|r| Record::decode(r).expect("a record that decodes")).collect()
}

/// One repetition: TSC cycles per call, core cycles per call off the CPU whose
/// `MPERF` moved most, and the share of the span that CPU was in C0.
struct Rep {
    tsc: u64,
    core: f64,
    busy: f64,
}

fn rep(cap: &SysCap, calls: u64, mut body: impl FnMut()) -> Rep {
    let before = records(cap);
    let start = rdtsc();
    for _ in 0..calls {
        body();
    }
    let end = rdtsc();
    let after = records(cap);
    let delta = |r: &Record, b: &Record, c: Counter| r.get(c).unwrap_or(0).wrapping_sub(b.get(c).unwrap_or(0));
    let (aperf, mperf, stamp) = after
        .iter()
        .zip(&before)
        .map(|(a, b)| (delta(a, b, Counter::Aperf), delta(a, b, Counter::Mperf), delta(a, b, Counter::Stamp)))
        .max_by_key(|&(_, m, _)| m)
        .expect("a machine has a cpu");
    let tsc = (end - start) / calls;
    let ratio = if mperf == 0 { f64::NAN } else { aperf as f64 / mperf as f64 };
    Rep { tsc, core: tsc as f64 * ratio, busy: mperf as f64 / stamp.max(1) as f64 }
}

fn report(cap: &SysCap, what: &str, calls: u64, mut body: impl FnMut()) -> u64 {
    // A first pass nobody reads: the loop's own pages have to be faulted in
    // before the number is about the body rather than about the text.
    rep(cap, calls, &mut body);
    let mut reps: Vec<Rep> = (0..REPS).map(|_| rep(cap, calls, &mut body)).collect();
    reps.sort_by_key(|r| r.tsc);
    let (min, mid) = (&reps[0], &reps[REPS / 2]);
    println!(
        "scout-sys: {what}: min tsc={} core={:.0} busy={:.3} | median tsc={} core={:.0} busy={:.3} | over {REPS}x{calls}",
        min.tsc, min.core, min.busy, mid.tsc, mid.core, mid.busy
    );
    min.tsc
}

#[repr(C, align(64))]
struct Area([u8; 512]);

macro_rules! xmm_check {
    ($($r:literal)*) => {
        concat!(
            "mov {t}, {p}\n",
            $("movq xmm", $r, ", {t}\n", "add {t}, 1\n",)*
            "mov rdi, {sleep}\n",
            "mov rsi, 50000\n",
            "syscall\n",
            "mov {t}, {p}\n",
            $("movq {u}, xmm", $r, "\n", "cmp {u}, {t}\n", "jne 2f\n", "add {t}, 1\n",)*
            "xor {bad:e}, {bad:e}\n",
            "jmp 3f\n",
            "2:\n",
            "mov {bad:e}, 1\n",
            "3:\n",
        )
    };
}

/// Every XMM register a thread loaded is still its own after it slept,
/// with twice as many threads as CPUs sleeping and loading their own: the
/// `scout-nofp` twin saves the user state at the switch instead of the entry.
fn fp_survives_switches() {
    let threads = 2 * toyos_abi::syscall::cpu_count() as u64;
    let bad: u32 = (0..threads)
        .map(|i| {
            std::thread::spawn(move || {
                let mut bad = 0u32;
                for round in 0..200u64 {
                    let p = (i << 48) | (round << 8);
                    let mut b: u32;
                    // SAFETY: loads and compares XMM0–15, which it declares
                    // clobbered, around one `SYS_NANOSLEEP`.
                    unsafe {
                        core::arch::asm!(
                            xmm_check!("0" "1" "2" "3" "4" "5" "6" "7" "8" "9" "10" "11" "12" "13" "14" "15"),
                            p = in(reg) p,
                            t = out(reg) _,
                            u = out(reg) _,
                            bad = out(reg) b,
                            sleep = const toyos_abi::syscall::SYS_NANOSLEEP,
                            out("rdi") _, out("rsi") _, out("rax") _, out("rcx") _, out("r11") _,
                            out("xmm0") _, out("xmm1") _, out("xmm2") _, out("xmm3") _,
                            out("xmm4") _, out("xmm5") _, out("xmm6") _, out("xmm7") _,
                            out("xmm8") _, out("xmm9") _, out("xmm10") _, out("xmm11") _,
                            out("xmm12") _, out("xmm13") _, out("xmm14") _, out("xmm15") _,
                        );
                    }
                    bad += b;
                }
                bad
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|h| h.join().unwrap())
        .sum();
    println!("scout-sys: fp check: {threads} threads x 200 sleeps, {bad} with an XMM register not their own");
    assert_eq!(bad, 0, "a thread's XMM state did not survive a switch");
}

fn main() {
    let cap: SysCap = Endowments::get().take(SYSCAP_LABEL).expect("test-runner endows a capability");
    fp_survives_switches();

    let syscall = report(&cap, "getpid", SYSCALLS_PER_REP, || {
        std::hint::black_box(toyos_abi::syscall::getpid());
    });

    let mut area = Area([0; 512]);
    let at = area.0.as_mut_ptr();
    let mut word = 0u32;
    let w = &raw mut word;
    // SAFETY: each body touches only `area`, 64-byte aligned and 512 bytes,
    // and `word`; `fxrstor64` loads the image `fxsave64` wrote just before.
    report(&cap, "ring3 fxsave64+fxrstor64", SYSCALLS_PER_REP, || unsafe {
        core::arch::asm!("fxsave64 [{a}]", "fxrstor64 [{a}]", a = in(reg) at, options(nostack));
    });
    report(&cap, "ring3 lock add", SYSCALLS_PER_REP, || unsafe {
        core::arch::asm!("lock add dword ptr [{w}], 1", w = in(reg) w, options(nostack));
    });
    report(&cap, "ring3 add", SYSCALLS_PER_REP, || unsafe {
        core::arch::asm!("add dword ptr [{w}], 1", w = in(reg) w, options(nostack));
    });
    // The entry's order: the save's 512 bytes of stores, then the locked add
    // that waits for them to drain.
    report(&cap, "ring3 fxsave64;lock add;fxrstor64", SYSCALLS_PER_REP, || unsafe {
        core::arch::asm!(
            "fxsave64 [{a}]", "lock add dword ptr [{w}], 1", "fxrstor64 [{a}]",
            a = in(reg) at, w = in(reg) w, options(nostack),
        );
    });
    report(&cap, "ring3 fxsave64;add;fxrstor64", SYSCALLS_PER_REP, || unsafe {
        core::arch::asm!(
            "fxsave64 [{a}]", "add dword ptr [{w}], 1", "fxrstor64 [{a}]",
            a = in(reg) at, w = in(reg) w, options(nostack),
        );
    });

    // The clock alongside the cycles, because a TSC that does not tick at a
    // fixed rate makes the cycle counts incomparable and this is the only
    // thing in the run that would show it.
    let t0 = clock_nanos();
    let c0 = rdtsc();
    while clock_nanos() - t0 < 20_000_000 {}
    let hz = (rdtsc() - c0) * 1_000_000_000 / (clock_nanos() - t0);

    println!("syscall_cost: {syscall} cycles/syscall over {REPS}x{SYSCALLS_PER_REP}");
    println!("syscall_cost: tsc {} MHz", hz / 1_000_000);
}
