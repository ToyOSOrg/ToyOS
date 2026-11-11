//! MEASUREMENT ONLY, never lands: the probe kernel's segment timings around a
//! benchmark's timed region. `begin` clears and arms, `on` and `off` bracket
//! the timed region, `end` prints every CPU's `id:count:ticks`.

use std::sync::atomic::{AtomicBool, Ordering};

static ARMED: AtomicBool = AtomicBool::new(false);

#[cfg(target_os = "toyos")]
fn call(action: u64, a2: u64, a3: u64) -> u64 {
    let ret: u64;
    // SAFETY: the probe kernel's control call; it writes only the buffer named.
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rdi") 0xffff_5c07u64,
            in("rsi") action,
            in("rdx") a2,
            in("r8") a3,
            in("r9") 0u64,
            lateout("rax") ret,
            out("rcx") _,
            out("r11") _,
            options(nostack),
        );
    }
    ret
}

#[cfg(not(target_os = "toyos"))]
fn call(_action: u64, _a2: u64, _a3: u64) -> u64 {
    0
}

pub fn begin() {
    call(0, 0, 0);
    ARMED.store(true, Ordering::Relaxed);
}

#[inline(always)]
pub fn on() {
    if ARMED.load(Ordering::Relaxed) {
        call(3, 0, 0);
    }
}

#[inline(always)]
pub fn off() {
    if ARMED.load(Ordering::Relaxed) {
        call(4, 0, 0);
    }
}

pub fn end(row: &str) {
    ARMED.store(false, Ordering::Relaxed);
    if !cfg!(target_os = "toyos") {
        return;
    }
    let mut raw = vec![0u8; 16 * 4096];
    let n = call(1, raw.as_mut_ptr() as u64, raw.len() as u64);
    if n > 4096 {
        return println!("mprobe row={row} unavailable={n:#x}");
    }
    let mut line = String::new();
    let mut on_line = 0;
    let mut last_cpu = u16::MAX;
    for rec in raw[..n as usize * 16].chunks_exact(16) {
        let cpu = u16::from_le_bytes([rec[0], rec[1]]);
        let id = u16::from_le_bytes([rec[2], rec[3]]);
        let count = u32::from_le_bytes(rec[4..8].try_into().unwrap());
        let sum = u64::from_le_bytes(rec[8..16].try_into().unwrap());
        if cpu != last_cpu || on_line == 10 {
            if !line.is_empty() {
                println!("mprobe row={row} cpu={last_cpu}{line}");
            }
            line.clear();
            on_line = 0;
            last_cpu = cpu;
        }
        line.push_str(&format!(" {id}:{count}:{sum}"));
        on_line += 1;
    }
    if !line.is_empty() {
        println!("mprobe row={row} cpu={last_cpu}{line}");
    }
    println!("mprobe row={row} end records={n}");
}

/// The counter's rate against the clock, and what a pair of probes costs.
pub fn calibrate() {
    #[cfg(all(target_os = "toyos", target_arch = "x86_64"))]
    {
        // SAFETY: unprivileged.
        let rdtsc = || unsafe { core::arch::x86_64::_rdtsc() };
        let t0 = std::time::Instant::now();
        let c0 = rdtsc();
        while t0.elapsed().as_millis() < 50 {}
        let khz = (rdtsc() - c0) as u128 * 1_000_000 / t0.elapsed().as_nanos();
        println!("mprobe tsc_khz={khz}");
        begin();
        on();
        for _ in 0..100_000 {
            call(2, 0, 0);
        }
        off();
        end("calibrate.probe_pair");
        begin();
        on();
        for _ in 0..100_000 {
            crate::sys::null_syscall();
        }
        off();
        end("calibrate.null_syscall");
    }
}
