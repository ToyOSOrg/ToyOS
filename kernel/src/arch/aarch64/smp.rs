//! Other CPUs: every GIC CPU interface the MADT enables, started one at a time
//! with PSCI's `CPU_ON` (Arm DEN0022) at [`super::boot::ap_start`], and
//! committed to the roster once it has echoed.

use core::mem::size_of;

use alloc::boxed::Box;
use toyos_gicv3::packed_affinity;

use super::percpu::{self, PerCpu};
use super::{cache, control_regs, cpu, irqchip, paging, psci};
use crate::mm::DirectMap;
use crate::smp::{self, ROSTER};
use crate::time::AP_START;
use crate::{clock, log};

/// What an AP's entry reads with its MMU off, at the physical address `CPU_ON`
/// hands it in `x0`, and what its first Rust reads after.
#[repr(C)]
pub struct ApStart {
    /// The root it turns its MMU on under: [`paging::bringup_root`].
    pub(super) root: u64,
    /// The top of the stack it runs on until the idle loop, a kernel address.
    pub(super) stack_top: u64,
    percpu: &'static PerCpu,
    /// Its redistributor's physical frame, which the boot CPU mapped.
    redistributor: u64,
    token: u32,
}

/// Start every other CPU `gic` names, in the MADT's order, until one does not
/// echo within [`AP_START`] or the roster is full.
pub fn start(gic: &irqchip::Gic, psci: Option<psci::Conduit>) {
    let me = cpu::hardware_id();
    ROSTER.set_bsp(me);
    let others = gic.cpus.iter().filter(|gicc| packed_affinity(gicc.mpidr) != me);
    let Some(psci) = psci else {
        log!("SMP: no PSCI to start the other {} CPUs with; the boot CPU runs alone", others.count());
        control_regs::report(smp::cpu_count());
        return;
    };
    let root = paging::bringup_root();
    let entry = DirectMap::phys_of(super::boot::ap_start as *const u8);
    for gicc in others {
        let Some(attempt) = ROSTER.begin_attempt() else {
            log!("SMP: roster full at {} CPUs; ignoring further MADT entries", smp::cpu_count());
            break;
        };
        let start: &'static ApStart = Box::leak(Box::new(ApStart {
            root,
            stack_top: smp::bringup_stack(),
            percpu: percpu::alloc(attempt.id()),
            redistributor: gic.redistributor(packed_affinity(gicc.mpidr)),
            token: attempt.token(),
        }));
        // Read with the MMU off, so from memory and never from this CPU's cache.
        cache::write_back(start as *const ApStart as u64, size_of::<ApStart>());
        let context = DirectMap::phys_of(start as *const ApStart);
        if !smp::skip_startup(attempt.id()) {
            if let Err(refused) = psci.cpu_on(gicc.mpidr, entry, context) {
                log!("SMP: CPU_ON refused cpu{} mpidr={:#x} ({refused:?}); the rest stay off", attempt.id(), gicc.mpidr);
                break;
            }
        }
        let deadline = clock::nanos_since_boot() + AP_START.nanos();
        // Committed only on this attempt's own token, so `0..cpu_count()` stays dense.
        if !ROSTER.await_echo(attempt, || clock::nanos_since_boot() >= deadline) {
            log!("SMP: cpu{} mpidr={:#x} did not echo within {AP_START}; the rest stay off", attempt.id(), gicc.mpidr);
            break;
        }
        ROSTER.commit(attempt, packed_affinity(gicc.mpidr));
        log!("SMP: cpu{} mpidr={:#x} online", attempt.id(), gicc.mpidr);
    }
    let online = smp::cpu_count();
    log!("SMP: {online} of {} MADT CPUs online", gic.cpus.len());
    control_regs::report(online);
}

/// An AP's first Rust: at EL1, on its bring-up stack with the vectors
/// installed, under [`paging::bringup_root`], entered at `el`.
pub(super) extern "C" fn ap_entry(start: &'static ApStart, el: u64) -> ! {
    // First: every log line, a fault's report among them, reads it.
    percpu::install(start.percpu);
    paging::join();
    paging::check_joined();
    control_regs::check(el);
    irqchip::init_cpu(start.redistributor);
    ROSTER.echo(start.token);
    crate::process::ap_idle();
}
