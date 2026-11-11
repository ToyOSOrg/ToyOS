//! Measurement scout, never landed: the scout kernel's segment timings
//! around a benchmark's timed loop, and a clock read's parts.

#[cfg(target_os = "toyos")]
mod imp {
    fn call(action: u64, a2: u64, a3: u64) -> u64 {
        let ret: u64;
        // SAFETY: the scout kernel's control call; it writes only the buffer named.
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

    pub fn begin() {
        call(0, 0, 0);
    }

    pub fn end(row: &str) {
        let mut raw = vec![0u8; 16 * 4096];
        let n = call(1, raw.as_mut_ptr() as u64, raw.len() as u64);
        if n > 4096 {
            return println!("scoutseg row={row} unavailable={n:#x}");
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
                    println!("scoutseg row={row} cpu={last_cpu}{line}");
                }
                line.clear();
                on_line = 0;
                last_cpu = cpu;
            }
            line.push_str(&format!(" {id}:{count}:{sum}"));
            on_line += 1;
        }
        if !line.is_empty() {
            println!("scoutseg row={row} cpu={last_cpu}{line}");
        }
        println!("scoutseg row={row} end records={n}");
    }

    fn rdtsc() -> u64 {
        // SAFETY: unprivileged.
        unsafe { core::arch::x86_64::_rdtsc() }
    }

    /// What a probe costs, what the counter's rate is, and a clock read's
    /// candidate shapes, each as ticks per call over a million calls.
    pub fn calibrate() {
        let t0 = toyos_abi::clock::nanos_since_boot();
        let c0 = rdtsc();
        while toyos_abi::clock::nanos_since_boot() - t0 < 50_000_000 {}
        let khz = (rdtsc() - c0) * 1_000_000 / (toyos_abi::clock::nanos_since_boot() - t0);
        println!("scoutseg tsc_khz={khz}");
        begin();
        for _ in 0..100_000 {
            call(2, 0, 0);
        }
        end("calibrate.probe_pair");

        const CALLS: u64 = 1_000_000;
        let per = |f: &dyn Fn() -> u64| {
            let mut best = u64::MAX;
            for _ in 0..7 {
                let c = rdtsc();
                let mut acc = 0u64;
                for _ in 0..CALLS {
                    acc = acc.wrapping_add(std::hint::black_box(f()));
                }
                std::hint::black_box(acc);
                best = best.min((rdtsc() - c) * 1000 / CALLS);
            }
            best
        };
        let clock = toyos_abi::clock::page();
        let line = move |counter: u64| clock.nanos_since_boot(counter);
        // SAFETY: each reads the counter; `lfence` and `rdtscp` are unprivileged.
        let shapes: [(&str, &dyn Fn() -> u64); 7] = [
            ("instant_now", &|| {
                let t = std::time::Instant::now();
                std::hint::black_box(t);
                0
            }),
            ("abi_nanos_since_boot", &|| toyos_abi::clock::nanos_since_boot()),
            ("line_only", &|| line(std::hint::black_box(12_345_678_901))),
            ("rdtsc", &|| line(rdtsc())),
            ("lfence_rdtsc", &|| unsafe {
                core::arch::x86_64::_mm_lfence();
                line(core::arch::x86_64::_rdtsc())
            }),
            ("lfence_rdtsc_lfence", &|| unsafe {
                core::arch::x86_64::_mm_lfence();
                let now = core::arch::x86_64::_rdtsc();
                core::arch::x86_64::_mm_lfence();
                line(now)
            }),
            ("rdtscp", &|| unsafe {
                let mut aux = 0u32;
                line(core::arch::x86_64::__rdtscp(&mut aux))
            }),
        ];
        for (name, f) in shapes {
            println!("scoutseg clockread shape={name} milliticks_per_call={}", per(f));
        }
    }
}

#[cfg(not(target_os = "toyos"))]
mod imp {
    pub fn begin() {}
    pub fn end(_row: &str) {}
    pub fn calibrate() {}
}

pub use imp::{begin, calibrate, end};
