//! A thread pinned to each CPU in turn stays there: across 1000 sleeps at
//! each, beside a spinner pinned to the same CPU so that every wake queues
//! behind running work and an idle CPU is free to take it, the CPU's own
//! x2APIC ID reads one value per pin and a different one for each CPU, and
//! the kernel's index reads the CPU pinned to. The pin moves the thread while
//! it runs, and each wake places it, so both routes are held.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use core_affinity::CoreId;
use toyos_abi::syscall;

#[path = "../arch/cpu.rs"]
mod cpu;

const SLEEPS: usize = 1000;

fn pin(cpu: u32) {
    assert!(core_affinity::set_for_current(CoreId { id: cpu as usize }), "pin to CPU {cpu}");
}

fn main() {
    let cpus = syscall::cpu_count();
    let cores = core_affinity::get_core_ids().expect("the CPUs this thread may run on");
    assert_eq!(cores.len(), cpus as usize, "an unpinned thread may run on every CPU, and core_affinity names {cores:?}");
    let mut ids = Vec::new();
    for on in 0..cpus {
        let stop = Arc::new(AtomicBool::new(false));
        let spinner = {
            let stop = stop.clone();
            thread::spawn(move || {
                pin(on);
                while !stop.load(Ordering::Relaxed) {
                    std::hint::spin_loop();
                }
            })
        };
        pin(on);
        let id = cpu::x2apic_id();
        for sleep in 0..SLEEPS {
            thread::sleep(Duration::from_micros(10));
            let now = cpu::x2apic_id();
            assert_eq!(now, id, "pinned to CPU {on}, x2APIC {id}, and after sleep {sleep} on x2APIC {now}");
            assert_eq!(syscall::current_cpu(), on, "pinned to CPU {on}, and the kernel says it runs elsewhere");
        }
        stop.store(true, Ordering::Relaxed);
        spinner.join().expect("the spinner panicked");
        ids.push(id);
    }
    let distinct: BTreeSet<u32> = ids.iter().copied().collect();
    assert_eq!(distinct.len(), ids.len(), "{cpus} pins read x2APIC IDs {ids:?}: two pins ran on one CPU");
    let named: Vec<String> = ids.iter().enumerate().map(|(cpu, id)| format!("cpu{cpu}=x2apic{id}")).collect();
    println!("affinity_pins: {cpus} CPUs held {SLEEPS} sleeps each: {}", named.join(" "));
}
